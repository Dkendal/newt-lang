//! Rendering of source-anchored diagnostics via [`ariadne`].
//!
//! Every pretty error in the codebase funnels through these helpers: parse
//! errors from the chumsky parser, static-validation diagnostics, assertion
//! failures from the `unittest` harness, and the panic hook's recovered spans.
//! A diagnostic is just a [`Span`] (byte offsets into the source) plus a
//! message; the helpers pair it with the source text and name and produce the
//! familiar underlined source excerpt.

use crate::ast::dbg_expr::Decision;
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

/// ANSI codes for the hand-formatted `[Trace]` line. ariadne cannot emit a
/// label-free one-liner, so the compact trace line is formatted here in
/// ariadne's visual style; explicit codes (rather than ariadne's
/// concolor-gated painting) keep output deterministic under the `color`
/// switch.
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_RESET: &str = "\x1b[0m";

/// The decision word and its colour for a conditional-trace line: which
/// branch the conditional took.
fn decision_word(decision: Decision) -> (&'static str, &'static str) {
    match decision {
        Decision::Then => ("then", "\x1b[32m"),
        Decision::Else => ("else", "\x1b[31m"),
        Decision::Never => ("never", "\x1b[35m"),
        Decision::Both => ("both (indeterminate)", "\x1b[33m"),
    }
}

/// Render one `--trace-eval` event as a compact source-anchored line:
/// `[Trace] file:line:col  message[ → decision]`. With `color` on, the kind
/// word is cyan and the decision word takes its branch colour.
pub fn render_trace_line(
    source_name: &str,
    source: &str,
    span: Span,
    message: &str,
    decision: Option<Decision>,
    color: bool,
) -> String {
    let (line, col) = line_col(source, span.start);
    let kind = if color {
        format!("{ANSI_CYAN}[Trace]{ANSI_RESET}")
    } else {
        "[Trace]".to_string()
    };
    let mut out = format!("{kind} {source_name}:{line}:{col}  {message}");
    if let Some(decision) = decision {
        let (word, code) = decision_word(decision);
        if color {
            out.push_str(&format!(" → {code}{word}{ANSI_RESET}"));
        } else {
            out.push_str(&format!(" → {word}"));
        }
    }
    out
}

/// The 1-based `(line, column)` of a byte `offset` into `source`, matching
/// ariadne's own numbering. Offsets past EOF clamp to the final position.
pub fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::dbg_expr::Decision;

    #[test]
    fn trace_line_plain_when_colour_off() {
        let out = render_trace_line(
            "x.nt",
            "type A as 1\n",
            Span::new(0, 4),
            "1 <: number",
            Some(Decision::Then),
            false,
        );
        assert_eq!(out, "[Trace] x.nt:1:1  1 <: number → then");
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn trace_line_colours_decision_word_when_colour_on() {
        for (decision, expected) in [
            (Decision::Then, "\x1b[32mthen\x1b[0m"),
            (Decision::Else, "\x1b[31melse\x1b[0m"),
            (Decision::Never, "\x1b[35mnever\x1b[0m"),
            (Decision::Both, "\x1b[33mboth (indeterminate)\x1b[0m"),
        ] {
            let out = render_trace_line(
                "x.nt",
                "type A as 1\n",
                Span::new(0, 4),
                "1 <: number",
                Some(decision),
                true,
            );
            assert!(out.contains(expected), "{out:?}");
            assert!(out.contains("\x1b[36m[Trace]\x1b[0m"), "{out:?}");
        }
    }

    #[test]
    fn trace_line_without_decision_has_no_arrow() {
        let out = render_trace_line(
            "x.nt",
            "type A as 1\ntype B as 2\n",
            Span::new(12, 16),
            "Id(1) = 1",
            None,
            false,
        );
        assert_eq!(out, "[Trace] x.nt:2:1  Id(1) = 1");
    }

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

    #[test]
    fn line_col_is_one_based_and_clamps() {
        let source = "ab\ncd\n";
        assert_eq!(line_col(source, 0), (1, 1)); // 'a'
        assert_eq!(line_col(source, 1), (1, 2)); // 'b'
        assert_eq!(line_col(source, 3), (2, 1)); // 'c' (after newline)
        assert_eq!(line_col(source, 4), (2, 2)); // 'd'
        assert_eq!(line_col(source, 100), (3, 1)); // past EOF clamps to end
    }
}
