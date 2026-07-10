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
//! A fixture file has four required sections, each separated by a line of three
//! or more `=` characters:
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
//! <expected stderr: the CLI's diagnostics/assertion report, or empty>
//! ```
//!
//! Only the first *three* separators split the file; any further `===` lines are
//! kept verbatim as part of the final section, so output that legitimately
//! contains such a line is preserved. The fourth (stderr) section is
//! **required** — every runner asserts that processing the source produces
//! exactly it on stderr (see [`cli_stderr`]). It is frequently empty (a valid
//! program with no `unittest`s writes nothing), but the section itself must
//! still be present. Each section is dedented (common leading indentation
//! stripped) and trimmed, mirroring the inline `assert_typescript!` /
//! `assert_expr_eq!` macros so fixtures and inline tests stay consistent.

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
    /// The required fourth section: the exact stderr the CLI produces for
    /// `source` — validation diagnostics or the `unittest` assertion report —
    /// dedented and trimmed. Empty (but always present) when the program is
    /// valid and declares no assertions.
    pub stderr: String,
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

/// Splits a fixture file's contents into its four sections: name, source,
/// expected, and stderr. Only the first three separator lines split the file;
/// later `===` lines stay in the final (stderr) section.
///
/// # Panics
///
/// Panics with a descriptive message if the file contains fewer than three
/// separator lines (i.e. fewer than four sections) — the stderr section is
/// required, even when empty.
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
        sections.len() >= 4,
        "expected a corpus fixture with at least 3 `===` separator lines \
         (name, source, expected, stderr), found {} section(s); every fixture \
         must include a stderr section — leave it empty if the program writes \
         nothing to stderr",
        sections.len()
    );

    Case {
        name: sections[0].trim().to_owned(),
        source: sections[1].trim().to_owned(),
        expected: normalize_block(&sections[2]),
        stderr: normalize_block(&sections[3]),
    }
}

