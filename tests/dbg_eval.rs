//! Observer-effect differential test (v2 Task 5, Step 3).
//!
//! For every `tests/conformance/*.nt` fixture, run the assert harness twice on
//! the *same* simplified program: once with an empty `dbg!` watch table, once
//! with a watch table containing the span of every expression node inside
//! every `assert` claim in the file. If the demand-driven `dbg!` hooks are
//! truly read-only, watching everything must not change a single pass/fail
//! outcome — the two `Report`s must be identical.
//!
//! This is a permanent gate on the observer-effect discipline documented in
//! `src/ast/dbg_expr.rs`'s `DbgSink` doc comment, not a one-off regression
//! test.

use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::rc::Rc;

use newtype::ast::dbg_expr::{DbgWatch, DbgWatches};
use newtype::ast::Ast;
use newtype::parser::parse_newtype_program;
use newtype::test_harness::{self, Config};

/// Collect the span of every node reachable inside every `assert` claim in
/// `program` (walking `unittest` bodies explicitly, since `Ast::map`/
/// `traverse` do not descend into `Assert`/`UnitTest` — mirroring the
/// `dbg_expr::strip` pass, which recurses into them by hand for the same
/// reason).
fn collect_claim_spans(program: &Ast) -> Vec<DbgWatch> {
    let statements: &[Ast] = match program {
        Ast::Program(p) => p.statements.as_slice(),
        other => std::slice::from_ref(other),
    };

    let mut watches = Vec::new();
    for statement in statements {
        collect_from_statement(statement, &mut watches);
    }
    watches
}

fn collect_from_statement(node: &Ast, watches: &mut Vec<DbgWatch>) {
    match node {
        Ast::Statement(inner) => collect_from_statement(inner, watches),
        Ast::UnitTest(unittest) => {
            for stmt in &unittest.body {
                collect_from_statement(stmt, watches);
            }
        }
        Ast::Assert(assert) => collect_all_spans(&assert.claim, watches),
        _ => {}
    }
}

/// Every node span in `node`'s subtree, via `prewalk`. `Ast::traverse`
/// clones the context for each child and discards each child's *returned*
/// context (only the top-level one survives), so plain `Vec` accumulation
/// would silently drop everything gathered below the root. An
/// `Rc<RefCell<_>>` context sidesteps that: cloning it shares the same
/// backing `Vec`, so every `pre` call — root or descendant — appends to the
/// one shared accumulator regardless of which clone of the context is
/// eventually returned.
fn collect_all_spans(node: &Ast, watches: &mut Vec<DbgWatch>) {
    let acc: Rc<RefCell<Vec<DbgWatch>>> = Rc::new(RefCell::new(Vec::new()));
    node.prewalk(Rc::clone(&acc), &|ast, ctx| {
        let bare_ident = match &ast {
            Ast::Ident(ident) => Some(ident.name.clone()),
            _ => None,
        };
        ctx.borrow_mut().push(DbgWatch {
            span: ast.as_span(),
            bare_ident,
        });
        (ast, ctx)
    });
    watches.extend(Rc::try_unwrap(acc).unwrap().into_inner());
}

/// Run the harness on `program`, discarding the rendered report text — only
/// the pass/fail counts matter for this differential.
fn run_report(program: &Ast, source: &str, watches: &DbgWatches) -> test_harness::Report {
    let mut sink = Vec::new();
    test_harness::run(
        program,
        source,
        "<conformance>",
        Config::default(),
        watches,
        &mut sink,
    )
    .expect("writing the assertion report to an in-memory buffer cannot fail")
}

/// Watching every expression node inside every `assert` claim must not
/// change a single pass/fail outcome, for every `tests/conformance/*.nt`
/// fixture. A divergence here is a real observer-effect bug in the `dbg!`
/// evaluation hooks (`DbgSink::observe`/`observe_replacement`): the hooks are
/// documented as read-only, and this is the gate that keeps that true.
#[test]
fn watching_every_assert_expression_does_not_change_outcomes() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conformance");
    let mut checked = 0usize;

    let mut entries: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("reading {}: {err}", dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "nt"))
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "expected at least one *.nt fixture under {}",
        dir.display()
    );

    for path in entries {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
        let program = parse_newtype_program(&source)
            .unwrap_or_else(|err| panic!("parsing {}: {err:?}", path.display()))
            .simplify();

        let empty = DbgWatches::default();
        let full: DbgWatches = collect_claim_spans(&program).into_iter().collect();

        let baseline = run_report(&program, &source, &empty);
        let watched = run_report(&program, &source, &full);

        assert_eq!(
            baseline,
            watched,
            "observer effect: watching every assert expression in {} changed the report \
             (empty watches = {baseline:?}, full watches = {watched:?})",
            path.display()
        );
        checked += 1;
    }

    assert!(checked > 0, "no conformance fixtures were checked");
}
