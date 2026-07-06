# `dbg!` v2 — evaluation-site semantics via span watchpoints

**Date:** 2026-07-05
**Status:** approved
**Supersedes:** the *semantics* of `2026-07-05-dbg-macro-design.md` (v1). The v1
parser work, erasure pass, `render_debug`, and `Ast::normalize` are retained.

## Motivation

v1 `dbg!` is definition-site and static: it normalizes the marked expression
*as written*, so inside a generic body the parameters print unbound
(`dbg!(K)` prints `= K`), and since nothing is evaluated at the definition
site, `dbg!`s in *every* conditional branch print — including branches a real
evaluation never takes. The intended semantics (per Elixir/Haskell `trace`)
are evaluation-site: fire when evaluation actually reaches the mark, with the
bindings live at that moment.

## Semantics

A `dbg!(X)` fires when the type engine **demands the structure of `X` during
evaluation** — i.e. whenever:

- a generic type containing the mark is instantiated to a concrete type and
  the marked node is demanded (parameter substitution counts as demand of a
  bare-parameter mark), or
- the assignability engine walks through the marked node while deciding a
  relation or reducing a conditional / indexed access / `keyof`.

Consequences (all deliberate):

- **Live bindings.** Inside `At(User, 'id')`'s evaluation, `dbg!(K)` prints
  `= 'id'`, and `dbg!(A[K])` prints the reduced property type.
- **Dead branches are silent.** `reduce_conditional` picks a branch before
  walking it; marks in the dropped branch are never demanded.
- **Indeterminate conditionals fire both branches** (the result is their
  union — both were genuinely evaluated).
- **Unused is silent.** A mark in a definition nothing instantiates or
  relates never prints. `let` stays lazily evaluated: `let _ = dbg!(K) in B`
  drops the unused binding at desugaring and never fires.
- **Once per distinct instantiation.** Events dedupe on
  `(watch span, fingerprint(observed value))`: `At(User,'id')` and
  `At(User,'name')` each print once; re-evaluating `At(User,'id')` does not
  reprint.
- **No observer effect, by construction.** The evaluated AST is byte-identical
  to the erased program (v1's strip runs unchanged); marks exist only as a
  side table of spans. A missed hook under-reports (a debug line doesn't
  print); it can never change an evaluation result.

## Architecture (the Prolog spy-point / GDB breakpoint shape)

The identity channel is the **span**: `Ast::map`-based substitution preserves
the spans of rebuilt definition-body nodes, so a watched node keeps its
definition-site span through instantiation.

1. **Strip & watch (extends the v1 pass, `src/ast/dbg_expr.rs`).** Runs where
   it runs today (after `desugar_globals`, before `simplify()`), erases each
   `dbg!(X)` to `X` exactly as now — but instead of reporting, it returns a
   **watch table**: the set of watched spans (the marked expression's span,
   plus each pipeline step's span recovered by `from_pipe` peeling, plus a
   `bare_param: Option<String>` note when the marked node is a bare
   identifier).

2. **Sink & context.** `ResolveCtx` gains two optional fields (both default
   off, so nothing changes for existing construction sites): a shared
   `&DbgWatches` (the table) and a shared event sink
   (`RefCell<Vec<DbgEvent>>`, `DbgEvent = { span: Span, observed: Ast }`).
   Events dedupe at insert on `(span, type_env::fingerprint(&observed))`.

3. **Demand hooks (read-only, one line each).** At each hook: *if this node's
   span is in the watch table, record `(span, node.clone())`*. Hook sites:
   - entry of `is_assignable_to_ctx` (both operands);
   - the operand-resolution points in `reduce_conditional`,
     `reduce_access_leaf`/`index_type`, and `eval_keyof`;
   - the substitution walk (`distribute_or_substitute`): replacing a
     parameter reference whose span is watched records the *argument* —
     this covers `dbg!(K)` on a bare parameter, whose node (and span) is
     replaced wholesale by substitution.
   Hooks observe and pass through; they never unwrap, rewrite, or return
   differently.

4. **Flush & render.** The assert harness flushes the sink after each claim:
   each event's `observed` is normalized (`Ast::normalize`, v1) and printed
   with `render_debug` (v1) anchored at the watch span — `= 'id'` style.
   `main.rs` threads table + sink into the harness's `ResolveCtx`; a final
   flush after the harness catches events from the last claim. Programs whose
   marks are never demanded print nothing.

## Complement: `--trace-eval` (checker-owned tracing)

A CLI flag (GHC `-ddump-tc-trace` / `tsc --extendedDiagnostics` shape) that
reuses the same sink plumbing but ignores the watch table — global tracing of
the engine's own steps, no `dbg!` marks required:

- each `TypeEnv::instantiate` cache miss: `trace: Name(args…)` and the
  substituted body (one line, pretty-printed, truncated to the render width);
- each `reduce_conditional` decision: the condition, and which branch was
  taken (`then` / `else` / `never` / `both (indeterminate)`).

Plain `trace: …` lines on stderr (not ariadne reports). Off by default;
independent of `dbg!` (both can be on at once).

## What changes from v1, concretely

- `expand` no longer normalizes or prints; it returns `(cleaned, DbgWatches)`.
  Its internal "build env from `cleaned.simplify()` and report" phase is
  deleted.
- The v1 tests asserting definition-site reports are rewritten: output now
  requires an evaluation driver (a `unittest` assert exercising the mark).
  Erasure-identity tests stay as-is.
- `test_harness::run` gains the watch table/sink wiring (a parameter or a
  small options struct) and the per-claim flush.
- Everything else from v1 stands: parser (`from_pipe`, pipe-into-macro),
  `render_debug`, `Ast::normalize`, `MacroCall` map arm, strip-before-simplify
  ordering, interface/let walker coverage.

## Testing

- **End-to-end (the motivating case):** a program shaped like the
  ts-toolbelt `At` example — assert `At(User, 'id') <: number` — must print
  `= 'id'` for `dbg!(K)` and the reduced type for the taken branch, and
  print *nothing* for marks in untaken branches. A mark in a definition no
  assert exercises prints nothing.
- **Per-hook unit tests:** one focused test per demand hook (relation
  operand, conditional check, indexed access, keyof, bare-parameter
  substitution).
- **Dedupe:** two asserts driving the same instantiation print once; two
  different instantiations print twice.
- **No-observer-effect (should now be a formality):** the assert outcomes of
  every `tests/conformance/*.nt` file are identical with and without the
  feature code paths active (the evaluated AST is identical by construction;
  the test guards the hooks' read-only discipline).
- **`--trace-eval`:** smoke test that instantiation and conditional-decision
  lines appear and the flag defaults off.
- Full suite (`cargo nextest run`) and conformance (`mise run tc`) stay green.
