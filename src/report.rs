//! Rendering of source-anchored diagnostics via [`ariadne`].
//!
//! Every pretty error in the codebase funnels through these helpers: parse
//! errors from the chumsky parser, static-validation diagnostics, assertion
//! failures from the `unittest` harness, and the panic hook's recovered spans.
//! A diagnostic is just a [`Span`] (byte offsets into the source) plus a
//! message; the helpers pair it with the source text and name and produce the
//! familiar underlined source excerpt.

use crate::ast::Span;
use ariadne::{Config, IndexType, Label, Report, ReportKind, Source};

/// The ariadne span type used by every report in this codebase: the source
/// name paired with a byte range into that source.
pub type ReportSpan = (String, std::ops::Range<usize>);

/// Render a built [`Report`] against its source to a trimmed `String`.
pub fn report_to_string(
    report: &Report<'_, ReportSpan>,
    source_name: &str,
    source: &str,
) -> String {
    let mut buf: Vec<u8> = Vec::new();
    report
        .write((source_name.to_string(), Source::from(source)), &mut buf)
        .expect("writing an ariadne report to an in-memory buffer cannot fail");

    let rendered = String::from_utf8_lossy(&buf).into_owned();
    rendered.trim_end().to_string()
}

/// Render a diagnostic to a `String` **without color**.
pub fn render_to_string(source_name: &str, source: &str, span: Span, message: &str) -> String {
    render(source_name, source, span, message, false)
}

/// Render a diagnostic straight to stderr, with color.
pub fn eprint(source_name: &str, source: &str, span: Span, message: &str) {
    eprintln!("{}", render(source_name, source, span, message, true));
}

/// Severity of a rendered diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

impl Severity {
    fn kind(self) -> ReportKind<'static> {
        match self {
            Severity::Warning => ReportKind::Warning,
            Severity::Error => ReportKind::Error,
        }
    }
}

/// Render a diagnostic with one message and any number of labeled spans (one
/// per use site). The first label's span anchors the report header.
pub fn render_labeled(
    severity: Severity,
    source_name: &str,
    source: &str,
    message: &str,
    labels: &[(Span, String)],
    color: bool,
) -> String {
    let primary = labels
        .first()
        .map(|(span, _)| clamp(*span, source.len()))
        .unwrap_or(0..0);

    let mut report = Report::build(severity.kind(), (source_name.to_string(), primary))
        .with_config(
            Config::new()
                .with_index_type(IndexType::Byte)
                .with_color(color),
        )
        .with_message(message);

    for (span, label) in labels {
        let range = clamp(*span, source.len());
        report =
            report.with_label(Label::new((source_name.to_string(), range)).with_message(label));
    }

    report_to_string(&report.finish(), source_name, source)
}

/// Render a `dbg!` report: a `Debug`-kind diagnostic anchored at `span`, whose
/// label carries the evaluated type (`= <type>`).
pub fn render_debug(
    source_name: &str,
    source: &str,
    span: Span,
    message: &str,
    color: bool,
) -> String {
    let range = clamp(span, source.len());

    let report = Report::build(
        ReportKind::Custom("Debug", ariadne::Color::Cyan),
        (source_name.to_string(), range.clone()),
    )
    .with_config(
        Config::new()
            .with_index_type(IndexType::Byte)
            .with_color(color),
    )
    .with_label(Label::new((source_name.to_string(), range)).with_message(message))
    .finish();

    report_to_string(&report, source_name, source)
}

fn render(source_name: &str, source: &str, span: Span, message: &str, color: bool) -> String {
    report_to_string(
        &build_report(source_name, source, span, message, color),
        source_name,
        source,
    )
}

fn build_report(
    source_name: &str,
    source: &str,
    span: Span,
    message: &str,
    color: bool,
) -> Report<'static, ReportSpan> {
    let range = clamp(span, source.len());

    Report::build(ReportKind::Error, (source_name.to_string(), range.clone()))
        .with_config(
            Config::new()
                .with_index_type(IndexType::Byte)
                .with_color(color),
        )
        .with_message(message)
        .with_label(Label::new((source_name.to_string(), range)).with_message(message))
        .finish()
}

/// Clamp a span to the source length (a synthesized or stale span must not
/// index out of bounds) and to valid char boundaries, and make sure it is not
/// backwards (ariadne panics on `start > end`).
fn clamp(span: Span, len: usize) -> std::ops::Range<usize> {
    let start = span.start.min(len);
    let end = span.end.min(len).max(start);
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_name_line_and_column() {
        let source = "type A as 1\ntype B as oops\n";
        let span = Span::new(
            source.find("oops").unwrap(),
            source.find("oops").unwrap() + 4,
        );
        let out = render_to_string("test.nt", source, span, "unknown type");
        assert!(out.contains("unknown type"), "{out}");
        assert!(out.contains("test.nt:2:11"), "{out}");
        assert!(out.contains("oops"), "{out}");
        // No ANSI escapes in the deterministic form.
        assert!(!out.contains('\x1b'), "{out}");
    }

    #[test]
    fn clamps_out_of_range_spans() {
        let out = render_to_string("x.nt", "short", Span::new(100, 200), "boom");
        assert!(out.contains("boom"), "{out}");
    }

    #[test]
    fn clamps_backwards_spans() {
        let out = render_to_string("x.nt", "abcdef", Span::new(4, 2), "boom");
        assert!(out.contains("boom"), "{out}");
    }

    #[test]
    fn renders_warning_with_multiple_labels() {
        let source = "type A as Foo\ntype B as Foo\n";
        let first = source.find("Foo").unwrap();
        let second = source.rfind("Foo").unwrap();
        let labels = vec![
            (
                Span::new(first, first + 3),
                "cannot be resolved to a definition".to_string(),
            ),
            (
                Span::new(second, second + 3),
                "cannot be resolved to a definition".to_string(),
            ),
        ];
        let out = render_labeled(
            Severity::Warning,
            "test.nt",
            source,
            "cannot resolve type `Foo`",
            &labels,
            false,
        );
        assert!(out.contains("Warning"), "{out}");
        assert!(out.contains("cannot resolve type `Foo`"), "{out}");
        assert!(out.contains("test.nt:1:11"), "{out}");
        // Both use sites are labeled.
        assert_eq!(
            out.matches("cannot be resolved to a definition").count(),
            2,
            "{out}"
        );
        assert!(!out.contains('\x1b'), "{out}");
    }

    #[test]
    fn renders_error_severity() {
        let source = "type A as Foo\n";
        let at = source.find("Foo").unwrap();
        let labels = vec![(Span::new(at, at + 3), "boom".to_string())];
        let out = render_labeled(Severity::Error, "x.nt", source, "bad", &labels, false);
        assert!(out.contains("Error"), "{out}");
    }

    #[test]
    fn renders_debug_kind() {
        let source = "type A as 1\n";
        let at = source.find('1').unwrap();
        let out = render_debug("x.nt", source, Span::new(at, at + 1), "= 1", false);
        assert!(out.contains("Debug"), "{out}");
        assert!(out.contains("= 1"), "{out}");
        assert!(out.contains("x.nt:1:11"), "{out}");
        assert!(!out.contains('\x1b'), "{out}");
    }
}
