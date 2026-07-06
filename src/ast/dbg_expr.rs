//! The `dbg!` compile pass.
//!
//! Runs in the CLI between [`Ast::simplify`] and the assert harness: it
//! **erases** every `dbg!` call (replacing it with its argument, so asserts,
//! rendered TypeScript, and source maps are unaffected) while printing one
//! `Debug` report per debugged expression — and, for a pipeline
//! (`a |> b |> dbg!()`), one report per pipeline step, first step first,
//! Elixir-style. Each report shows the step's source excerpt and its
//! normalized type ([`Ast::normalize`]).

use std::cell::RefCell;
use std::io::{self, Write};

use crate::ast::type_env::{ResolveCtx, TypeEnv};
use crate::ast::{ApplyGeneric, Assert, Ast, Interface, MacroCall, UnitTest};
use crate::typescript::Pretty;

/// Width for pretty-printing normalized types in report labels.
const RENDER_WIDTH: usize = 80;

/// One collected `dbg!` call: the steps to report, first step first. A
/// non-pipeline argument is a single step.
struct DbgCall {
    steps: Vec<Ast>,
}

/// Strip every `dbg!` from `program`, print one Debug report per step to
/// `out`, and return the cleaned program. `source`/`source_name` anchor the
/// reports; `color` selects ANSI output (true in the CLI, false in tests).
pub fn expand(
    program: &Ast,
    source: &str,
    source_name: &str,
    color: bool,
    out: &mut dyn Write,
) -> io::Result<Ast> {
    let calls = RefCell::new(Vec::new());
    let cleaned = strip(program, source, source_name, &calls);
    let calls = calls.into_inner();

    if calls.is_empty() {
        return Ok(cleaned);
    }

    let env = TypeEnv::from_program(&cleaned);
    let ctx = ResolveCtx::new(&env);

    for call in &calls {
        for step in &call.steps {
            let normalized = step.normalize(&ctx);
            let message = format!("= {}", normalized.render_pretty_ts(RENDER_WIDTH));
            writeln!(
                out,
                "{}",
                crate::report::render_debug(source_name, source, step.as_span(), &message, color)
            )?;
        }
    }

    Ok(cleaned)
}

/// Replace every `dbg!(X)` with `X`, recording a [`DbgCall`] per occurrence.
/// `UnitTest` bodies, `Assert` claims, and `Interface` bodies are recursed
/// into explicitly — [`Ast::map`] deliberately does not descend into them
/// (mirroring `rewrite_unique_symbols` in `walk.rs`).
fn strip(node: &Ast, source: &str, source_name: &str, calls: &RefCell<Vec<DbgCall>>) -> Ast {
    match node {
        Ast::MacroCall(MacroCall { name, args, span }) if name == "dbg!" => {
            let [arg] = args.as_slice() else {
                let error = crate::report::render_to_string(
                    source_name,
                    source,
                    *span,
                    "dbg! expects exactly one argument: `dbg!(T)`, or `T |> dbg!()` in a pipeline",
                );
                panic!("{error}");
            };
            // Inner dbg! calls (e.g. `a |> dbg!() |> b |> dbg!()`) strip
            // first, so this call's steps see the already-cleaned argument.
            let arg = strip(arg, source, source_name, calls);
            calls.borrow_mut().push(DbgCall {
                steps: pipeline_steps(&arg),
            });
            arg
        }
        Ast::UnitTest(unittest) => Ast::UnitTest(UnitTest {
            span: unittest.span,
            name: unittest.name.clone(),
            body: unittest
                .body
                .iter()
                .map(|stmt| strip(stmt, source, source_name, calls))
                .collect(),
        }),
        Ast::Assert(assert) => Ast::Assert(Assert {
            span: assert.span,
            claim: strip(&assert.claim, source, source_name, calls).into(),
        }),
        Ast::Interface(interface) => Ast::Interface(Interface {
            span: interface.span,
            export: interface.export,
            name: interface.name.clone(),
            extends: interface
                .extends
                .as_ref()
                .map(|e| strip(e, source, source_name, calls).into()),
            params: interface
                .params
                .iter()
                .map(|p| p.map(|child| strip(child, source, source_name, calls)))
                .collect(),
            definition: interface
                .definition
                .iter()
                .cloned()
                .map(|prop| prop.map(|child| strip(child, source, source_name, calls)))
                .collect(),
        }),
        other => other.map(|child| strip(child, source, source_name, calls)),
    }
}

