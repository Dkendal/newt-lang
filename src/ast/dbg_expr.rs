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

/// Lets a watch table be assembled from an arbitrary collection of watches
/// (e.g. in tests, from spans collected by walking a program's `assert`
/// claims) without exposing the internal `Vec`/`HashSet` representation.
impl FromIterator<DbgWatch> for DbgWatches {
    fn from_iter<I: IntoIterator<Item = DbgWatch>>(iter: I) -> Self {
        let mut watches = DbgWatches::default();
        for watch in iter {
            watches.push(watch);
        }
        watches
    }
}

/// One evaluation frame for the `dbg!` stacktrace: a named-type application
/// in flight (or the harness's `assert claim` root), with its call-site span.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub label: String,
    pub span: Span,
}

/// The `--trace-eval` decision for a definite/indeterminate relation result:
/// which branch a conditional took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Then,
    Else,
    Never,
    Both,
}

/// One recorded `--trace-eval` event: an instantiate cache miss or a
/// conditional decision, anchored at a source span.
#[derive(Debug, Clone)]
pub struct TraceEvent {
    pub span: Span,
    pub message: String,
    /// `Some` for a conditional decision (which branch was taken); `None` for
    /// an instantiation event, which has no branch.
    pub decision: Option<Decision>,
}

/// One recorded observation of a watched span: the span itself, plus the node
/// observed at that point in evaluation.
#[derive(Debug, Clone)]
pub struct DbgEvent {
    pub span: Span,
    pub observed: Ast,
    /// The evaluation stack at record time, outermost frame first.
    pub stack: Vec<Frame>,
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
    /// Every watch span that has fired at least once via `record` (even if
    /// the specific event was deduped by `seen`), so the harness can report
    /// on marks that never fired at all. Keyed the same way as
    /// `DbgWatches`'s own index: `(span.start(), span.end())`.
    fired: RefCell<HashSet<(usize, usize)>>,
    paused: Cell<bool>,
    /// `--trace-eval`: when set, [`Self::trace`] records a plain-line trace
    /// event for every instantiate cache miss and conditional decision.
    trace: Cell<bool>,
    trace_events: RefCell<Vec<TraceEvent>>,
    /// Live evaluation stack for stacktraces: pushed/popped by [`Self::frame`]
    /// guards, snapshotted into each event by `record`.
    stack: RefCell<Vec<Frame>>,
}

impl DbgSink {
    pub fn new(watches: DbgWatches) -> Self {
        Self {
            watches,
            events: RefCell::new(Vec::new()),
            seen: RefCell::new(HashSet::new()),
            fired: RefCell::new(HashSet::new()),
            paused: Cell::new(false),
            trace: Cell::new(false),
            trace_events: RefCell::new(Vec::new()),
            stack: RefCell::new(Vec::new()),
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

    /// Record a `--trace-eval` event, unless tracing is off or the sink is
    /// paused (flush-time renormalization must not self-trace). Unlike
    /// `observe`, trace events are a log — not deduped.
    pub fn trace(&self, event: TraceEvent) {
        if self.paused.get() || !self.trace.get() {
            return;
        }
        self.trace_events.borrow_mut().push(event);
    }

    /// Drain trace events recorded since the last call.
    pub fn drain_trace(&self) -> Vec<TraceEvent> {
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
        self.fired.borrow_mut().insert((span.start(), span.end()));
        let key = (span.start(), span.end(), fingerprint(&observed));
        if !self.seen.borrow_mut().insert(key) {
            return;
        }
        self.events.borrow_mut().push(DbgEvent {
            span,
            observed,
            stack: self.stack.borrow().clone(),
        });
    }

    /// Drain events recorded since the last call.
    pub fn drain(&self) -> Vec<DbgEvent> {
        std::mem::take(&mut self.events.borrow_mut())
    }

    /// Count of watches (spans in [`DbgWatches`]) that never fired via
    /// [`Self::observe`]/[`Self::observe_replacement`] over the sink's whole
    /// lifetime — used for the "N dbg! mark(s) were never evaluated" hint
    /// printed once the harness run completes.
    pub fn never_fired_count(&self) -> usize {
        let fired = self.fired.borrow();
        self.watches
            .iter()
            .filter(|watch| !fired.contains(&(watch.span.start(), watch.span.end())))
            .count()
    }

    /// Suppress `observe`/`observe_replacement` for the lifetime of the
    /// returned guard (RAII: dropping it restores whatever paused state was
    /// in effect before the guard was created — safe under nesting).
    #[must_use]
    pub fn pause(&self) -> PauseGuard<'_> {
        let was_paused = self.paused.replace(true);
        PauseGuard {
            sink: self,
            was_paused,
        }
    }

    /// Whether the sink is currently paused (flush-time re-normalization):
    /// callers that must not let a paused instantiation poison a shared cache
    /// (e.g. [`TypeEnv::instantiate`](crate::ast::type_env::TypeEnv)) check
    /// this to skip caching the result of work done while paused.
    pub fn is_paused(&self) -> bool {
        self.paused.get()
    }

    /// Push an evaluation frame for the lifetime of the returned guard
    /// (RAII: dropping it pops the frame). No-op while paused — flush-time
    /// renormalization must not build stacks, mirroring `observe`.
    #[must_use]
    pub fn frame(&self, label: String, span: Span) -> FrameGuard<'_> {
        let pushed = !self.paused.get();
        if pushed {
            self.stack.borrow_mut().push(Frame { label, span });
        }
        FrameGuard { sink: self, pushed }
    }
}

/// RAII guard returned by [`DbgSink::pause`]; restores the prior paused state
/// on drop (rather than unconditionally resuming), so nested pauses are safe.
pub struct PauseGuard<'a> {
    sink: &'a DbgSink,
    was_paused: bool,
}

