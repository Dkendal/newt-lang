# `dbg!` stacktraces + ariadne `--trace-eval` reports

**Date:** 2026-07-06
**Status:** approved
**Extends:** `2026-07-05-dbg-macro-v2-watchpoints-design.md` (the span-watchpoint
architecture; nothing there is superseded).

## Motivation

`dbg!` reports show *what* a watched expression evaluated to but not *how
evaluation got there*; `--trace-eval` emits plain unanchored `trace: …` lines
with no source location. Three improvements:

1. Each `dbg!` report carries an **evaluation stacktrace** — the chain of
   generic instantiations in flight when the mark fired.
2. `--trace-eval` output is anchored: every trace line carries
   `file:line:col` and renders in ariadne's visual style under
   `ReportKind::Custom("Trace")`.
3. **Colour indicates control flow**: the decision word of a conditional
   trace is coloured by which branch was taken.

## 1. Frame stack (new mechanism in `DbgSink`)

`DbgSink` gains a live evaluation stack:

```rust
pub struct Frame { pub label: String, pub span: Span }
// in DbgSink:
stack: RefCell<Vec<Frame>>,
```

pushed/popped via an RAII `FrameGuard` (same shape as the existing
`PauseGuard`: drop restores, safe under nesting and early returns). A helper
on the sink (`fn frame(&self, label, span) -> FrameGuard`) is a no-op-ish
push; callers without a sink skip it entirely.

**Where frames are pushed — resolve-and-recurse sites, not `instantiate`.**
`TypeEnv::instantiate` only substitutes and returns; the resolved body is
evaluated *afterwards* in the caller. So the guard wraps each engine site in
`assignability.rs` that does `ctx.resolve_head(node)` **and then recurses
into the result** — the frame is alive for exactly the dynamic extent of that
named type's evaluation. Frame label = `one_line(node)` capped at ~60 chars;
frame span = the application node's own span.

Deliberate consequences:

- `observe_replacement` (the `dbg!(K)` bare-parameter hook, firing inside
  `substitute` during instantiation) runs within the resolving caller's
  guard, so it snapshots the correct stack.
- **Cache hits still get stacks**: frames live at the resolve call sites,
  which run identically whether `instantiate` hits or misses its cache.
- Only *named-type applications* become frames (per the chosen design) —
  not every assignability-relation or conditional-reduction step.

