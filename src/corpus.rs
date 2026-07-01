//! Runtime support for the *corpus* tests.
//!
//! Two corpora share this machinery, both asserting on rendered output rather
//! than a parse tree:
//!
//! * **TypeScript corpus** (`tests/corpus/typescript`, [`run_case`]): assert that
//!   newtype source renders to the expected TypeScript.
//! * **Equivalence corpus** (`tests/corpus/newtype`, [`run_equivalence_case`]):
//!   assert that two newtype snippets simplify to the same AST (the corpus form
//!   of the inline `assert_expr_eq!` macro).
//!
//! The list of test functions is generated at compile time by the
//! `typescript_tests` / `equivalent_tests` attribute macros in
//! `newtype_macros_lib`; each generated function calls one of the runners below
//! at run time, so editing a fixture's *body* does not require a recompile.
//!
//! ## A note on adding/removing fixtures
//!
//! Adding or removing a fixture *file* changes the set of generated tests, which
//! only happens when the proc-macro re-runs. Cargo will not notice a new or
//! deleted fixture on its own, so the crate ships a `build.rs` that emits
//! `cargo:rerun-if-changed=tests/corpus` to force a rebuild when the corpus
//! directory changes. (Editing an existing fixture's contents needs no rebuild —
//! the runner reads the file at run time.)
//!
//! # Fixture format
//!
//! A fixture file has three required sections and one optional fourth, each
//! separated by a line of three or more `=` characters:
//!
//! ```text
//! Name of the test
//!
//! =======
//!
//! <source snippet (stdin)>
//!
//! =======
//!
//! <expected output (stdout): TypeScript, or the equivalent newtype snippet>
//!
//! =======
//!
//! <expected stderr: e.g. a validation diagnostic>   (optional)
//! ```
//!
//! Only the first *three* separators split the file; any further `===` lines are
//! kept verbatim as part of the final section, so output that legitimately
//! contains such a line is preserved. The fourth (stderr) section is optional:
//! when present, a runner may assert that processing the source produces it on
//! stderr (see [`run_equivalence_case`]). Each section is dedented (common
//! leading indentation stripped) and trimmed, mirroring the inline
//! `assert_typescript!` / `assert_expr_eq!` macros so fixtures and inline tests
//! stay consistent.

use crate::ast::Ast;
use crate::parser::{self, Rule};
use crate::typescript::Pretty;
use itertools::Itertools;
use std::path::Path;

/// Width passed to the pretty-printer. Matches the value used by the inline
/// `assert_typescript!` tests so fixtures and inline tests stay consistent.
const RENDER_WIDTH: usize = 80;

/// A parsed corpus fixture: a human-readable name and the two snippets. For the
/// TypeScript corpus, `expected` is rendered TypeScript; for the equivalence
/// corpus, it is a second newtype snippet that must simplify to the same AST as
/// `source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    pub name: String,
    pub source: String,
    pub expected: String,
    /// The optional fourth section: expected stderr (e.g. a validation
    /// diagnostic). `None` when the fixture has no stderr section, or it is
    /// blank after trimming.
    pub stderr: Option<String>,
}

/// Returns `true` for a separator line: trimmed, non-empty, and made up solely
/// of `=` characters (at least three).
fn is_separator(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 3 && line.chars().all(|c| c == '=')
}

/// Strips the common leading whitespace shared by every non-blank line, then
/// trims surrounding blank lines — the run-time equivalent of the `dedent!`
/// applied by the inline test macros.
fn dedent_trim(section: &str) -> String {
    let indent = section
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);

    let dedented: Vec<&str> = section
        .lines()
        .map(|l| if l.len() >= indent { &l[indent..] } else { l })
        .collect();

    dedented.join("\n").trim().to_string()
}

/// Splits a fixture file's contents into its sections: name, source, expected,
/// and an optional stderr. Only the first three separator lines split the file;
/// later `===` lines stay in the final section.
///
/// # Panics
///
/// Panics with a descriptive message if the file contains fewer than two
/// separator lines (i.e. fewer than three sections).
pub fn parse_fixture(contents: &str) -> Case {
    let mut sections: Vec<String> = vec![String::new()];
    for line in contents.lines() {
        if sections.len() < 4 && is_separator(line) {
            sections.push(String::new());
        } else {
            let current = sections.last_mut().unwrap();
            current.push_str(line);
            current.push('\n');
        }
    }

    assert!(
        sections.len() >= 3,
        "expected a corpus fixture with at least 2 `===` separator lines \
         (name, source, expected), found {} section(s)",
        sections.len()
    );

    // The optional stderr section is dropped when blank after trimming, so a
    // trailing separator with no content asserts nothing.
    let stderr = sections
        .get(3)
        .map(|s| dedent_trim(s))
        .filter(|s| !s.is_empty());

    Case {
        name: dedent_trim(&sections[0]),
        source: dedent_trim(&sections[1]),
        expected: dedent_trim(&sections[2]),
        stderr,
    }
}

/// Parses `source` starting from `rule`. Panics with the rendered parse errors
/// on a failure (full-input consumption is inherent to the parser's entry
/// points, so trailing garbage is a parse error).
pub fn parse_source(rule: Rule, source: &str) -> Ast {
    parser::parse_source(rule, source).unwrap_or_else(|errors| {
        let rendered = errors
            .iter()
            .map(|e| e.render("<stdin>", source))
            .join("\n");
        panic!("failed to parse as {:?}:\n{}", rule, rendered)
    })
}

/// Parses and simplifies `source`, then renders it to TypeScript.
pub fn render(rule: Rule, source: &str) -> String {
    parse_source(rule, source)
        .simplify()
        .render_pretty_ts(RENDER_WIDTH)
}