impl Drop for PauseGuard<'_> {
    fn drop(&mut self) {
        self.sink.paused.set(self.was_paused);
    }
}

/// RAII guard returned by [`DbgSink::frame`]; pops the frame it pushed (if
/// any — a paused sink pushes nothing) on drop.
pub struct FrameGuard<'a> {
    sink: &'a DbgSink,
    pushed: bool,
}

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        if self.pushed {
            self.sink.stack.borrow_mut().pop();
        }
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
        let (cleaned, _) = run("type User do { id: number } end\ntype T do dbg!(User) end");
        // Erased: renders exactly as if dbg! weren't there.
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type User do { id: number } end\ntype T do User end")
        );
    }

    #[test]
    fn pipeline_is_erased() {
        let src = "type Id(T) do T end\n\
            type Box(T) do { value: T } end\n\
            type T do 1 |> Id |> Box |> dbg!() end";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type Id(T) do T end\ntype Box(T) do { value: T } end\ntype T do 1 |> Id |> Box end")
        );
    }

    #[test]
    fn mid_pipeline_dbg_is_transparent() {
        let src = "type Id(T) do T end\ntype T do 1 |> dbg!() |> Id end";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type Id(T) do T end\ntype T do 1 |> Id end")
        );
    }

    #[test]
    fn hand_written_application_is_a_single_step() {
        let (_, watches) = run("type Box(T) do { value: T } end\ntype T do dbg!(Box(1)) end");
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
        let src = "type A do 1 end";
        let (cleaned, watches) = run(src);
        assert!(watches.is_empty());
        assert_eq!(cleaned.render_pretty_ts(120), render(src));
    }

    #[test]
    #[should_panic(expected = "dbg! expects exactly one argument")]
    fn dbg_with_wrong_arity_panics_with_report() {
        run("type T do dbg!(1, 2) end");
    }

    #[test]
    fn dbg_inside_let_binding_is_erased() {
        let src = "type T do let x = dbg!(1) in x end";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type T do let x = 1 in x end")
        );
    }

    #[test]
    fn dbg_inside_if_branch_is_erased() {
        let src = "type Get(A, K) do if K <: keyof A then dbg!(A[K]) else never end end";
        let (cleaned, _) = run(src);
        assert_eq!(
            cleaned.render_pretty_ts(120),
            render("type Get(A, K) do if K <: keyof A then A[K] else never end end")
        );
    }

    #[test]
    fn watches_record_marked_spans() {
        let src = "type T do dbg!(1) end";
        let (_, watches) = run(src);
        let at = src.find('1').unwrap();
        assert!(watches.contains(crate::ast::Span::new(at, at + 1)));
    }

    #[test]
    fn pipeline_yields_one_watch_per_step() {
        let src = "type Id(T) do T end\ntype B do 1 |> Id |> dbg!() end";
        let (_, watches) = run(src);
        assert_eq!(watches.iter().count(), 2);
    }

    #[test]
    fn bare_ident_watch_is_flagged() {
        let src = "type Get(A, K) do dbg!(K) end";
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

    /// Regression test for the final-review finding: `PauseGuard::drop` must
    /// restore whatever paused state was in effect before the guard was
    /// created, not unconditionally unpause. Nesting a guard while already
    /// paused (e.g. an outer flush-time pause around an inner instantiation
    /// path that also calls `pause`) must leave the sink paused once the
    /// inner guard drops — only the outermost guard's drop should resume it.
    #[test]
    fn nested_pause_restores_prior_paused_state() {
        let sink = DbgSink::new(DbgWatches::default());
        assert!(!sink.is_paused());

        let outer = sink.pause();
        assert!(sink.is_paused());
        {
            let inner = sink.pause();
            assert!(sink.is_paused());
            drop(inner);
            // Inner guard's drop must restore "paused" (the state from
            // before it was created), not unconditionally resume.
            assert!(
                sink.is_paused(),
                "inner guard's drop must not resume a sink an outer guard is still pausing"
            );
        }
        drop(outer);
        assert!(
            !sink.is_paused(),
            "outermost guard's drop must resume the sink"
        );
    }

    /// Frames snapshot into events at record time and pop LIFO: an event
    /// recorded while `outer`+`inner` are alive carries both (outermost
    /// first); one recorded after `inner` drops carries only `outer`.
    #[test]
    fn frames_snapshot_into_events_and_pop_lifo() {
        use crate::ast::Span;

        let span = Span::new(5, 6);
        let mut watches = DbgWatches::default();
        watches.push(DbgWatch {
            span,
            bare_ident: None,
        });
        let sink = DbgSink::new(watches);

        let outer = sink.frame("outer".to_string(), Span::new(0, 1));
        {
            let _inner = sink.frame("inner".to_string(), Span::new(2, 3));
            sink.observe(&Ast::TrueKeyword(span));
        }
        // Different value at the same span so dedupe doesn't drop it.
        sink.observe(&Ast::FalseKeyword(span));
        drop(outer);

        let events = sink.drain();
        assert_eq!(events.len(), 2);
        let labels: Vec<&str> = events[0].stack.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["outer", "inner"], "outermost frame first");
        let labels: Vec<&str> = events[1].stack.iter().map(|f| f.label.as_str()).collect();
        assert_eq!(labels, ["outer"], "inner frame must have popped");
    }

    /// A paused sink pushes no frames (flush-time renormalization must not
    /// build stacks), and the guard's drop is still safe.
    #[test]
    fn paused_sink_pushes_no_frames() {
        use crate::ast::Span;

        let sink = DbgSink::new(DbgWatches::default());
        let _pause = sink.pause();
        let _frame = sink.frame("x".to_string(), Span::new(0, 1));
        assert!(sink.stack.borrow().is_empty());
    }
}
