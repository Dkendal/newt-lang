//! A CLI panic hook that turns internal `panic!`s carrying a debug-dumped AST
//! node into a source-highlighted diagnostic.
//!
//! Many compiler-internal invariants bail with a `{node:#?}` dump, e.g.
//! `panic!("Expected extends operator, found {condition:#?}")`. The raw dump is
//! large and near-impossible to relate back to the program the user wrote.
//! Because every AST node embeds a `Span { start, end }`, this hook scans the
//! panic message for those spans, takes the widest enclosing range, and renders
//! that region of the original `.nt` source with an underline (reusing the
//! same diagnostic renderer used for parse errors). When no span can be
//! recovered it defers to the default panic hook, so nothing is lost.

use crate::ast::Span;
use std::panic::PanicHookInfo;

/// Install a panic hook that renders a source-highlighted report for `source`
/// (named `source_name`) whenever a panic message embeds AST `Span`s. Any panic
/// without a recoverable span is handed to the previous (default) hook.
pub fn install_hook(source_name: String, source: String) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !report(info, &source_name, &source) {
            previous(info);
        }
    }));
}

/// Render the report; returns `false` (so the caller falls back to the default
/// hook) when the payload isn't a string or carries no AST span.
fn report(info: &PanicHookInfo, source_name: &str, source: &str) -> bool {
    let Some(message) = payload_message(info) else {
        return false;
    };
    let Some(span) = enclosing_span(message, source.len()) else {
        return false;
    };

    // Drop the multi-line `{node:#?}` dump: the first line is a usable headline
    // (e.g. "Expected extends operator, found ApplyGeneric(").
    let headline = message.lines().next().unwrap_or(message).trim_end();
    let location = info
        .location()
        .map(|l| format!(" at {}:{}", l.file(), l.line()))
        .unwrap_or_default();

    let error = crate::report::render_to_string(
        source_name,
        source,
        span,
        &format!("internal compiler panic{location}: {headline}"),
    );
    eprintln!("{error}");
    true
}

/// The panic payload as a string slice, covering both `&str` and `String`
/// payloads produced by the `panic!` macro.
fn payload_message<'a>(info: &'a PanicHookInfo<'a>) -> Option<&'a str> {
    let payload = info.payload();
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
}

/// Scan `message` for every `Span { start, end }` (compact or pretty-printed)
/// and return the span enclosing them all, clamped to `source_len` so a stale
/// or placeholder span can't index out of bounds. `None` when no span is found.
fn enclosing_span(message: &str, source_len: usize) -> Option<Span> {
    const MARKER: &str = "Span {";

    let mut start_min = usize::MAX;
    let mut end_max = 0usize;
    let mut found = false;

    for (idx, _) in message.match_indices(MARKER) {
        let rest = &message[idx + MARKER.len()..];
        if let (Some(start), Some(end)) = (field(rest, "start:"), field(rest, "end:")) {
            start_min = start_min.min(start);
            end_max = end_max.max(end);
            found = true;
        }
    }

    if !found {
        return None;
    }

    let start = start_min.min(source_len);
    let end = end_max.min(source_len).max(start);
    Some(Span::new(start, end))
}

/// Parse the unsigned integer following `key` in `hay` (skipping the whitespace
/// that pretty-printing inserts). Returns the first such value, which — because
/// each `Span {` marker is scanned from its own offset — is that span's field.
fn field(hay: &str, key: &str) -> Option<usize> {
    let after = &hay[hay.find(key)? + key.len()..];
    let digits: String = after
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_widest_span_from_pretty_dump() {
        let message = "Expected extends operator, found ApplyGeneric(\n    ApplyGeneric {\n        span: Span {\n            start: 10,\n            end: 40,\n        },\n        args: [\n            Ident {\n                span: Span {\n                    start: 12,\n                    end: 17,\n                },\n            },\n        ],\n    },\n)";
        let span = enclosing_span(message, 100).unwrap();
        assert_eq!((span.start, span.end), (10, 40));
    }

    #[test]
    fn recovers_span_from_compact_dump() {
        let span = enclosing_span("boom Span { start: 3, end: 8 }", 100).unwrap();
        assert_eq!((span.start, span.end), (3, 8));
    }

    #[test]
    fn clamps_to_source_length() {
        let span = enclosing_span("Span { start: 90, end: 999 }", 100).unwrap();
        assert_eq!((span.start, span.end), (90, 100));
    }

    #[test]
    fn none_when_no_span_present() {
        assert!(enclosing_span("plain panic message", 100).is_none());
    }
}