/// Reads the fixture at `path`, renders its source with `rule`, and asserts the
/// output matches the expected (TypeScript) section. Intended to be called from
/// a generated `#[test]` function, so a mismatch panics with a readable diff.
pub fn run_case(rule: Rule, path: &Path) {
    let contents = read_fixture(path);
    let case = parse_fixture(&contents);
    let actual = render(rule, &case.source);

    pretty_assertions::assert_eq!(case.expected, actual.trim());
}

/// Parses `source` with `rule`, runs static validation, and renders the
/// resulting diagnostics exactly as the CLI writes them to stderr (reported
/// under the `<stdin>` filename), one per diagnostic. Returns the empty string
/// when the source is valid.
pub fn render_diagnostics(rule: Rule, source: &str) -> String {
    parse_source(rule, source)
        .validate()
        .iter()
        .map(|d| d.to_report_string("<stdin>", source))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reads the fixture at `path` and asserts that its two newtype snippets
/// simplify to the same AST (compared via their s-expression form, exactly as
/// the inline `assert_expr_eq!` macro does). Intended to be called from a
/// generated `#[test]` function.
///
/// When the fixture carries a fourth (stderr) section it is instead treated as
/// a *diagnostic* fixture: the source must produce exactly those validation
/// diagnostics on stderr, and the equivalence check is skipped (the stdout
/// section is expected to be empty).
pub fn run_equivalence_case(rule: Rule, path: &Path) {
    let contents = read_fixture(path);
    let case = parse_fixture(&contents);

    if let Some(expected_stderr) = &case.stderr {
        let actual = render_diagnostics(rule, &case.source);
        pretty_assertions::assert_eq!(*expected_stderr, actual.trim());
        return;
    }

    let lhs = parse_source(rule, &case.source).simplify();
    let rhs = parse_source(rule, &case.expected).simplify();

    pretty_assertions::assert_eq!(
        lhs.to_sexp().unwrap(),
        rhs.to_sexp().unwrap(),
        "the two snippets are not equivalent after simplification"
    );
}

/// Reads the fixture at `path` and runs it end-to-end: its source must render to
/// the expected TypeScript (the `unittest` blocks emit nothing), *and* every
/// `assert` in its `unittest`s must hold. This exercises the full pipeline —
/// parsing, simplification, top-level type resolution, assignability, and
/// rendering — so a fixture doubles as living documentation. Intended to be
/// called from a generated `#[test]` function.
///
/// # Panics
///
/// Panics if the rendered output differs from the expected section, if the
/// fixture declares no assertions, or if any assertion fails (the report is
/// included in the message).
pub fn run_assertion_case(rule: Rule, path: &Path) {
    let contents = read_fixture(path);
    let case = parse_fixture(&contents);

    let program = parse_source(rule, &case.source).simplify();

    // Rendering: the `unittest` blocks vanish; everything else renders normally.
    let rendered = program.render_pretty_ts(RENDER_WIDTH);
    pretty_assertions::assert_eq!(case.expected, rendered.trim());

    // Assertions: every `assert` in the program must hold.
    let mut log = Vec::new();
    let report = crate::test_harness::run(
        &program,
        &case.source,
        "<stdin>",
        crate::test_harness::Config { fail_fast: false },
        &mut log,
    )
    .expect("writing the assertion report to an in-memory buffer cannot fail");

    let log = String::from_utf8_lossy(&log);
    assert!(
        report.passed > 0,
        "fixture {} ran no assertions; an assertion fixture must contain at least one \
         `assert` inside a `unittest`:\n{}",
        path.display(),
        log
    );
    assert!(
        !report.has_failures(),
        "fixture {} has failing assertions:\n{}",
        path.display(),
        log
    );
}

fn read_fixture(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_sections() {
        let case = parse_fixture("Name\n\n=======\n\ntype A as 1\n\n=======\n\ntype A = 1;\n");
        assert_eq!(case.name, "Name");
        assert_eq!(case.source, "type A as 1");
        assert_eq!(case.expected, "type A = 1;");
        assert_eq!(case.stderr, None);
    }

    #[test]
    fn captures_optional_stderr_section() {
        let case = parse_fixture("Name\n=======\nsrc\n=======\n\n=======\nboom\n");
        assert_eq!(case.source, "src");
        assert_eq!(case.expected, "");
        assert_eq!(case.stderr.as_deref(), Some("boom"));
    }

    #[test]
    fn blank_stderr_section_is_none() {
        let case = parse_fixture("Name\n=======\nsrc\n=======\nout\n=======\n\n");
        assert_eq!(case.stderr, None);
    }

    #[test]
    #[should_panic(expected = "at least 2 `===` separator")]
    fn rejects_missing_section() {
        parse_fixture("Name\n\n=======\n\ntype A as 1\n");
    }

    #[test]
    fn separator_requires_three_equals() {
        assert!(is_separator("==="));
        assert!(is_separator("  ======= "));
        assert!(!is_separator("=="));
        assert!(!is_separator("= = ="));
        assert!(!is_separator("type A = 1"));
    }

    #[test]
    fn extra_separators_stay_in_stderr_section() {
        // Only the first three `===` lines split; a fourth stays in the final
        // (stderr) section, so stderr output containing `===` is preserved.
        let case =
            parse_fixture("Name\n=======\nsrc\n=======\nout\n=======\nline1\n=======\nline2\n");
        assert_eq!(case.source, "src");
        assert_eq!(case.expected, "out");
        assert_eq!(case.stderr.as_deref(), Some("line1\n=======\nline2"));
    }

    #[test]
    fn dedents_common_indentation() {
        let case =
            parse_fixture("Name\n=======\n    if a then\n        b\n    end\n=======\n    x\n");
        assert_eq!(case.source, "if a then\n    b\nend");
        assert_eq!(case.expected, "x");
    }
}