**Root frame.** The assert harness pushes one frame per claim
(label `assert claim`, span = the claim's span) around `evaluate`, so every
stack bottoms out at the assert that drove the evaluation.

**Snapshot.** `DbgEvent` gains `stack: Vec<Frame>` — a clone of the sink's
stack at `record()` time, innermost frame last-pushed. Dedupe is unchanged
(`(span, fingerprint(observed))`); when the same value is reached via two
different stacks, the first-recorded stack wins.

**Rendering.** The flush step appends plain indented lines under the existing
Debug report, innermost frame first:

```
[Debug] ts-toolbelt.nt:12:27
    ╭─[ts-toolbelt.nt:12:27]
 12 │ ... dbg!(K) ...
    │          ┬
    │          ╰── = 'id'
────╯
    in Get(User, 'id')   ts-toolbelt.nt:33:19
    in At(User, 'id')    ts-toolbelt.nt:40:10
    in assert claim      ts-toolbelt.nt:40:3
```

Locations come from a new `pub fn line_col(source: &str, offset: usize) ->
(usize, usize)` helper in `src/report.rs` (1-based, matching ariadne's own
numbering; offsets past EOF clamp to the last position). Labels are padded so
the location column aligns within one report. An event with an empty stack
(e.g. a mark demanded outside any named instantiation) renders only the
`assert claim` frame, or no frame lines at all if the stack is empty.

## 2. `--trace-eval` as ariadne `Custom("Trace")` output

`DbgSink`'s `trace_events` changes from `Vec<String>` to `Vec<TraceEvent>`:

```rust
pub struct TraceEvent {
    pub span: Span,
    pub message: String,
    pub decision: Option<Decision>,
}
pub enum Decision { Then, Else, Never, Both }
```

Producers:

- **Instantiation** (in `TypeEnv::instantiate`): gains a real span —
  `resolve_head` passes the call-site node's span into `instantiate` as a new
  parameter. Message: `Name(args…) = <one_line(body)>`, `decision: None`.
- **Conditional decision** (`trace_conditional` in `assignability.rs`):
  span = the conditional expression's span. Message:
  `<check> <: <extends> → <decision>` with the decision word carried in
  `decision: Some(_)` so rendering can colour it.

**Rendering** happens in the harness flush (where the source text lives).
ariadne cannot emit a label-free one-liner, so each event is formatted as a
single line *in ariadne's visual style*, using ariadne's `Color` type and the
`line_col` helper:

```
[Trace] ts-toolbelt.nt:40:10  At(User, 'id') = Get(User, 'id')
[Trace] ts-toolbelt.nt:33:19  Get(User, 'id') = if 'id' <: keyof User ...
[Trace] ts-toolbelt.nt:12:19  'id' <: keyof User → then
```

The `[Trace]` kind word is `Color::Cyan` (same family as `[Debug]`; the kind
word itself disambiguates). The `trace:` plain prefix is gone; existing trace
tests update to the new shape.

## 3. Colours

Decision-word colouring (word only, not the whole line):

| decision | colour |
| --- | --- |
| `then` | green |
| `else` | red |
| `both (indeterminate)` | yellow |
| `never` | magenta |

Colour is applied only when the existing colour switch is on — the same
`color: bool` already threaded to `render_debug` (the CLI passes `true` for
stderr; tests and string renderers pass `false`). With colour off, output
contains no ANSI escapes (existing tests assert this and stay).

## What changes, concretely

- `src/ast/dbg_expr.rs`: `Frame`, `FrameGuard`, `sink.frame(…)`,
  `DbgEvent.stack`, `TraceEvent`/`Decision`, `trace(TraceEvent)`,
  `drain_trace() -> Vec<TraceEvent>`. Frame pushes respect `paused` (a
  paused sink neither records frames nor observes).
- `src/ast/assignability.rs`: frame guards at the resolve-and-recurse sites;
  `trace_conditional` builds a `TraceEvent` with span + decision.
- `src/ast/type_env.rs`: `instantiate` gains a call-site span parameter
  (threaded from `resolve_head`); its trace becomes a `TraceEvent`.
- `src/test_harness.rs`: pushes the per-claim root frame; flush renders
  frame lines under each Debug report and formats `TraceEvent`s as
  `[Trace]` lines (colour-aware).
- `src/report.rs`: `line_col` helper; a small `render_trace_line` /
  `render_frames` pair so formatting is testable in isolation.
- `src/main.rs`: unchanged behaviour; colour flag already threads through.

## Testing

- **Stack contents (the motivating case):** ts-toolbelt shape —
  `assert At(User,'id') <: number` with `dbg!(K)` inside `Get`; the Debug
  report is followed by `in Get(User, 'id')`, `in At(User, 'id')`,
  `in assert claim`, innermost first, each with the correct `line:col`.
- **Cache-hit stacks:** two claims driving different instantiations of the
  same generic; the second event still carries a full stack.
- **Guard discipline:** nested frames pop in LIFO order; a paused sink
  records no frames.
- **`line_col`:** first char, mid-line, after newline, offset at/past EOF.
- **Trace shape:** `[Trace]` lines carry `file:line:col`; instantiation and
  decision events both appear; `trace:` prefix gone.
- **Colours:** with colour on, the decision word carries the mapped colour
  (then/else/both/never each tested); with colour off, no ANSI escapes.
- **No observer effect:** the existing differential test (all
  `tests/conformance/*.nt`, watched vs unwatched) continues to pass — frame
  push/pop and trace events are read-only observations.
- Full suite (`cargo nextest run`) and conformance (`mise run tc`) stay
  green.
