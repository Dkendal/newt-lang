# dbg! Stacktraces + ariadne --trace-eval Reports Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Each `dbg!` report gains an evaluation stacktrace (the chain of generic instantiations in flight); `--trace-eval` output becomes source-anchored `[Trace] file:line:col …` lines with the taken branch colour-coded.

**Architecture:** `DbgSink` (src/ast/dbg_expr.rs) gains a live frame stack pushed/popped by an RAII `FrameGuard` at two kinds of site — inside `TypeEnv::instantiate` around substitution, and at the resolve-and-recurse sites in `assignability.rs` — snapshotted into each `DbgEvent`. Trace lines become structured `TraceEvent`s carrying a span and an optional branch `Decision`, rendered by the harness flush via a new `report::render_trace_line`.

**Tech Stack:** Rust, ariadne 0.6 (visual style only — the compact one-liner is hand-formatted because ariadne cannot emit a label-free single line), cargo nextest, jj (colocated with git).

**Spec:** `docs/superpowers/specs/2026-07-06-dbg-stacktrace-trace-reports-design.md`

## Global Constraints

- **jj, not git**: commit with `jj commit -m "<msg>" <paths>` scoped to the files you changed. Never `git commit`. Run `cargo fmt` before every commit.
- **NEVER commit `examples/ts-toolbelt.nt`** — it carries uncommitted local experiments. Never pass it to `jj commit`.
- **No observer effect**: every hook added to the engine is read-only — frame push/pop and trace recording must never change a return value or control flow. The differential test `tests/dbg_eval.rs` guards this and must stay green.
- **Paused sink is inert**: `DbgSink::frame` pushes nothing while the sink is paused (flush-time renormalization must not build stacks), mirroring `observe`/`trace`.
- Frame labels are `one_line(...)` output truncated to **60 chars**; stack lines render **innermost frame first**; the harness root frame label is exactly `assert claim`.
- Decision colours (word only, raw ANSI, applied only when the colour flag is on): `then` → green `\x1b[32m`, `else` → red `\x1b[31m`, `never` → magenta `\x1b[35m`, `both (indeterminate)` → yellow `\x1b[33m`. `[Trace]` kind word → cyan `\x1b[36m`. Reset `\x1b[0m`. With colour off, output contains no ANSI escapes.
- `line:col` are 1-based (matching ariadne); offsets past EOF clamp.
- Test commands: `cargo nextest run <filter>`; full suite `cargo nextest run`; conformance `mise run tc`.

---

### Task 1: Frame stack in DbgSink + `line_col` helper

**Files:**
- Modify: `src/ast/dbg_expr.rs` (add `Frame`, `FrameGuard`, `DbgSink::frame`, `DbgEvent.stack`)
- Modify: `src/report.rs` (add `line_col`)
- Test: inline `#[cfg(test)] mod tests` in both files

**Interfaces:**
- Consumes: existing `DbgSink` internals (`paused: Cell<bool>`, `events`, `record`), `Span` (has `.start()`/`.end()` methods and public `start`/`end` fields).
- Produces: `pub struct Frame { pub label: String, pub span: Span }`; `DbgSink::frame(&self, label: String, span: Span) -> FrameGuard<'_>`; `DbgEvent` gains `pub stack: Vec<Frame>`; `pub fn line_col(source: &str, offset: usize) -> (usize, usize)` in `src/report.rs`. Tasks 2–3 rely on these exact names.

- [ ] **Step 1: Write the failing tests**

In `src/ast/dbg_expr.rs`'s existing `mod tests`, add:

```rust
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
```

In `src/report.rs`'s existing `mod tests`, add:

```rust
#[test]
fn line_col_is_one_based_and_clamps() {
    let source = "ab\ncd\n";
    assert_eq!(line_col(source, 0), (1, 1)); // 'a'
    assert_eq!(line_col(source, 1), (1, 2)); // 'b'
    assert_eq!(line_col(source, 3), (2, 1)); // 'c' (after newline)
    assert_eq!(line_col(source, 4), (2, 2)); // 'd'
    assert_eq!(line_col(source, 100), (3, 1)); // past EOF clamps to end
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run frames_snapshot_into_events; cargo nextest run line_col_is_one_based`
Expected: COMPILE ERROR (`frame` method / `line_col` function not found).

- [ ] **Step 3: Implement**

In `src/ast/dbg_expr.rs`, above `DbgEvent`:

