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

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use crate::ast::type_env::fingerprint;
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

/// One recorded observation of a watched span: the span itself, plus the node
/// observed at that point in evaluation.
#[derive(Debug, Clone)]
pub struct DbgEvent {
    pub span: Span,
    pub observed: Ast,
}

/// Read-only sink accumulating `dbg!` observations during evaluation: a watch
/// table (which spans to notice) plus a deduped event log.
///
/// `observe`/`observe_replacement` are called from inside the assignability
/// engine as a side effect of demand-driven evaluation — they must never
/// change any return value or control flow, only append to the log.
///
/// `paused` suppresses both methods (a no-op while set): the harness's
/// flush step calls [`Ast::normalize`](crate::ast::Ast::normalize) to render
/// an already-drained event's value, and `normalize` re-enters the very same
/// reducers (`reduce_conditional`, `index_type`, `eval_keyof`) that carry
/// these hooks. Left unpaused, that presentation-only re-reduction would
/// itself count as "demand" and append fresh events *after* the drain that
/// already collected this claim's events — landing them in the next claim's
/// flush (misattribution) or, for the last claim, never being drained at all
/// (silently lost). `normalize` is documented as presentation-only and must
/// not be a demand source in its own right, so pausing for its duration is
/// correct, not merely a workaround.
#[derive(Debug, Default)]
pub struct DbgSink {
    watches: DbgWatches,
    events: RefCell<Vec<DbgEvent>>,
    seen: RefCell<HashSet<(usize, usize, String)>>,
    paused: Cell<bool>,
    /// `--trace-eval`: when set, [`Self::trace`] records a plain-line trace
    /// event for every instantiate cache miss and conditional decision.
    trace: Cell<bool>,
    trace_events: RefCell<Vec<String>>,
}

impl DbgSink {
    pub fn new(watches: DbgWatches) -> Self {
        Self {
            watches,
            events: RefCell::new(Vec::new()),
            seen: RefCell::new(HashSet::new()),
            paused: Cell::new(false),
            trace: Cell::new(false),
            trace_events: RefCell::new(Vec::new()),
        }
    }

    /// Enable (or disable) `--trace-eval` engine tracing on this sink.
    #[must_use]
    pub fn with_trace(self, on: bool) -> Self {
        self.trace.set(on);
        self
    }

    /// Whether trace mode is on (checked by callers before formatting a trace
    /// line, so no work is done when tracing is off).
    pub fn trace_enabled(&self) -> bool {
        self.trace.get()
    }

    /// Record a plain `trace: …` line, unless tracing is off or the sink is
    /// paused (flush-time renormalization must not self-trace). Unlike
    /// `observe`, trace lines are a log — not deduped.
    pub fn trace(&self, line: impl Into<String>) {
        if self.paused.get() || !self.trace.get() {
            return;
        }
        self.trace_events.borrow_mut().push(line.into());
    }

    /// Drain trace lines recorded since the last call.
    pub fn drain_trace(&self) -> Vec<String> {
        std::mem::take(&mut self.trace_events.borrow_mut())
    }

    /// If `node`'s span is watched, record a clone of it — deduped on
    /// `(span, fingerprint(node))` so repeated evaluation of the same site
    /// with the same value only reports once. No-op while [`Self::pause`]'s
    /// guard is alive.
    pub fn observe(&self, node: &Ast) {
        if self.paused.get() {
            return;
        }
        let span = node.as_span();
        if !self.watches.contains(span) {
            return;
        }
        self.record(span, node.clone());
    }

    /// For the substitution hook (Task 3): a watched bare-identifier span was
    /// replaced by `replacement` during instantiation. No-op while
    /// [`Self::pause`]'s guard is alive.
    pub fn observe_replacement(&self, ident_span: Span, replacement: &Ast) {
        if self.paused.get() {
            return;
        }
        if !self.watches.contains(ident_span) {
            return;
        }
        self.record(ident_span, replacement.clone());
    }

    fn record(&self, span: Span, observed: Ast) {
        let key = (span.start(), span.end(), fingerprint(&observed));
        if !self.seen.borrow_mut().insert(key) {
            return;
        }
        self.events.borrow_mut().push(DbgEvent { span, observed });
    }

    /// Drain events recorded since the last call.
    pub fn drain(&self) -> Vec<DbgEvent> {
        std::mem::take(&mut self.events.borrow_mut())
    }

    /// Suppress `observe`/`observe_replacement` for the lifetime of the
    /// returned guard (RAII: dropping it resumes). Re-entrant-safe only in
    /// the sense of restoring to "resumed" on drop — callers are not expected
    /// to nest pauses.
    #[must_use]
    pub fn pause(&self) -> PauseGuard<'_> {
        self.paused.set(true);
        PauseGuard { sink: self }
    }
}

/// RAII guard returned by [`DbgSink::pause`]; resumes observation on drop.
pub struct PauseGuard<'a> {
    sink: &'a DbgSink,
}

impl Drop for PauseGuard<'_> {
    fn drop(&mut self) {
        self.sink.paused.set(false);
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
            &DbgWatches::default(),
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

    /// Direct regression test for the flush-time suppression mechanism
    /// (Task 3's mandatory fix): `Ast::normalize`, invoked while rendering an
    /// already-drained event, re-enters the very reducers that carry `dbg!`
    /// hooks (`reduce_conditional`, `index_type`, `eval_keyof`). Without
    /// suppression, a watched span touched again during that presentation-
    /// only pass would append a *new* event after the drain that already
    /// collected this claim's events — landing in the next flush
    /// (misattribution) or, for the last claim, never being drained again
    /// (silently lost). `pause`/the `PauseGuard` must make `observe` and
    /// `observe_replacement` no-ops for exactly the guard's lifetime, with no
    /// panic and no event recorded, then resume cleanly afterward.
    #[test]
    fn paused_sink_drops_observations_and_resumes_cleanly() {
        use crate::ast::Span;

        let span = Span::new(5, 6);
        let mut watches = DbgWatches::default();
        watches.push(DbgWatch {
            span,
            bare_ident: None,
        });
        let sink = DbgSink::new(watches);

        let node = Ast::TrueKeyword(span);

        {
            let _guard = sink.pause();
            // Both observation entry points are no-ops while paused.
            sink.observe(&node);
            sink.observe_replacement(span, &node);
            assert!(sink.drain().is_empty(), "paused sink must record nothing");
        } // guard drops here, resuming the sink.

        // Resumed: the same span/value now fires normally, exactly once —
        // repeating it dedupes rather than double-counting.
        sink.observe(&node);
        sink.observe(&node);
        let events = sink.drain();
        assert_eq!(events.len(), 1, "resumed sink should observe normally");

        // A *different* value at the same watched span, observed once the
        // guard has dropped (simulating a mark demanded right after a flush
        // finishes, e.g. by the next claim's real evaluation), must still be
        // captured by this fresh drain — not lost because a guard existed
        // earlier in the sink's lifetime.
        let other = Ast::FalseKeyword(span);
        sink.observe(&other);
        let events = sink.drain();
        assert_eq!(
            events.len(),
            1,
            "an observation made once resumed must not vanish silently"
        );
    }
}