/// The pipeline steps of a `dbg!` argument, first step first: peel
/// `from_pipe` applications, so `c(b(a))` (from `a |> b |> c`) yields
/// `[a, b(a), c(b(a))]`. Peeling stops at the first non-pipe node — a
/// hand-written application or a mid-pipeline `dbg!` boundary — so a
/// non-pipeline argument is a single step. Each step's span covers the
/// source from the pipeline start through that step, which is exactly the
/// excerpt to display.
fn pipeline_steps(arg: &Ast) -> Vec<Ast> {
    let mut steps = vec![arg.clone()];
    let mut current = arg;
    while let Ast::ApplyGeneric(ApplyGeneric {
        args,
        from_pipe: true,
        ..
    }) = current
    {
        let Some(head) = args.first() else { break };
        steps.push(head.clone());
        current = head;
    }
    steps.reverse();
    steps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_newtype_program;
    use crate::typescript::Pretty;

    /// Parse + simplify `src`, run the pass, return (cleaned AST, report text).
    fn run(src: &str) -> (Ast, String) {
        let program = parse_newtype_program(src).unwrap().simplify();
        let mut out = Vec::new();
        let cleaned = expand(&program, src, "<test>", false, &mut out).unwrap();
        (cleaned, String::from_utf8(out).unwrap())
    }

    fn render(src: &str) -> String {
        parse_newtype_program(src)
            .unwrap()
            .simplify()
            .render_pretty_ts(120)
    }

    #[test]
    fn plain_dbg_reports_normalized_type_and_is_erased() {
        let (cleaned, out) = run("type User as { id: number }\ntype T as dbg!(User)");
        assert_eq!(out.matches("Debug").count(), 1, "{out}");
        // The label carries the *normalized* type, not the alias name.
        assert!(out.contains("id: number"), "{out}");
        // Erased: renders exactly as if dbg! weren't there.
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type User as { id: number }\ntype T as User")
        );
    }

    #[test]
    fn pipeline_reports_each_step_innermost_first() {
        let src = "type Id(T) as T\n\
            type Box(T) as { value: T }\n\
            type T as 1 |> Id |> Box |> dbg!()";
        let (cleaned, out) = run(src);
        assert_eq!(out.matches("Debug").count(), 3, "{out}");
        // Step order: `1`, then `1 |> Id`, then `1 |> Id |> Box`.
        let first = out.find("= 1").expect(&out);
        let last = out.find("value").expect(&out);
        assert!(first < last, "{out}");
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type Id(T) as T\ntype Box(T) as { value: T }\ntype T as 1 |> Id |> Box")
        );
    }

    #[test]
    fn mid_pipeline_dbg_reports_one_step_and_is_transparent() {
        let src = "type Id(T) as T\ntype T as 1 |> dbg!() |> Id";
        let (cleaned, out) = run(src);
        assert_eq!(out.matches("Debug").count(), 1, "{out}");
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type Id(T) as T\ntype T as 1 |> Id")
        );
    }

    #[test]
    fn hand_written_application_is_a_single_step() {
        let (_, out) = run("type Box(T) as { value: T }\ntype T as dbg!(Box(1))");
        assert_eq!(out.matches("Debug").count(), 1, "{out}");
        assert!(out.contains("value"), "{out}");
    }

    #[test]
    fn dbg_inside_assert_claim_is_reported_and_erased() {
        let src = "unittest \"t\" do\n  assert dbg!(1) <: number\nend";
        let (cleaned, out) = run(src);
        assert_eq!(out.matches("Debug").count(), 1, "{out}");
        // The cleaned program's assert must still evaluate (and pass).
        let mut sink = Vec::new();
        let report = crate::test_harness::run(
            &cleaned,
            src,
            "<test>",
            crate::test_harness::Config::default(),
            &mut sink,
        )
        .unwrap();
        assert_eq!((report.passed, report.failed), (1, 0));
    }

    #[test]
    fn dbg_inside_interface_body_is_reported_and_erased() {
        let src = "interface Foo {\n  x: dbg!(1)\n}";
        let (cleaned, out) = run(src);
        assert_eq!(out.matches("Debug").count(), 1, "{out}");
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("interface Foo {\n  x: 1\n}")
        );
    }

    #[test]
    fn program_without_dbg_is_untouched_and_silent() {
        let src = "type A as 1";
        let (cleaned, out) = run(src);
        assert!(out.is_empty(), "{out}");
        assert_eq!(cleaned.render_pretty_ts(120), render(src));
    }

    #[test]
    #[should_panic(expected = "dbg! expects exactly one argument")]
    fn dbg_with_wrong_arity_panics_with_report() {
        run("type T as dbg!(1, 2)");
    }
}