```rust
/// One evaluation frame for the `dbg!` stacktrace: a named-type application
/// in flight (or the harness's `assert claim` root), with its call-site span.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub label: String,
    pub span: Span,
}
```

Extend `DbgEvent`:

```rust
#[derive(Debug, Clone)]
pub struct DbgEvent {
    pub span: Span,
    pub observed: Ast,
    /// The evaluation stack at record time, outermost frame first.
    pub stack: Vec<Frame>,
}
```

Add to `DbgSink`'s fields (after `fired`):

```rust
    /// Live evaluation stack for stacktraces: pushed/popped by [`Self::frame`]
    /// guards, snapshotted into each event by `record`.
    stack: RefCell<Vec<Frame>>,
```

Initialize `stack: RefCell::new(Vec::new())` in `DbgSink::new`. Update `record` to snapshot:

```rust
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
```

Add the guard API (next to `pause`/`PauseGuard`):

```rust
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
```

```rust
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
```

In `src/report.rs` (above the tests module):

```rust
/// The 1-based `(line, column)` of a byte `offset` into `source`, matching
/// ariadne's own numbering. Offsets past EOF clamp to the final position.
pub fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
```

- [ ] **Step 4: Run tests to verify they pass, then the whole suite**

Run: `cargo nextest run frames_snapshot_into_events; cargo nextest run line_col_is_one_based; cargo nextest run`
Expected: new tests PASS; full suite green (the `stack` field is only constructed in `record`, so nothing else breaks).

- [ ] **Step 5: Commit**

```bash
cargo fmt
jj commit -m "Add dbg! frame stack to DbgSink and line_col helper" src/ast/dbg_expr.rs src/report.rs
```

---

### Task 2: Frame push sites, harness root frame, stacktrace rendering

**Files:**
- Modify: `src/ast/type_env.rs` (`instantiate` gains a `site: Span` parameter + site-1 frame; new `frame_label` helpers)
- Modify: `src/ast/assignability.rs` (site-2 frames in `is_assignable_to_ctx` ~lines 93–107 and `reduce_conditional` ~line 1443)
- Modify: `src/test_harness.rs` (root frame per claim; frame lines under each Debug report)
- Test: `src/test_harness.rs` inline tests

**Interfaces:**
- Consumes: Task 1's `DbgSink::frame(label: String, span: Span) -> FrameGuard`, `DbgEvent.stack: Vec<Frame>`, `report::line_col`. `TypeEnv::dbg()` returns `Option<&DbgSink>`; `ctx.env()` returns the env option; `one_line`/`truncate_chars` live in `type_env.rs`.
- Produces: `pub(crate) fn frame_label(node: &Ast) -> String` and `pub(crate) fn frame_label_apply(name: &str, args: &[Ast]) -> String` in `type_env.rs` (both ≤60 chars); `instantiate(&self, name: &str, args: &[Ast], site: Span)` (private — callers are only `resolve_head`). Rendered frame-line format consumed by no later code: `    in {label:<width}   {source_name}:{line}:{col}`.

- [ ] **Step 1: Write the failing tests**

In `src/test_harness.rs`'s `mod tests` (near the existing dbg tests; reuse the existing `run_src_dbg` helper, which expands `dbg!` and runs the harness with watches):

