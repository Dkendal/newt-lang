//! Compile-time evaluation of `unittest` assertion blocks.
//!
//! The harness runs after [`Ast::simplify`](crate::ast::Ast::simplify) and
//! before rendering. Each `assert <claim>` inside a `unittest` is evaluated with
//! the same semantics as an `if` condition: the claim is lowered to a
//! conditional and reduced by the runtime
//! ([`runtime::builtin::unquote`](crate::runtime::builtin::unquote)). A claim
//! that reduces to `true` passes; anything else (a definite `false`, an
//! indeterminate result, or a non-relational claim) fails.
//!
//! Top-level `type` aliases and `interface`s are resolved via a [`TypeEnv`], so
//! claims may reference them by name (`assert Foo <: number`), apply generics
//! (`assert Id(1) <: number`), and rely on interface inheritance. References the
//! environment can't resolve (e.g. imported types, or `any`) stay indeterminate.
//!
//! Results are written to a caller-supplied sink (stderr in the compiler). The
//! caller inspects [`Report::has_failures`] to decide the process exit code —
//! rendering still happens regardless, so the emitted TypeScript is always
//! produced.

use std::io::{self, Write};

use crate::ast::type_env::{ResolveCtx, TypeEnv};
use crate::ast::{Ast, ExtendsInfixOp, ExtendsPrefixOp, InfixOp, PrefixOp, Span};
use crate::extends_result::ExtendsResult;

/// Configuration for a harness run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Config {
    /// Stop at the first failing assertion instead of evaluating the rest.
    pub fail_fast: bool,
    /// Mirror TypeScript's `exactOptionalPropertyTypes`: an optional target
    /// property `x?: T` no longer accepts `T | undefined` sources.
    pub exact_optional_property_types: bool,
}

/// Summary of a harness run.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub passed: usize,
    pub failed: usize,
}

impl Report {
    /// Whether any assertion failed (used to pick the process exit code).
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.failed > 0
    }

    fn total(&self) -> usize {
        self.passed + self.failed
    }
}

/// The result of evaluating a single `assert` claim.
enum Outcome {
    Pass,
    /// The claim evaluated to something other than `true`: a definite `false`, a
    /// collapsed `never`, or an indeterminate result (an unresolved reference,
    /// which the assignability engine reports as `Both`).
    Fail {
        result: ExtendsResult,
    },
    /// The claim couldn't be evaluated: it wasn't a relational proposition, or it
    /// contained a generic application with the wrong arity. Each diagnostic
    /// carries its own source span.
    Errors(Vec<Diagnostic>),
}

/// A static error preventing evaluation, with the source span to point at.
struct Diagnostic {
    span: Span,
    message: String,
}

/// Evaluate every `unittest` in `program`, writing a report to `out`. Failure
/// excerpts are reported under `source_name`.
///
/// Returns once all assertions are evaluated, or — when [`Config::fail_fast`] is
/// set — as soon as one fails. Write errors on `out` are propagated.
pub fn run(
    program: &Ast,
    source: &str,
    source_name: &str,
    config: Config,
    out: &mut dyn Write,
) -> io::Result<Report> {
    let mut report = Report::default();

    // Symbol table of the program's top-level `type`s and `interface`s, so a
    // claim referencing them (`assert Foo <: number`) resolves to their shape.
    let env = TypeEnv::from_program(program);

    'outer: for unittest in collect_unittests(program) {
        writeln!(out, "unittest {}", unittest.name)?;

        for stmt in &unittest.body {
            let Ast::Assert(assert) = stmt else { continue };

            // Display from the `assert` statement's span (which includes any
            // grouping parentheses) rather than the claim node's span, which
            // excludes them — a parenthesized claim like `not (A <: B)` would
            // otherwise render with the closing `)` missing.
            let span = claim_span(source, assert.span);
            let claim_src = slice_or(source, span, "<claim>");

            match evaluate(&assert.claim, &env, config) {
                Outcome::Pass => {
                    report.passed += 1;
                    writeln!(out, "  ok      {claim_src}")?;
                }
                Outcome::Fail { result } => {
                    report.failed += 1;
                    let message = format!(
                        "assertion failed: expected the relation to hold (`true`), but it \
                        evaluated to {}",
                        describe(result)
                    );
                    writeln!(out, "  FAILED  {claim_src}")?;
                    writeln!(
                        out,
                        "{}",
                        crate::report::render_to_string(source_name, source, span, &message)
                    )?;
                    if config.fail_fast {
                        break 'outer;
                    }
                }
                Outcome::Errors(diagnostics) => {
                    report.failed += 1;
                    writeln!(out, "  FAILED  {claim_src}")?;
                    for diagnostic in diagnostics {
                        writeln!(
                            out,
                            "{}",
                            crate::report::render_to_string(
                                source_name,
                                source,
                                diagnostic.span,
                                &diagnostic.message
                            )
                        )?;
                    }
                    if config.fail_fast {
                        break 'outer;
                    }
                }
            }
        }
    }

    if report.total() > 0 {
        writeln!(
            out,
            "\n{} assertion(s): {} passed, {} failed",
            report.total(),
            report.passed,
            report.failed
        )?;
    }

    Ok(report)
}

