//! The `dbg!` compile pass.
//!
//! Runs in the CLI after `desugar_globals` and BEFORE [`Ast::simplify`]: it
//! **erases** every `dbg!` call (replacing it with its argument, so asserts,
//! rendered TypeScript, and source maps are unaffected) while *recording* a
//! [`DbgWatch`] per debugged expression — and, for a pipeline
//! (`a |> b |> dbg!()`), one watch per pipeline step, first step first,
//! Elixir-style. Reporting no longer happens here: evaluation demands the
//! watched spans and reports on them as they're resolved (see the v2 design
//! doc); this pass only erases and records *where* to watch.
//!
//! The pass must run before `simplify()`: `simplify()` desugars `if`/`cond`/…
//! via `ExtendsExpr::new`, which asserts every operand is a TypeScript
//! feature — a `MacroCall` is not, so a `dbg!` inside an `if` branch would
//! panic during `simplify()` before this pass ever got a chance to strip it.

use std::cell::RefCell;
use std::collections::HashSet;

use crate::ast::{ApplyGeneric, Assert, Ast, Ident, Interface, MacroCall, Span, UnitTest};

/// A single watched site: the span evaluation must demand for it to fire.
#[derive(Debug, Clone)]
pub struct DbgWatch {
    pub span: Span,
    /// `Some(name)` when the watched node is a bare identifier (a probable
    /// type-parameter reference): substitution must observe its replacement.
    pub bare_ident: Option<String>,
}

/// The watch table produced by [`expand`]: every span evaluation must watch
/// for, plus a fast exact-span membership/lookup index.
#[derive(Debug, Clone, Default)]
pub struct DbgWatches {
    watches: Vec<DbgWatch>,
    index: HashSet<(usize, usize)>,
}

impl DbgWatches {
    fn push(&mut self, watch: DbgWatch) {
        self.index.insert((watch.span.start(), watch.span.end()));
        self.watches.push(watch);
    }

    pub fn is_empty(&self) -> bool {
        self.watches.is_empty()
    }

    /// Exact `(start, end)` match.
    pub fn contains(&self, span: Span) -> bool {
        self.index.contains(&(span.start(), span.end()))
    }

    pub fn bare_ident(&self, span: Span) -> Option<&str> {
        self.watches
            .iter()
            .find(|watch| watch.span.start() == span.start() && watch.span.end() == span.end())
            .and_then(|watch| watch.bare_ident.as_deref())
    }

    pub fn iter(&self) -> impl Iterator<Item = &DbgWatch> {
        self.watches.iter()
    }
}

/// One collected `dbg!` call: the steps to watch, first step first. A
/// non-pipeline argument is a single step.
struct DbgCall {
    steps: Vec<Ast>,
}

/// Strip every `dbg!` from `program`, recording a [`DbgWatch`] per step, and
/// return the cleaned program alongside the watch table.
pub fn expand(program: &Ast, source: &str, source_name: &str) -> (Ast, DbgWatches) {
    let calls = RefCell::new(Vec::new());
    let cleaned = strip(program, source, source_name, &calls);
    let calls = calls.into_inner();

    let mut watches = DbgWatches::default();
    for call in &calls {
        for step in &call.steps {
            let bare_ident = match step {
                Ast::Ident(Ident { name, .. }) => Some(name.clone()),
                _ => None,
            };
            watches.push(DbgWatch {
                span: step.as_span(),
                bare_ident,
            });
        }
    }

    (cleaned, watches)
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

    /// Parse `src`, run the pass, return (simplified cleaned AST, watches).
    fn run(src: &str) -> (Ast, DbgWatches) {
        let (cleaned, watches) = expand(&parse_newtype_program(src).unwrap(), src, "<test>");
        (cleaned.simplify(), watches)
    }

    fn render(src: &str) -> String {
        parse_newtype_program(src)
            .unwrap()
            .simplify()
            .render_pretty_ts(120)
    }

    #[test]
    fn plain_dbg_is_erased() {
        let (cleaned, _) = run("type User as { id: number }\ntype T as dbg!(User)");
        // Erased: renders exactly as if dbg! weren't there.
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type User as { id: number }\ntype T as User")
        );
    }

    #[test]
    fn pipeline_is_erased() {
        let src = "type Id(T) as T\n\
            type Box(T) as { value: T }\n\
            type T as 1 |> Id |> Box |> dbg!()";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type Id(T) as T\ntype Box(T) as { value: T }\ntype T as 1 |> Id |> Box")
        );
    }

    #[test]
    fn mid_pipeline_dbg_is_transparent() {
        let src = "type Id(T) as T\ntype T as 1 |> dbg!() |> Id";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type Id(T) as T\ntype T as 1 |> Id")
        );
    }

    #[test]
    fn hand_written_application_is_a_single_step() {
        let (_, watches) = run("type Box(T) as { value: T }\ntype T as dbg!(Box(1))");
        assert_eq!(watches.iter().count(), 1);
    }

    #[test]
    fn dbg_inside_assert_claim_is_erased() {
        let src = "unittest \"t\" do\n  assert dbg!(1) <: number\nend";
        let (cleaned, _) = run(src);
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
    fn dbg_inside_interface_body_is_erased() {
        let src = "interface Foo {\n  x: dbg!(1)\n}";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("interface Foo {\n  x: 1\n}")
        );
    }

    #[test]
    fn program_without_dbg_is_untouched_and_has_no_watches() {
        let src = "type A as 1";
        let (cleaned, watches) = run(src);
        assert!(watches.is_empty());
        assert_eq!(cleaned.render_pretty_ts(120), render(src));
    }

    #[test]
    #[should_panic(expected = "dbg! expects exactly one argument")]
    fn dbg_with_wrong_arity_panics_with_report() {
        run("type T as dbg!(1, 2)");
    }

    #[test]
    fn dbg_inside_let_binding_is_erased() {
        let src = "type T as let x = dbg!(1) in x";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type T as let x = 1 in x")
        );
    }

    #[test]
    fn dbg_inside_if_branch_is_erased() {
        let src = "type Get(A, K) as if K <: keyof A then dbg!(A[K]) else never end";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type Get(A, K) as if K <: keyof A then A[K] else never end")
        );
    }

    #[test]
    fn watches_record_marked_spans() {
        let src = "type T as dbg!(1)";
        let (_, watches) = run(src);
        let at = src.find('1').unwrap();
        assert!(watches.contains(crate::ast::Span::new(at, at + 1)));
    }

    #[test]
    fn pipeline_yields_one_watch_per_step() {
        let src = "type Id(T) as T\ntype B as 1 |> Id |> dbg!()";
        let (_, watches) = run(src);
        assert_eq!(watches.iter().count(), 2);
    }

    #[test]
    fn bare_ident_watch_is_flagged() {
        let src = "type Get(A, K) as dbg!(K)";
        let (_, watches) = run(src);
        let at = src.rfind('K').unwrap();
        assert_eq!(
            watches.bare_ident(crate::ast::Span::new(at, at + 1)),
            Some("K")
        );
    }
}