```rust
/// The motivating case for stacktraces: a bare-parameter mark inside `Get`,
/// reached via `At(User, 'id')`, reports its live binding AND the chain of
/// instantiations that led there — innermost frame first, each with a
/// 1-based source location.
#[test]
fn dbg_report_includes_instantiation_stacktrace() {
    let src = "type User as { id: number }\n\
        type Get(A, K) as if dbg!(K) <: keyof A then A[K] else never end\n\
        type At(A, K) as Get(A, K)\n\
        unittest \"t\" do\n\
        \x20 assert At(User, 'id') <: number\n\
        end";
    let (report, out) = run_src_dbg(src);
    assert_eq!((report.passed, report.failed), (1, 0), "{out}");
    assert!(out.contains("= 'id'"), "live binding: {out}");

    let get = out
        .find("in Get(User,")
        .expect(&format!("Get frame missing: {out}"));
    let at = out
        .find("in At(User,")
        .expect(&format!("At frame missing: {out}"));
    let claim = out
        .find("in assert claim")
        .expect(&format!("claim frame missing: {out}"));
    assert!(get < at && at < claim, "innermost frame first: {out}");

    // Locations: `Get(A, K)` is applied on line 3, the claim is on line 5.
    assert!(out.contains("<test>:3:"), "Get frame location: {out}");
    assert!(out.contains("<test>:5:"), "claim frame location: {out}");
}

/// Distinct instantiations of the same generic each fire with a full stack
/// (the spec's cache-interaction case: the second event must not lose its
/// frames to the first's cache entry).
#[test]
fn each_distinct_instantiation_gets_a_full_stack() {
    let src = "type Probe(K) as dbg!(K)\n\
        type Wrap(K) as Probe(K)\n\
        unittest \"t\" do\n\
        \x20 assert Wrap(1) <: number\n\
        \x20 assert Wrap(2) <: number\n\
        end";
    let (report, out) = run_src_dbg(src);
    assert_eq!((report.passed, report.failed), (2, 0), "{out}");
    assert!(out.contains("= 1"), "{out}");
    assert!(out.contains("= 2"), "{out}");
    assert!(out.contains("in Probe(1)"), "{out}");
    assert!(out.contains("in Probe(2)"), "{out}");
    assert!(out.contains("in Wrap(1)"), "{out}");
    assert!(out.contains("in Wrap(2)"), "{out}");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run dbg_report_includes_instantiation_stacktrace; cargo nextest run each_distinct_instantiation_gets_a_full_stack`
Expected: FAIL — no `in …` frame lines are printed yet.

- [ ] **Step 3: Add the frame-label helpers and the site-1 frame in `type_env.rs`**

Next to `one_line` (bottom of `src/ast/type_env.rs`):

```rust
/// A frame label for the `dbg!` stacktrace: the node on one line, truncated
/// so the location column stays readable. A named application is spelled in
/// newtype call syntax (`Wrap(1)`), matching the labels `instantiate` builds
/// for the same application — `one_line` would render TypeScript's `Wrap<1>`
/// and the two frame kinds would disagree about the same call.
pub(crate) fn frame_label(node: &Ast) -> String {
    if let Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) = node {
        if let Ast::Ident(Ident { name, .. }) = receiver.as_ref() {
            return frame_label_apply(name, args);
        }
    }
    truncate_chars(&one_line(node), 60)
}

/// A frame label for an instantiation: `Name(arg, …)` (or the bare name for
/// a zero-argument application), truncated like [`frame_label`].
pub(crate) fn frame_label_apply(name: &str, args: &[Ast]) -> String {
    if args.is_empty() {
        return truncate_chars(name, 60);
    }
    let args_src = args
        .iter()
        .map(one_line)
        .collect::<Vec<_>>()
        .join(", ");
    truncate_chars(&format!("{name}({args_src})"), 60)
}
```

Change `instantiate`'s signature and add the substitution frame (site 1 — this is what puts `Get(User, 'id')` itself on the stack when a bare-parameter mark fires inside `substitute`):

```rust
    /// Expand `name(args)` to its definition's body, substituting `args` for the
    /// definition's parameters. Interns and caches the result. `site` is the
    /// call-site span (the applying node), used for `dbg!` stack frames and
    /// `--trace-eval` locations.
    fn instantiate(&self, name: &str, args: &[Ast], site: Span) -> Option<Ast> {
        let def = self.defs.get(name)?;

        let key = instantiation_key(name, args);
        if let Some(id) = self.cache.borrow().get(&key) {
            return Some(self.arena.borrow()[*id].clone());
        }

        let body = if def.params.is_empty() {
            def.body.clone()
        } else {
            // `dbg!` stacktrace frame for the generic being instantiated:
            // `observe_replacement` fires inside `distribute_or_substitute`,
            // so this frame must already be on the stack at that moment.
            let _frame = self
                .dbg()
                .map(|sink| sink.frame(frame_label_apply(name, args), site));
            let bindings = bind_params(&def.params, args);
            distribute_or_substitute(&def.body, &bindings, self.dbg())
        };
        // … rest unchanged (trace block, paused-cache skip, cache insert) …
```

Update `resolve_head` to thread the call-site span:

```rust
    pub fn resolve_head(&self, ast: &Ast) -> Option<Ast> {
        match ast {
            Ast::Ident(Ident { name, .. }) => self.instantiate(name, &[], ast.as_span()),
            Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) => match receiver.as_ref() {
                Ast::Ident(Ident { name, .. }) => self.instantiate(name, args, ast.as_span()),
                _ => None,
            },
            _ => None,
        }
    }
```