/// Evaluate one claim to a pass/fail/invalid outcome.
///
/// The claim is simplified (resolving `let`s, applications, etc. in its
/// operands) and then reduced over the [`ExtendsResult`] algebra, matching the
/// desugaring an `if` condition uses: `==`/`!=` become mutual assignability,
/// `and`/`or` fold, and `not` swaps the relation. Only a definite `true` passes.
fn evaluate(claim: &Ast, env: &TypeEnv, config: Config) -> Outcome {
    match claim {
        Ast::TrueKeyword(_) => return Outcome::Pass,
        Ast::FalseKeyword(_) => {
            return Outcome::Fail {
                result: ExtendsResult::False,
            }
        }
        Ast::ExtendsInfixOp(_) | Ast::ExtendsPrefixOp(_) => {}
        _ => {
            return Outcome::Errors(vec![Diagnostic {
                span: claim.as_span(),
                message: "the claim must be a relational expression such as `A <: B`, \
                    `A == B`, or `not (A <: B)`"
                    .to_string(),
            }])
        }
    }

    // Reject generic applications with the wrong arity before evaluating, so a
    // mismatch is a clear error rather than a silently-wrong result.
    let arity_errors = env.arity_errors(claim);
    if !arity_errors.is_empty() {
        return Outcome::Errors(
            arity_errors
                .into_iter()
                .map(|error| Diagnostic {
                    span: error.span,
                    message: error.message,
                })
                .collect(),
        );
    }

    let ctx = ResolveCtx::new(env)
        .with_exact_optional_property_types(config.exact_optional_property_types);

    // A generic application whose argument violates a `where` constraint is an
    // ill-typed program (TypeScript TS2344), not a relation that evaluates to a
    // boolean — surface it as an error rather than silently computing a result.
    let constraint_errors = env.constraint_errors(claim, &ctx);
    if !constraint_errors.is_empty() {
        return Outcome::Errors(
            constraint_errors
                .into_iter()
                .map(|error| Diagnostic {
                    span: error.span,
                    message: error.message,
                })
                .collect(),
        );
    }

    match eval_claim(&claim.simplify(), &ctx) {
        ExtendsResult::True => Outcome::Pass,
        result => Outcome::Fail { result },
    }
}

/// Reduce a (simplified) relational claim to an [`ExtendsResult`], resolving
/// named references on the leaves against `ctx`.
fn eval_claim(claim: &Ast, ctx: &ResolveCtx) -> ExtendsResult {
    match claim {
        Ast::ExtendsPrefixOp(ExtendsPrefixOp {
            op: PrefixOp::Not,
            value,
            ..
        }) => negate(eval_claim(value, ctx)),

        Ast::ExtendsInfixOp(ExtendsInfixOp { lhs, op, rhs, .. }) => match op {
            InfixOp::Extends => relation(lhs, rhs, ctx),
            InfixOp::NotExtends => negate(relation(lhs, rhs, ctx)),
            // `a == b` is `(a <: b) and (b <: a)`; strict and loose coincide here.
            InfixOp::Equals | InfixOp::StrictEquals => {
                relation(lhs, rhs, ctx).and(relation(rhs, lhs, ctx))
            }
            // `a != b` is `not (a <: b) or not (b <: a)`.
            InfixOp::NotEquals | InfixOp::StrictNotEquals => {
                negate(relation(lhs, rhs, ctx)).or(negate(relation(rhs, lhs, ctx)))
            }
            InfixOp::And => eval_claim(lhs, ctx).and(eval_claim(rhs, ctx)),
            InfixOp::Or => eval_claim(lhs, ctx).or(eval_claim(rhs, ctx)),
        },

        // `evaluate` only forwards relational claims here, and the operands of
        // `and`/`or` are relational by construction, so other nodes shouldn't
        // appear. Treat them as a definite failure rather than panicking.
        _ => ExtendsResult::False,
    }
}

