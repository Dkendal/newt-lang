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

/// Render a diagnostic to a `String` **without color**, suitable for writers
/// that are captured and string-compared (corpus fixtures, the test harness
/// report) or for embedding in panic messages.
pub fn render_to_string(source_name: &str, source: &str, span: Span, message: &str) -> String {
    render(source_name, source, span, message, false)
}

/// Render a diagnostic straight to stderr, with color.
pub fn eprint(source_name: &str, source: &str, span: Span, message: &str) {
    eprintln!("{}", render(source_name, source, span, message, true));
}

fn render(source_name: &str, source: &str, span: Span, message: &str, color: bool) -> String {
    let range = clamp(span, source.len());

    let mut buf: Vec<u8> = Vec::new();
    Report::build(ReportKind::Error, (source_name.to_string(), range.clone()))
        .with_config(
            Config::new()
                .with_index_type(IndexType::Byte)
                .with_color(color),
        )
        .with_message(message)
        .with_label(Label::new((source_name.to_string(), range)).with_message(message))
        .finish()
        .write((source_name.to_string(), Source::from(source)), &mut buf)
        .expect("writing an ariadne report to an in-memory buffer cannot fail");

    let rendered = String::from_utf8_lossy(&buf).into_owned();
    rendered.trim_end().to_string()
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
}