`Span` may need importing in `type_env.rs` if not already (`use crate::ast::Span` — check the existing use list).

- [ ] **Step 4: Add the site-2 frames in `assignability.rs`**

Import the helper: add `frame_label` to the existing `type_env::{…}` use item at the top of `src/ast/assignability.rs`.

In `is_assignable_to_ctx`, the named-reference block (~lines 93–107) — push one frame per side that resolved, alive for the recursion into the resolved body:

```rust
        if let Some(env) = ctx.env() {
            let lhs_resolved = env.resolve_head(self);
            let rhs_resolved = env.resolve_head(other);

            if lhs_resolved.is_some() || rhs_resolved.is_some() {
                let pair = (fingerprint(self), fingerprint(other));
                if !ctx.assume(&pair) {
                    return T::True;
                }
                // `dbg!` stacktrace frames: the resolved body is evaluated by
                // the recursion below, so each resolved side's application
                // stays on the stack for exactly that extent. Read-only.
                let sink = ctx.env().and_then(|env| env.dbg());
                let _lhs_frame = sink
                    .filter(|_| lhs_resolved.is_some())
                    .map(|s| s.frame(frame_label(self), self.as_span()));
                let _rhs_frame = sink
                    .filter(|_| rhs_resolved.is_some())
                    .map(|s| s.frame(frame_label(other), other.as_span()));
                let lhs = lhs_resolved.as_ref().unwrap_or(self);
                let rhs = rhs_resolved.as_ref().unwrap_or(other);
                let result = lhs.is_assignable_to_ctx(rhs, ctx);
                ctx.discharge(&pair);
                return result;
            }
        }
```

In `reduce_conditional` (~line 1443), frame the resolved check for the duration of branch selection:

```rust
        // Resolve a named check type one step (e.g. an alias) before matching.
        let resolved = ctx.env().and_then(|env| env.resolve_head(lhs));
        // `dbg!` stacktrace frame for a named check type: alive while the
        // resolved check is related below. Read-only.
        let sink = ctx.env().and_then(|env| env.dbg());
        let _check_frame = sink
            .filter(|_| resolved.is_some())
            .map(|s| s.frame(frame_label(lhs), lhs.as_span()));
        let check = resolved.unwrap_or_else(|| (**lhs).clone());
```

(`sink` is `Option<&DbgSink>`, which is `Copy` — using it twice is fine.)

- [ ] **Step 5: Root frame + frame-line rendering in `test_harness.rs`**

Wrap `evaluate` so every stack bottoms out at the driving claim. Replace `match evaluate(&assert.claim, &env, config) {` with:

```rust
            let outcome = {
                // Root `dbg!` stacktrace frame: every evaluation this claim
                // triggers reports `in assert claim` at the claim's span.
                let _claim_frame = sink
                    .as_deref()
                    .map(|s| s.frame("assert claim".to_string(), span));
                evaluate(&assert.claim, &env, config)
            };
            match outcome {
```

In `flush_dbg_events`, after writing each Debug report, append the frame lines (innermost first — the stack is stored outermost-first, so iterate reversed), padding labels so the location column aligns within one report:

```rust
    for event in events {
        let rendered = event.observed.normalize(ctx).render_pretty_ts(80);
        writeln!(
            out,
            "{}",
            crate::report::render_debug(
                source_name,
                source,
                event.span,
                &format!("= {rendered}"),
                false,
            )
        )?;
        write_frames(&event.stack, source_name, source, out)?;
    }
```

New function below `flush_dbg_events`:

```rust
/// Render a `dbg!` event's evaluation stacktrace, innermost frame first,
/// Elixir-style: `    in <label>   <file>:<line>:<col>`. Labels are padded so
/// the location column aligns within one report. No-op for an empty stack.
fn write_frames(
    stack: &[crate::ast::dbg_expr::Frame],
    source_name: &str,
    source: &str,
    out: &mut dyn Write,
) -> io::Result<()> {
    let Some(width) = stack.iter().map(|f| f.label.chars().count()).max() else {
        return Ok(());
    };
    for frame in stack.iter().rev() {
        let (line, col) = crate::report::line_col(source, frame.span.start());
        writeln!(
            out,
            "    in {:<width$}   {source_name}:{line}:{col}",
            frame.label
        )?;
    }
    Ok(())
}
```

- [ ] **Step 6: Run tests, then the whole suite (including the observer-effect differential test)**

