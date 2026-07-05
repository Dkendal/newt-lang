# Unresolved type-reference warnings

Date: 2026-07-05
Status: approved design, pending implementation plan

## Problem

A reference to an undefined type (e.g. `ReadonlyArray` in
`examples/ts-toolbelt.nt`, whose definition is commented out) is treated by the
assignability engine as a free type variable, so relations involving it
evaluate to `Both` (indeterminate). The only surface today is the generic
per-assert failure message ("evaluated to indeterminate…"), which does not name
the unresolved reference or point at its use site. `examples/ts-toolbelt.nt`
carries a `TODO raise on unresolved types` for exactly this.

## Behavior

A **static whole-program pass** runs after parsing (on the pre-`simplify` AST)
and reports every type reference that cannot resolve to a definition in the
file.

A reference is a bare `Ident` in type position or the `Ident` head of an
`ApplyGeneric`. It resolves if it names:

- a top-level `type` alias, `interface`, or `unique symbol` declaration (the
  same set `TypeEnv::from_program` registers), or
- a name bound in the current lexical scope:
  - type parameters of the enclosing `type`/`interface` (in scope for `where`
    constraints, parameter defaults, and the body),
  - `infer X` bindings (in both sugar `if`/`cond`/`match` conditions and the
    core `ExtendsExpr` form), in scope for the branch they guard,
  - the index variable of a mapped type (`map K in … do … end`), including its
    `as`-remap clause and body,
  - `let` bindings, in scope for the `let` body,
  - `match` arm pattern binders, in scope for that arm's body.

Deliberately **not** exempt:

- **Imported names.** An `import` does not resolve a name — imports must be
  fully resolved within the file to count, so uses of imported types warn like
  any other unresolved reference.
- **TypeScript built-ins.** No allowlist: `ReadonlyArray`, `Array`, `Record`,
  etc. all warn until defined in the file. (A future "include built-in types"
  feature may change this; out of scope here.)

Scanned positions: type alias bodies, parameter defaults and `where`
constraint bounds, interface bodies and `extends` clauses, and `assert` claims
inside `unittest` blocks.

## Output

One ariadne report per **distinct unresolved name**, `ReportKind::Warning`,
with one label per use site, written to stderr before the unittest report:

```
Warning: cannot resolve type `ReadonlyArray`
   ╭─[ examples/ts-toolbelt.nt:41:9 ]
   │
41 │         ReadonlyArray(A)
   │         ──────┬──────
   │               ╰──────── not defined in this file
```

Reports are ordered by the first use site's source position (deterministic).

## Severity and exit code

- Default: warnings only. Exit code is unchanged — only assert failures (and
  existing hard errors) fail the run. TypeScript output is still rendered.
- New clap flag `--deny-unresolved` on the CLI: the same diagnostics render as
  `ReportKind::Error` and any unresolved reference makes the process exit
  nonzero. Rendering and assert evaluation still proceed (mirroring assert
  failures).

## Implementation shape

- **New module `src/ast/unresolved.rs`** exporting something like
  `pub fn unresolved_references(program: &Ast) -> Vec<UnresolvedRef>` where
  `UnresolvedRef { name: String, spans: Vec<Span> }` (grouped by name, spans in
  source order).
- The walk is an **explicit recursive visitor with a scope stack**
  (`Vec<HashSet<String>>` or equivalent) — not `prewalk`/`Ast::map` — because
  binders scope over specific children (a type parameter is in scope for its
  own constraint and later parameters' defaults), and `walk.rs` coverage gaps
  have historically caused silent bugs. The visitor matches on every `Ast`
  variant; unknown/uninteresting variants recurse through children generically
  where possible, and the module carries a test that exercises each binder
  form.
- Top-level definition collection mirrors `TypeEnv::from_program` (idents,
  interfaces, unique symbols). Duplication is acceptable at this size; if it
  drifts, extract a shared helper.
- **`src/report.rs`** gains warning support: `build_report`/`render`/`eprint`
  take a `ReportKind` (or grow `_warning` variants), keeping the existing
  error helpers' signatures for current callers.
- **`src/main.rs`**: after parse (before `simplify`), run the pass, render
  each report to stderr, and honor `--deny-unresolved` in the exit-code
  decision.

## Error handling

- The pass is purely additive: it never blocks evaluation or rendering.
- Synthesized/out-of-range spans are already clamped by `report::clamp`.
- Name shadowing (a type parameter shadowing a top-level def) resolves to the
  innermost binding, which is a resolution either way — no warning.

## Testing

- **Unit tests in `src/ast/unresolved.rs`**: an undefined name warns (bare and
  as generic head, and in generic argument position); each binder form does
  not warn (type params incl. `where`/defaults, `infer` in sugar and core
  forms, mapped-type index, `let`, `match` binders); imported names warn;
  shadowing resolves; grouping/ordering of multiple use sites.
- **CLI/harness-level test**: a program with `ReadonlyArray` used but
  undefined produces a warning on stderr and exit code 0; with
  `--deny-unresolved`, nonzero.
- **End-to-end motivating case**: `examples/ts-toolbelt.nt` warns on
  `ReadonlyArray` only.
- Conformance harness (`mise run tc`) is unaffected: warnings do not change
  evaluation or the generated TypeScript.