/// Evaluate a single `lhs <: rhs` relation leaf for the assert harness.
///
/// A `Never` result means the left-hand side is (or reduces to) the bottom type
/// `never`, which is assignable to *everything* — so as an assertion the relation
/// **holds**. We map `Never -> True` here, at the relation leaf and *before* any
/// `not` is applied, so negation composes correctly (`negate(True) = False`):
/// `assert never <: string` passes while `assert not (never <: string)` fails.
///
/// This interpretation is specific to assert/claim evaluation. The conditional-
/// type path ([`runtime::builtin::unquote`](crate::runtime::builtin::unquote))
/// keeps `Never -> never` and is unaffected.
fn relation(lhs: &Ast, rhs: &Ast, ctx: &ResolveCtx) -> ExtendsResult {
    match lhs.is_assignable_to_ctx(rhs, ctx) {
        ExtendsResult::Never => ExtendsResult::True,
        other => other,
    }
}

/// Logical negation over [`ExtendsResult`], mirroring how the `not` prefix swaps
/// a conditional's branches: definite results flip, while `Never` (a collapsed
/// conditional) and `Both` (indeterminate) are preserved.
fn negate(result: ExtendsResult) -> ExtendsResult {
    match result {
        ExtendsResult::True => ExtendsResult::False,
        ExtendsResult::False => ExtendsResult::True,
        ExtendsResult::Never => ExtendsResult::Never,
        ExtendsResult::Both => ExtendsResult::Both,
    }
}

/// A human-readable description of a non-passing result, for the report.
fn describe(result: ExtendsResult) -> &'static str {
    match result {
        ExtendsResult::True => "`true`",
        ExtendsResult::False => "`false`",
        ExtendsResult::Never => "`never` (the left-hand side is the bottom type)",
        ExtendsResult::Both => {
            "indeterminate (`true | false`) — the claim involves `any` or a type \
            the environment can't resolve (e.g. an imported type)"
        }
    }
}

/// The span of a claim within an `assert <claim>` statement: the statement span
/// with the leading `assert` keyword (and following whitespace) trimmed off.
/// Unlike the claim node's own span, this keeps any grouping parentheses.
fn claim_span(source: &str, assert_span: Span) -> Span {
    let start = assert_span.start();
    let end = assert_span.end().min(source.len());

    let Some(text) = source.get(start..end) else {
        return assert_span;
    };

    // Drop the leading `assert` keyword and trim surrounding whitespace, so the
    // span is tight to the claim (the statement span can include a trailing
    // newline, which would otherwise bleed the caret onto the next line).
    let Some(rest) = text.strip_prefix("assert") else {
        return assert_span;
    };
    let leading_ws = rest.len() - rest.trim_start().len();
    let claim = rest.trim();

    let claim_start = start + "assert".len() + leading_ws;
    Span::new(claim_start, claim_start + claim.len())
}

/// The trimmed source slice for `span`, or `fallback` if it can't be sliced
/// (e.g. a synthesized span) or is empty.
fn slice_or<'a>(source: &'a str, span: Span, fallback: &'a str) -> &'a str {
    source
        .get(span.start()..span.end())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
}

/// Collect the `unittest` statements of a (top-level) program. `unittest` is a
/// top-level `statement`, so there is no need to recurse into nested scopes.
fn collect_unittests(program: &Ast) -> Vec<&crate::ast::UnitTest> {
    let statements = match program {
        Ast::Program(program) => program.statements.as_slice(),
        other => std::slice::from_ref(other),
    };

    statements
        .iter()
        .filter_map(|statement| match statement {
            Ast::Statement(inner) => as_unittest(inner),
            other => as_unittest(other),
        })
        .collect()
}