/// Trims each line's trailing whitespace and the surrounding blank lines.
///
/// Corpus comparisons are otherwise exact, but ariadne renders caret/underline
/// lines with trailing spaces that hand-edited fixtures — and editors that strip
/// trailing whitespace on save — can't reliably preserve. Normalizing trailing
/// whitespace on *both* sides keeps the comparison meaningful without depending
/// on invisible characters. Leading indentation, which is significant for the
/// `  ok`/`  FAILED` assertion-report lines, is preserved.
pub fn normalize_block(s: &str) -> String {
    s.lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
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

/// Reads the fixture at `path`, renders its source with `rule`, and asserts both
/// that the output matches the expected (TypeScript) section *and* that the
/// program's stderr matches the fixture's stderr section. Intended to be called
/// from a generated `#[test]` function, so a mismatch panics with a readable
/// diff.
pub fn run_case(rule: Rule, path: &Path) {
    let case = load_case(path);
    let actual = render(rule, &case.source);

    pretty_assertions::assert_eq!(
        case.expected,
        normalize_block(&actual),
        "rendered TypeScript mismatch in fixture {}",
        path.display()
    );
    pretty_assertions::assert_eq!(
        case.stderr,
        normalize_block(&cli_stderr(rule, &case.source)),
        "stderr mismatch in fixture {}",
        path.display()
    );
}

/// Parses `source` with `rule`, runs static validation, and renders the
/// resulting diagnostics exactly as the CLI writes them to stderr (reported
/// under the `<stdin>` filename), one per diagnostic. Returns the empty string
/// when the source is valid.
pub fn render_diagnostics(rule: Rule, source: &str) -> String {
    parse_source(rule, source)
        .validate("<stdin>", source)
        .iter()
        .map(|report| crate::report::report_to_string(report, "<stdin>", source))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The exact bytes the CLI writes to stderr for `source` parsed via `rule`,
/// reported under the `<stdin>` filename: the validation diagnostics if the
/// program is invalid (the CLI stops there), otherwise the `unittest` assertion
/// report. Empty when the program is valid and declares no assertions. This is
/// the runner-side mirror of `src/main.rs`'s stderr behavior, used by every
/// corpus runner to assert against a fixture's stderr section.
pub fn cli_stderr(rule: Rule, source: &str) -> String {
    let diagnostics = render_diagnostics(rule, source);
    if !diagnostics.is_empty() {
        return diagnostics;
    }

    let program = parse_source(rule, source).simplify();
    let mut out = Vec::new();
    crate::test_harness::run(
        &program,
        source,
        "<stdin>",
        crate::test_harness::Config::default(),
        &crate::ast::dbg_expr::DbgWatches::default(),
        &mut out,
    )
    .expect("writing the assertion report to an in-memory buffer cannot fail");
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads the fixture at `path`, asserts its source's stderr matches the
/// fixture's stderr section, and — for a well-formed source — asserts that its
/// two newtype snippets simplify to the same AST (compared via their
/// s-expression form, exactly as the inline `assert_expr_eq!` macro does).
/// Intended to be called from a generated `#[test]` function.
///
/// When the source is intentionally malformed (its stderr section carries a
/// validation diagnostic) there is no simplified form to compare, so the
/// equivalence check is skipped and the stdout section is expected to be empty.
pub fn run_equivalence_case(rule: Rule, path: &Path) {
    let case = load_case(path);

    pretty_assertions::assert_eq!(
        case.stderr,
        normalize_block(&cli_stderr(rule, &case.source)),
        "stderr mismatch in fixture {}",
        path.display()
    );

    // A source that fails static validation can't be simplified (`simplify`
    // would panic on the malformed construct the CLI rejects), so the
    // equivalence comparison is meaningful only for well-formed snippets.
    let source_ast = parse_source(rule, &case.source);
    if !source_ast.validate("<stdin>", &case.source).is_empty() {
        return;
    }

    let lhs = source_ast.simplify();
    let rhs = parse_source(rule, &case.expected).simplify();

    pretty_assertions::assert_eq!(
        lhs.to_sexp().unwrap(),
        rhs.to_sexp().unwrap(),
        "the two snippets are not equivalent after simplification in fixture {}",
        path.display()
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
/// assertion report differs from the stderr section, if the fixture declares no
/// assertions, or if any assertion fails (the report is included in the
/// message).
pub fn run_assertion_case(rule: Rule, path: &Path) {
    let case = load_case(path);

    let program = parse_source(rule, &case.source).simplify();

    // Rendering: the `unittest` blocks vanish; everything else renders normally.
    let rendered = program.render_pretty_ts(RENDER_WIDTH);
    pretty_assertions::assert_eq!(
        case.expected,
        normalize_block(&rendered),
        "rendered TypeScript mismatch in fixture {}",
        path.display()
    );

    // Assertions: every `assert` in the program must hold, and the report the
    // harness writes to stderr must match the fixture's stderr section.
    let mut log = Vec::new();
    let report = crate::test_harness::run(
        &program,
        &case.source,
        "<stdin>",
        crate::test_harness::Config::default(),
        &crate::ast::dbg_expr::DbgWatches::default(),
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

    pretty_assertions::assert_eq!(
        case.stderr,
        normalize_block(&log),
        "stderr mismatch in fixture {}",
        path.display()
    );
}

fn read_fixture(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e))
}

/// Reads and parses the fixture at `path`, attributing a malformed-structure
/// failure (too few sections, e.g. a missing stderr section) to the offending
/// `.txt` file rather than to the bare contents.
fn load_case(path: &Path) -> Case {
    let contents = read_fixture(path);
    let separators = contents.lines().filter(|l| is_separator(l)).count();
    assert!(
        separators >= 3,
        "corpus fixture {} is malformed: found {} `===` separator line(s), need at \
         least 3 (name, source, expected, stderr) — every fixture must include a \
         stderr section, left empty when the program writes nothing to stderr",
        path.display(),
        separators
    );
    parse_fixture(&contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_four_sections() {
        let case = parse_fixture(
            "Name\n\n=======\n\ntype A do 1 end\n\n=======\n\ntype A = 1;\n\n=======\n",
        );
        assert_eq!(case.name, "Name");
        assert_eq!(case.source, "type A do 1 end");
        assert_eq!(case.expected, "type A = 1;");
        assert_eq!(case.stderr, "");
    }

    #[test]
    fn captures_stderr_section() {
        let case = parse_fixture("Name\n=======\nsrc\n=======\n\n=======\nboom\n");
        assert_eq!(case.source, "src");
        assert_eq!(case.expected, "");
        assert_eq!(case.stderr, "boom");
    }

    #[test]
    fn blank_stderr_section_is_empty() {
        let case = parse_fixture("Name\n=======\nsrc\n=======\nout\n=======\n\n");
        assert_eq!(case.stderr, "");
    }

    #[test]
    #[should_panic(expected = "at least 3 `===` separator")]
    fn rejects_missing_stderr_section() {
        // Three sections (name, source, expected) but no stderr section.
        parse_fixture("Name\n\n=======\n\ntype A do 1 end\n\n=======\n\ntype A = 1;\n");
    }

    #[test]
    fn separator_requires_three_equals() {
        assert!(is_separator("==="));
        assert!(is_separator("  ======= "));
        assert!(!is_separator("=="));
        assert!(!is_separator("= = ="));
        assert!(!is_separator("type A = 1"));
    }
}