Run: `cargo nextest run dbg_report_includes_instantiation_stacktrace; cargo nextest run each_distinct_instantiation_gets_a_full_stack; cargo nextest run`
Expected: new tests PASS; full suite green — `tests/dbg_eval.rs` (differential, all conformance files watched vs unwatched) must pass, proving frame push/pop changed no outcomes.

If the motivating test's frame labels don't match (e.g. string args render as `"id"` not `'id'`), fix the *test's* expectation to the pretty-printer's actual output — the labels use `one_line`, whose rendering is TypeScript syntax; the assertions above (`in Get(User,`) are written to be quoting-agnostic. Do not change the pretty-printer.

- [ ] **Step 7: Commit**

```bash
cargo fmt
jj commit -m "Record dbg! evaluation stacktraces and render them under Debug reports" src/ast/type_env.rs src/ast/assignability.rs src/test_harness.rs
```

---

### Task 3: Structured TraceEvents, `[Trace]` rendering, decision colours

**Files:**
- Modify: `src/ast/dbg_expr.rs` (`TraceEvent`, `Decision`, `trace`/`drain_trace` retyped)
- Modify: `src/ast/type_env.rs` (instantiate's trace block builds a `TraceEvent` with the `site` span)
- Modify: `src/ast/assignability.rs` (`trace_conditional` builds a `TraceEvent`; `decision_label` → `decision_of`)
- Modify: `src/report.rs` (`render_trace_line` + colour constants)
- Modify: `src/test_harness.rs` (`Config.color`; flush renders `TraceEvent`s and threads colour into `render_debug`)
- Modify: `src/main.rs` (`color: true`)
- Test: `src/report.rs` and `src/test_harness.rs` inline tests (two existing trace tests updated)

**Interfaces:**
- Consumes: Task 1's `line_col`; Task 2's `site: Span` parameter on `instantiate`.
- Produces (in `src/ast/dbg_expr.rs`):
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Decision { Then, Else, Never, Both }

  #[derive(Debug, Clone)]
  pub struct TraceEvent {
      pub span: Span,
      pub message: String,
      pub decision: Option<Decision>,
  }
  ```
  `DbgSink::trace(&self, event: TraceEvent)`, `drain_trace() -> Vec<TraceEvent>`; and in `src/report.rs`:
  ```rust
  pub fn render_trace_line(
      source_name: &str,
      source: &str,
      span: Span,
      message: &str,
      decision: Option<Decision>,
      color: bool,
  ) -> String
  ```

- [ ] **Step 1: Write the failing tests**

In `src/report.rs`'s `mod tests` (add `use crate::ast::dbg_expr::Decision;` inside the module):

```rust
#[test]
fn trace_line_plain_when_colour_off() {
    let out = render_trace_line(
        "x.nt",
        "type A as 1\n",
        Span::new(0, 4),
        "1 <: number",
        Some(Decision::Then),
        false,
    );
    assert_eq!(out, "[Trace] x.nt:1:1  1 <: number → then");
    assert!(!out.contains('\x1b'));
}

#[test]
fn trace_line_colours_decision_word_when_colour_on() {
    for (decision, expected) in [
        (Decision::Then, "\x1b[32mthen\x1b[0m"),
        (Decision::Else, "\x1b[31melse\x1b[0m"),
        (Decision::Never, "\x1b[35mnever\x1b[0m"),
        (Decision::Both, "\x1b[33mboth (indeterminate)\x1b[0m"),
    ] {
        let out = render_trace_line(
            "x.nt",
            "type A as 1\n",
            Span::new(0, 4),
            "1 <: number",
            Some(decision),
            true,
        );
        assert!(out.contains(expected), "{out:?}");
        assert!(out.contains("\x1b[36m[Trace]\x1b[0m"), "{out:?}");
    }
}

#[test]
fn trace_line_without_decision_has_no_arrow() {
    let out = render_trace_line(
        "x.nt",
        "type A as 1\ntype B as 2\n",
        Span::new(12, 16),
        "Id(1) = 1",
        None,
        false,
    );
    assert_eq!(out, "[Trace] x.nt:2:1  Id(1) = 1");
}
```

In `src/test_harness.rs`, update the two existing trace tests to the new shape:

In `trace_eval_reports_instantiation_and_conditional_lines`, replace the assertions from `let trace_lines: Vec<&str> = …` down with:

```rust
        let trace_lines: Vec<&str> = out.lines().filter(|l| l.starts_with("[Trace]")).collect();
        assert!(
            trace_lines
                .iter()
                .any(|l| l.contains("Id(1)") && l.contains("<test>:4:")),
            "expected a source-anchored instantiate trace line for Id(1): {out}"
        );
        assert!(
            trace_lines
                .iter()
                .any(|l| l.contains("<:") && l.contains("→ then")),
            "expected a conditional-decision trace line with its branch: {out}"
        );
        assert!(
            !out.lines().any(|l| l.starts_with("trace: ")),
            "old plain trace prefix must be gone: {out}"
        );
```

(The source in that test has `assert Id(1) <: number` on line 4 — the instantiation site span is the claim's application node.)

In `trace_eval_off_by_default_produces_no_trace_lines`, replace the last assertion with:

```rust
        assert!(!out.lines().any(|l| l.starts_with("[Trace]")), "{out}");
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run trace_line_; cargo nextest run trace_eval_`
Expected: COMPILE ERROR (`render_trace_line`/`Decision` not found), and the updated harness tests fail.

- [ ] **Step 3: Retype the sink's trace channel in `dbg_expr.rs`**

Add `Decision` and `TraceEvent` (as in Interfaces above) near `Frame`. Change `DbgSink`:

```rust
    trace_events: RefCell<Vec<TraceEvent>>,
```

```rust
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
```

- [ ] **Step 4: Producers**

`src/ast/type_env.rs` — in `instantiate`, the trace block becomes (import `TraceEvent` in the existing `dbg_expr` use item):

```rust
        // `--trace-eval`: one event per cache miss (i.e. per distinct
        // instantiation, matching the cache's own granularity), anchored at
        // the call site.
        if let Some(sink) = self.dbg() {
            if sink.trace_enabled() {
                let args_src = args
                    .iter()
                    .map(one_line)
                    .collect::<Vec<_>>()
                    .join(", ");
                sink.trace(TraceEvent {
                    span: site,
                    message: format!("{name}({args_src}) = {}", one_line(&body)),
                    decision: None,
                });
            }
        }
```

`src/ast/assignability.rs` — import `Decision` and `TraceEvent` from `crate::ast::dbg_expr`. Replace `decision_label` with:

```rust
    /// The `--trace-eval` decision for a definite/indeterminate relation
    /// result, matching the branch [`Self::reduce_conditional`] selects.
    fn decision_of(result: ExtendsResult) -> Decision {
        match result {
            ExtendsResult::True => Decision::Then,
            ExtendsResult::False => Decision::Else,
            ExtendsResult::Never => Decision::Never,
            ExtendsResult::Both => Decision::Both,
        }
    }
```

Retype `trace_conditional` (span = the conditional expression's span, threaded from `reduce_conditional`'s `span` binding):

```rust
    /// `--trace-eval`: one event per conditional decision, anchored at the
    /// conditional's span and carrying which branch was taken. Read-only —
    /// only appends to the sink's log when trace mode is on and the sink
    /// isn't paused (see `DbgSink::trace`); never affects the branch
    /// selected above.
    fn trace_conditional(
        ctx: &ResolveCtx,
        span: Span,
        check: &Ast,
        extends: &Ast,
        decision: Decision,
    ) {
        let Some(sink) = ctx.env().and_then(|env| env.dbg()) else {
            return;
        };
        if !sink.trace_enabled() {
            return;
        }
        sink.trace(TraceEvent {
            span,
            message: format!("{} <: {}", one_line(check), one_line(extends)),
            decision: Some(decision),
        });
    }
```

Update its two call sites in `reduce_conditional`:

```rust
            Self::trace_conditional(
                ctx,
                *span,
                &check,
                rhs,
                if matched { Decision::Then } else { Decision::Else },
            );
```

```rust
        Self::trace_conditional(ctx, *span, &check, rhs, Self::decision_of(result));
```

(`Span` needs to be in `assignability.rs`'s imports if not already.)

- [ ] **Step 5: Renderer in `report.rs`**

```rust
use crate::ast::dbg_expr::Decision;

/// ANSI codes for the hand-formatted `[Trace]` line. ariadne cannot emit a
/// label-free one-liner, so the compact trace line is formatted here in
/// ariadne's visual style; explicit codes (rather than ariadne's
/// concolor-gated painting) keep output deterministic under the `color`
/// switch.
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_RESET: &str = "\x1b[0m";

/// The decision word and its colour for a conditional-trace line: which
/// branch the conditional took.
fn decision_word(decision: Decision) -> (&'static str, &'static str) {
    match decision {
        Decision::Then => ("then", "\x1b[32m"),
        Decision::Else => ("else", "\x1b[31m"),
        Decision::Never => ("never", "\x1b[35m"),
        Decision::Both => ("both (indeterminate)", "\x1b[33m"),
    }
}

/// Render one `--trace-eval` event as a compact source-anchored line:
/// `[Trace] file:line:col  message[ → decision]`. With `color` on, the kind
/// word is cyan and the decision word takes its branch colour.
pub fn render_trace_line(
    source_name: &str,
    source: &str,
    span: Span,
    message: &str,
    decision: Option<Decision>,
    color: bool,
) -> String {
    let (line, col) = line_col(source, span.start);
    let kind = if color {
        format!("{ANSI_CYAN}[Trace]{ANSI_RESET}")
    } else {
        "[Trace]".to_string()
    };
    let mut out = format!("{kind} {source_name}:{line}:{col}  {message}");
    if let Some(decision) = decision {
        let (word, code) = decision_word(decision);
        if color {
            out.push_str(&format!(" → {code}{word}{ANSI_RESET}"));
        } else {
            out.push_str(&format!(" → {word}"));
        }
    }
    out
}
```

- [ ] **Step 6: Harness flush + CLI colour**

`src/test_harness.rs` — add to `Config`:

```rust
    /// Colour the flush output (`Debug` reports, `[Trace]` lines, decision
    /// words). The CLI sets this for stderr; tests leave it off so output
    /// stays ANSI-free and assertable.
    pub color: bool,
```

Thread it into the flush: change `flush_dbg_events`'s signature to take `color: bool` (pass `config.color` at all three call sites), replace the trace-line loop, and un-hardcode `render_debug`'s colour:

```rust
    let trace_events = sink.drain_trace();
    let _guard = sink.pause();
    for event in trace_events {
        writeln!(
            out,
            "{}",
            crate::report::render_trace_line(
                source_name,
                source,
                event.span,
                &event.message,
                event.decision,
                color,
            )
        )?;
    }
```

and in the events loop pass `color` as `render_debug`'s final argument (was `false`).

`src/main.rs` — add `color: true,` to the `test_harness::Config { … }` literal (matching the CLI's existing always-coloured stderr diagnostics, e.g. the `render_labeled(…, true)` call above it).

- [ ] **Step 7: Run tests, then the whole suite**

Run: `cargo nextest run trace_line_; cargo nextest run trace_eval_; cargo nextest run`
Expected: all PASS. Existing dbg tests are unaffected (they run with `color` defaulting to `false`).

- [ ] **Step 8: Commit**

```bash
cargo fmt
jj commit -m "Render --trace-eval as source-anchored [Trace] lines with coloured branch decisions" src/ast/dbg_expr.rs src/ast/type_env.rs src/ast/assignability.rs src/report.rs src/test_harness.rs src/main.rs
```

---

### Task 4: Integration verification

**Files:** none created or modified (fixes only if a gate fails).

**Interfaces:**
- Consumes: everything from Tasks 1–3.
- Produces: a verified branch — full suite, conformance oracle, and a manual smoke of the end-to-end output.

- [ ] **Step 1: Full Rust suite**

Run: `cargo nextest run`
Expected: all tests pass, including `tests/dbg_eval.rs` (the observer-effect differential over every `tests/conformance/*.nt`).

- [ ] **Step 2: Conformance oracle**

Run: `mise run tc`
Expected: zero `DISAGREE` rows (frames and trace events are observation-only; any divergence is a bug — stop and investigate rather than proceeding).

- [ ] **Step 3: End-to-end smoke**

Run: `cargo build && ./target/debug/newtype --trace-eval --input examples/ts-toolbelt.nt > /dev/null; echo "exit: $?"`
Expected on stderr: `[Trace] examples/ts-toolbelt.nt:<line>:<col>  …` lines (cyan `[Trace]`, coloured decision words); each `[Debug]` report followed by indented `in …   examples/ts-toolbelt.nt:<line>:<col>` frame lines ending at `in assert claim`. (`examples/ts-toolbelt.nt` carries local `dbg!` experiments — do NOT commit it.)

- [ ] **Step 4: Report**

No commit (nothing changed). Report the three gate results verbatim.