fn as_unittest(node: &Ast) -> Option<&crate::ast::UnitTest> {
    match node {
        Ast::UnitTest(unittest) => Some(unittest),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_newtype_program;

    /// Parse + simplify `src`, run the harness, and return `(report, stderr)`.
    fn run_src(src: &str, fail_fast: bool) -> (Report, String) {
        let program = parse_newtype_program(src).unwrap().simplify();
        let mut out = Vec::new();
        let report = run(
            &program,
            src,
            "<test>",
            Config {
                fail_fast,
                ..Config::default()
            },
            &mut out,
        )
        .unwrap();
        (report, String::from_utf8(out).unwrap())
    }

    #[test]
    fn passing_assertions_report_no_failures() {
        let (report, _) = run_src(
            "unittest \"t\" do\n  assert string <: unknown\n  assert 1 <: number\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 2,
                failed: 0
            }
        );
        assert!(!report.has_failures());
    }

    #[test]
    fn failing_assertion_is_counted() {
        let (report, stderr) = run_src("unittest \"t\" do\n  assert number <: string\nend", false);
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 1
            }
        );
        assert!(report.has_failures());
        assert!(stderr.contains("assertion failed"));
    }

    #[test]
    fn not_operator_is_supported() {
        let (report, _) = run_src(
            "unittest \"t\" do\n  assert not (number <: string)\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 1,
                failed: 0
            }
        );
    }

    #[test]
    fn equality_lowers_to_mutual_assignability() {
        let (report, _) = run_src("unittest \"t\" do\n  assert 1 == 1\nend", false);
        assert_eq!(
            report,
            Report {
                passed: 1,
                failed: 0
            }
        );
    }

    #[test]
    fn fail_fast_stops_after_first_failure() {
        let (report, _) = run_src(
            "unittest \"t\" do\n  assert number <: string\n  assert number <: string\nend",
            true,
        );
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 1
            }
        );
    }

    #[test]
    fn without_fail_fast_all_assertions_run() {
        let (report, _) = run_src(
            "unittest \"t\" do\n  assert number <: string\n  assert number <: string\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 2
            }
        );
    }

    #[test]
    fn program_without_unittests_reports_nothing() {
        let (report, stderr) = run_src("type Foo as 1", false);
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 0
            }
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn resolves_top_level_alias() {
        let (report, _) = run_src(
            "type Foo as 1\nunittest \"t\" do\n  assert Foo <: number\n  assert Foo == 1\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 2,
                failed: 0
            }
        );
    }

    #[test]
    fn alias_that_does_not_hold_fails() {
        let (report, _) = run_src(
            "type Foo as 1\nunittest \"t\" do\n  assert Foo <: string\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 1
            }
        );
    }

    #[test]
    fn resolves_generic_application() {
        let (report, _) = run_src(
            "type Id(T) as T\nunittest \"t\" do\n  assert Id(1) <: number\n  assert Id(string) == string\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 2,
                failed: 0
            }
        );
    }

    #[test]
    fn resolves_interface_shape() {
        let (report, _) = run_src(
            "interface Point { x: number }\nunittest \"t\" do\n  assert { x: 1, y: 2 } <: Point\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 1,
                failed: 0
            }
        );
    }

    #[test]
    fn resolves_interface_inheritance() {
        let src = "interface Point { x: number }\n\
            interface Point3 extends Point { z: number }\n\
            unittest \"t\" do\n\
            \x20 assert { x: 1, z: 3 } <: Point3\n\
            \x20 assert { z: 3 } <: Point3\n\
            end";
        let (report, _) = run_src(src, false);
        // The first holds; the second is missing the inherited `x`.
        assert_eq!(
            report,
            Report {
                passed: 1,
                failed: 1
            }
        );
    }

    #[test]
    fn generic_with_too_many_args_is_an_error() {
        let (report, stderr) = run_src(
            "type Id(T) as T\nunittest \"t\" do\n  assert Id(1, 2) <: number\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 1
            }
        );
        assert!(stderr.contains("expects 1 type argument(s), but 2 were provided"));
    }

    #[test]
    fn generic_with_too_few_args_is_an_error() {
        let (report, stderr) = run_src(
            "type Pair(A, B) as [A, B]\nunittest \"t\" do\n  assert Pair(1) <: [number, number]\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 1
            }
        );
        assert!(stderr.contains("expects 2 type argument(s), but 1 was provided"));
    }

    #[test]
    fn generic_default_fills_omitted_argument() {
        let (report, _) = run_src(
            "type WithDefault(A, B) defaults B = string as [A, B]\n\
            unittest \"t\" do\n  assert WithDefault(number) == [number, string]\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 1,
                failed: 0
            }
        );
    }

    #[test]
    fn never_lhs_makes_assertion_hold() {
        // `never` is the bottom type: assignable to everything, so as an
        // assertion `never <: T` holds. `string & number` reduces to `never`.
        let src = "unittest \"t\" do\n\
            \x20 assert never <: string\n\
            \x20 assert string & number <: never\n\
            \x20 assert string & number <: string\n\
            \x20 assert never <: never\n\
            end";
        let (report, _) = run_src(src, false);
        assert_eq!(
            report,
            Report {
                passed: 4,
                failed: 0
            }
        );
    }

    #[test]
    fn negated_never_relation_fails() {
        // `never <: string` holds, so its negation must fail — the `Never -> True`
        // interpretation happens at the leaf, before `not` is applied.
        let (report, _) = run_src(
            "unittest \"t\" do\n  assert not (never <: string)\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 1
            }
        );
    }

    #[test]
    fn string_is_not_never() {
        // `string <: never` is genuinely false (string is not the bottom type).
        let (report, _) = run_src("unittest \"t\" do\n  assert string <: never\nend", false);
        assert_eq!(
            report,
            Report {
                passed: 0,
                failed: 1
            }
        );
    }

    #[test]
    fn mapped_type_expands_against_object_literals() {
        // A mapped type over a statically-known key set expands to the equivalent
        // object literal and relates structurally (verified against tsgo --strict).
        let src = "type O as { a: number, b: string }\n\
            unittest \"t\" do\n\
            \x20 assert map K in \"a\" | \"b\" do number end <: { a: number, b: number }\n\
            \x20 assert { a: number, b: number } <: map K in \"a\" | \"b\" do number end\n\
            \x20 assert map K in keyof O do number end <: { a: number, b: number }\n\
            \x20 assert map K in keyof O do O[K] end <: O\n\
            \x20 assert O <: map K in keyof O do O[K] end\n\
            \x20 assert not (map K in \"a\" do string end <: { a: number })\n\
            end";
        let (report, _) = run_src(src, false);
        assert_eq!(
            report,
            Report {
                passed: 6,
                failed: 0
            }
        );
    }

    /// Like `run_src` but with `exact_optional_property_types` enabled.
    fn run_src_exact(src: &str) -> (Report, String) {
        let program = parse_newtype_program(src).unwrap().simplify();
        let mut out = Vec::new();
        let report = run(
            &program,
            src,
            "<test>",
            Config {
                exact_optional_property_types: true,
                ..Config::default()
            },
            &mut out,
        )
        .unwrap();
        (report, String::from_utf8(out).unwrap())
    }

    #[test]
    fn optional_target_accepts_undefined_by_default() {
        let src = "unittest \"t\" do\n\
            \x20 assert { x: number | undefined } <: { x?: number }\n\
            \x20 assert { x: undefined } <: { x?: number }\n\
            \x20 assert { x: number } <: { x?: number }\n\
            \x20 assert {} <: { x?: number }\n\
            \x20 assert not ({ x?: number } <: { x: number | undefined })\n\
            end";
        let (report, out) = run_src(src, false);
        assert_eq!(
            report,
            Report {
                passed: 5,
                failed: 0
            },
            "{out}"
        );
    }

    #[test]
    fn exact_optional_rejects_undefined_sources() {
        let src = "unittest \"t\" do\n\
            \x20 assert not ({ x: number | undefined } <: { x?: number })\n\
            \x20 assert not ({ x: undefined } <: { x?: number })\n\
            \x20 assert { x: number } <: { x?: number }\n\
            \x20 assert {} <: { x?: number }\n\
            \x20 assert not ({ x?: number } <: { x: number | undefined })\n\
            end";
        let (report, out) = run_src_exact(src);
        assert_eq!(
            report,
            Report {
                passed: 5,
                failed: 0
            },
            "{out}"
        );
    }

    #[test]
    fn recursive_alias_terminates() {
        // A self-referential type must not loop forever: the coinductive guard
        // takes `Rec <: Rec` as holding while it is being proven.
        let (report, _) = run_src(
            "type Rec as { next: Rec, value: number }\n\
            unittest \"t\" do\n  assert Rec <: Rec\n  assert Rec == Rec\nend",
            false,
        );
        assert_eq!(
            report,
            Report {
                passed: 2,
                failed: 0
            }
        );
    }
}
