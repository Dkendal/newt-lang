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

Two cooperating pieces:

1. **Global sugar desugaring (new, pre-resolution).** Some built-in
   TypeScript types are alternate spellings of forms the engine already
   understands. These are rewritten bottom-up across the whole tree —
   including `unittest` bodies, `assert` claims, and `interface` definitions —
   *before* any identifier resolution, so they are never treated as type
   references at all:

   - `Array(T)` → `T[]`
   - `ReadonlyArray(T)` → `readonly T[]`
   - `Readonly(T)` → `readonly T`, only when `T` is a tuple or array type
     (TypeScript's mapped-type `Readonly<T>` over objects is not implemented)
   - `keyof any` → `string | number | symbol`

   The rewrite runs at the front of `simplify()` (so evaluation and rendering
   both see the core forms) and again before the warning pass in the CLI.
   Leftover spellings the rewrite doesn't cover — `Array` with the wrong
   arity, a bare `Array` ident, `Readonly` of a non-tuple/array — fall
   through to the warning pass as unresolved references.

   Rendering consequence: `Array(x)` now renders as `x[]` rather than
   `Array<x>` (equivalent TypeScript); the one corpus fixture asserting the
   old spelling is updated.

2. **A static whole-program warning pass** runs after parsing and desugaring
   (before `simplify`) and reports every type reference that cannot resolve
   to a definition in the file.

A reference is a bare `Ident` in type position or the `Ident` head of an
`ApplyGeneric`. It resolves if it names:

- a top-level `type` alias, `interface`, or `unique symbol` declaration (the
  same set `TypeEnv::from_program` registers),
- a name brought in by an `import` statement — a named specifier's local name
  (the alias if present, otherwise the exported name) or a namespace import's
  alias. This exemption is **temporary**: real module loading is a planned
  follow-up project (see Future work), after which an import only resolves if
  its module is actually found and loaded, and its types evaluate concretely.
  Until then imported names do not warn, but evaluation still treats them as
  indeterminate,
- a name the assignability engine understands semantically without a
  definition: the `Object` and `Function` interfaces and the object wrappers
  `Boolean`, `Number`, `String`, `Symbol`, `BigInt` (all special-cased in the
  engine; `assert () => void <: Function` genuinely evaluates today) — or
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

- **Other TypeScript built-ins.** Beyond the desugared aliases and the
  engine-known names above, there is no allowlist: `Record`, `Pick`,
  `Promise`, etc. all warn until defined in (or imported into) the file. (A
  future "include built-in types" feature may change this; out of scope
  here.)

Scanned positions: type alias bodies, parameter defaults and `where`
constraint bounds, interface bodies and `extends` clauses, and `assert` claims
inside `unittest` blocks.

## Output

One ariadne report per **distinct unresolved name**, `ReportKind::Warning`,
with one label per use site, written to stderr before the unittest report:

```
Warning: cannot resolve type `Pick`
   ╭─[ example.nt:41:9 ]
   │
41 │         Pick(User, 'id')
   │         ────┬───
   │             ╰───── cannot be resolved to a definition
```

The label says "cannot be resolved to a definition" rather than "not defined
in this file" — resolution will not stay file-local once module loading lands.

Reports are ordered by the first use site's source position (deterministic).

## Severity and exit code

- Default: warnings only. Exit code is unchanged — only assert failures (and
  existing hard errors) fail the run. TypeScript output is still rendered.
- New clap flag `--deny-unresolved` on the CLI: the same diagnostics render as
  `ReportKind::Error` and any unresolved reference makes the process exit
  nonzero. Rendering and assert evaluation still proceed (mirroring assert
  failures).

## Implementation shape

- **New module `src/ast/desugar.rs`**: `Ast::desugar_globals(&self) -> Ast`,
  an explicit bottom-up rewrite modeled on `rewrite_unique_symbols` (with its
  own `UnitTest`/`Assert`/`Interface` arms, since `Ast::map` does not recurse
  into those). Called at the front of `Ast::simplify` and by the CLI before
  the warning pass.
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
  interfaces, unique symbols), plus the local names bound by each
  `ImportStatement` (named specifiers' aliases/exported names, namespace
  aliases). Duplication is acceptable at this size; if it drifts, extract a
  shared helper.
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
  forms, mapped-type index, `let`, `match` binders); imported names do not
  warn (named, aliased, and namespace imports); shadowing resolves;
  grouping/ordering of multiple use sites.
- **CLI/harness-level test**: a program with `ReadonlyArray` used but
  undefined produces a warning on stderr and exit code 0; with
  `--deny-unresolved`, nonzero.
- **Desugaring unit tests**: each alias rewrites (in alias bodies, interface
  definitions, and assert claims); `Readonly` of a non-tuple/array and
  wrong-arity `Array` are left alone; `keyof any` becomes
  `string | number | symbol`. The `params_in_application` corpus fixture's
  expected output changes from `Array<x>` to `x[]`.
- **End-to-end motivating case**: `examples/ts-toolbelt.nt` — `ReadonlyArray(A)`
  desugars to `readonly A[]`, so `List` resolves, the `At(User, 'id') <: number`
  assert evaluates definitively (expected: passes), and no warning is
  emitted. Stale TODO comments in the example are updated.
- Conformance harness (`mise run tc`) re-run to confirm the desugared forms
  (e.g. `Array(?U)` patterns becoming `(?U)[]`) still agree with tsgo.

## `--exact-optional-property-types` flag

A second CLI flag mirroring TypeScript's `exactOptionalPropertyTypes`. The
engine's default already matches tsgo `--strict` without that option: an
optional target property `x?: T` is widened to `T | undefined`
(`src/ast/assignability.rs`, `property_relation`), so `{x: T | undefined}`
and `{x: undefined}` sources are accepted. With
`--exact-optional-property-types`, that widening is disabled: the source
property's type must be assignable to `T` itself.

Unaffected in both modes (already TS-accurate): an optional *source*
property is never assignable to a required target property (so
`{x?: T} <: {x: T | undefined}` is false regardless — the "equivalence" is
one-directional in TypeScript), and a target property missing from the
source is fine when optional.

Plumbing: the flag threads from the CLI through `test_harness::Config` into
`ResolveCtx` (a new `exact_optional_property_types: bool`, default false),
which `property_relation` consults. Conformance is unaffected: the default
matches tsgo `--strict` (which does not include `exactOptionalPropertyTypes`).

## Future work (separate spec)

**Real module loading.** Imports should be fully resolved: locating the
imported `.nt` module, parsing it, and registering its exports (transitively,
with cycle handling) into the `TypeEnv`, so an imported generic type expands
to its concrete definition and evaluates in asserts. Once that lands, this
pass tightens: an imported name resolves only if the loader actually found
the module and the module exports it — a failed import warns like any other
unresolved reference, and the temporary blanket import exemption above is
removed.
