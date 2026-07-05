# `dbg!` macro — design

**Date:** 2026-07-05
**Status:** approved

## Summary

Add a compile-time `dbg!` macro to the `.nt` language, modelled after Elixir's
`dbg`. `dbg!(Expr)` prints an ariadne report (`ReportKind::Custom("Debug")`)
showing the source expression and its **fully normalized** type, then behaves
as the identity: asserts, rendered TypeScript, test codegen, and the source map
are unaffected. At the end of (or inside) a pipeline, `a |> b |> c |> dbg!()`
reports **each pipeline step** — one Debug report per step, innermost first —
as if every step had been wrapped in `dbg!`.

## Language surface

- `dbg!(Expr)` anywhere a type expression is allowed (type alias bodies,
  assert claims).
- `... |> dbg!()` anywhere in a pipeline, including mid-pipeline
  (`a |> dbg!() |> b`). Pipe semantics prepend the LHS as the first argument,
  mirroring the existing `ApplyGeneric` rule.
- `dbg!` is erased before rendering: `dbg!(X)` emits exactly what `X` emits.

## Output format

One report per step (Elixir order — first step first):

```
[Debug] foo.nt:12:11
  <underlined excerpt of `A`>
  = { a: 1, b: 2 }

[Debug] foo.nt:12:11
  <underlined excerpt of `A |> Partial`>
  = { a?: 1, b?: 2 }

[Debug] foo.nt:12:11
  <underlined excerpt of `A |> Partial |> Keys`>
  = "a" | "b"
```

Each report is anchored at the step's span; the label message is
`= <pretty-printed normalized type>` (rendered via the existing TypeScript
pretty-printer). A non-pipeline `dbg!(X)` is a single such report. Reports go
to a caller-supplied `&mut dyn Write` (stderr in the CLI), like the assert
harness.

## Pipeline position

The pass runs in `main.rs` **after `simplify()` and before
`test_harness::run`**:

```
parse → validate → desugar_globals → unresolved-refs warnings
      → simplify
      → dbg! pass   ← new
      → test_harness::run → render TS / test codegen → source map
```

- After `simplify()`: sugar (`if`/`cond`/`match`/`let`) is gone, so what the
  pass evaluates matches what the assert harness would see. Original spans
  survive simplification, so reports point at the source as written.
- Before the harness/renderer: the pass erases `MacroCall` nodes, which
  nothing downstream handles (`typescript.rs` treats a surviving `MacroCall`
  as unreachable).

## Components

### 1. Parser (`src/parser.rs`)

`pipe_to_application` gains a `MacroCall` arm: `lhs |> dbg!(...)` prepends
`lhs` to the macro's args. To let the debug pass recover pipeline steps,
`ApplyGeneric` gains a `from_pipe: bool` field, annotated
`#[derivative(PartialEq = "ignore")] #[serde(skip)]` (the same treatment the
`#[ast_node]` macro applies to spans), so node equality, corpus fixtures, and
sexpr serialization are unaffected. It is set `true` only in
`pipe_to_application`; all other construction sites pass `false`, and
`Ast::map` in `src/ast/walk.rs` copies it through. Insta parse-tree snapshots
that include pipes change (field shows in `Debug`); review with
`cargo insta review`.

### 2. Debug pass (new `src/ast/dbg_expr.rs`)

Two phases, because the `TypeEnv` must be built from a program that no longer
contains `MacroCall` nodes:

1. **Strip & collect.** Walk the program; replace each
   `MacroCall { name: "dbg!" }` with its single argument (wrong arity is a
   source-anchored error), collecting a work item per call: the argument AST
   and its step list. Steps are recovered by peeling `from_pipe` applications:
   `dbg!(c(b(a)))` where both applications are pipe-created yields steps `a`,
   `a |> b`, `a |> b |> c`. Peeling stops at any non-`from_pipe` node (so a
   mid-pipeline `dbg!` boundary or a hand-written application ends the chain).
   Step spans are the nodes' own spans — a pipe-created application's span
   already covers the source from the pipeline start through that step.
2. **Evaluate & report.** Build the `TypeEnv` from the cleaned program, then
   for each work item in source order, normalize each step and print its
   Debug report.

Other macros (`assert_equal!`, `unquote!`) are out of scope and keep their
current behavior. The buggy dead `MacroCall::eval` match (strips `!` then
matches names that still contain `!`) is corrected in passing.

### 3. Normalization

`pub(crate) fn normalize(ast, ctx) -> Ast`: a bottom-up fixpoint loop of
`simplify()`, `TypeEnv::resolve_head`, and the existing private reducers in
`src/ast/assignability.rs` (`reduce_conditional`, `reduce_indexed_access`,
keyof reduction), with a depth/iteration cap so recursive aliases terminate.
Reducers get `pub(crate)` visibility; no behavior change to the assignability
engine. A step that stops reducing (unresolvable reference, recursion cap)
prints as far as it got.

### 4. Reporting (`src/report.rs`)

A `render_debug` helper (or `Severity::Debug` variant) mapping to
`ReportKind::Custom("Debug")`, reusing the existing clamp/config plumbing.

## Error handling

- `dbg!` with zero args outside a pipeline, or more than one arg: a
  source-anchored error report (consistent with existing semantic panics
  recovered by the panic hook).
- Unknown macros keep today's behavior.
- Normalization never fails: it stops reducing and prints the partial form.

## Testing

- Unit tests for the debug pass in `src/ast/dbg_expr.rs` (capture the writer;
  assert report contents and that the returned AST equals the program with
  `dbg!` erased) — including: plain `dbg!(X)`, full-pipeline
  `a |> b |> dbg!()`, mid-pipeline `a |> dbg!() |> b`, `dbg!` around a
  hand-written application (single step), and alias normalization
  (`dbg!(Partial(User))` prints the expanded object type).
- Parser: insta snapshot for `|> dbg!()`; corpus TS fixture confirming
  `dbg!(X)` renders as `X`.
- Conformance (`mise run tc`) unaffected by construction; run it to confirm.
