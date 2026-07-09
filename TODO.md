# TODO

Open issues discovered while building the corpus test suites. Each item notes
its source location and the test that will go green once it's addressed.

Pending specs live in `tests/pending.rs` (all `#[ignore]`d). Run them with:

```sh
cargo test --test pending -- --ignored
```

When you fix an item, its test should pass — then delete the `#[ignore]` or
promote the case into `tests/corpus/`.

---

## Conformance audit (2026-06-30): newtype vs tsgo disagreements

Cross-checked every type-system domain against `tsgo --strict` via
`scripts/conformance.py`. Found 7 soundness bugs (newtype wrongly *accepts* an
unsound relation), 12 wrong-rejects (newtype returns a definite `false` on an
in-scope relation tsgo accepts), and a set of feature/parser gaps (newtype
returns *indeterminate*). Each repro below is what tsgo reports as **true**.
`[x]` = fixed and verified against the harness; `[ ]` = deferred.

### Soundness bugs (newtype wrongly accepts)

- [x] **B1. Generic-alias substitution skips function types.** `Ast::map`
  (`src/ast/walk.rs`) had no `FunctionType` arm, so `substitute` never replaced a
  type parameter inside a function type in an alias body. `type Fn(T) as (x: T)
  => void` made `Fn(string) <: Fn(number)` wrongly hold (free `T` on both
  sides). Affects param/return/nested-in-object/nested-in-tuple positions.
- [x] **B2. Weak-type rule unmodeled.** An all-optional target with no required
  member, index, or call signature must reject a source sharing *no* property
  (`{b: string} <: {a?: number}` is false in TS). newtype accepts it, and the
  hole propagates through array/tuple/return/param/interface positions.
- [x] **B3. `where` constraints parsed but never enforced.** `type Num(T) where
  T <: number as T` accepts `Num('x')` and evaluates the body; tsgo emits
  TS2344. (Manifests as both-fail in the harness since the bad application also
  fails to type-check on the TS side.)
- [x] **B4. Rest-vs-rest param element compared covariantly.** `(...a: 1[]) =>
  void <: (...a: number[]) => void` wrongly holds — two pure-rest signatures have
  no fixed params, so the contravariant per-position loop never runs and the rest
  elements are never related.
- [x] **B5. Homomorphic mapped type drops the source optional modifier.** `map k
  in keyof(P) do P[k] end` over `P = {a?: number, b: string}` rebuilds `a` as
  *required*, so it wrongly satisfies `{a: number}`. (readonly is preserved;
  optional is not.)
- [x] **B6. `boolean` not distributed as `true | false`.** A naked type
  parameter conditional over `boolean` should distribute; `type BoolDist(T) as
  if T <: true then 1 else 2 end` gives `1 | 2`, but newtype returns just `2`.
- [x] **B7. Contravariant `infer` candidates unioned, not intersected.** Two
  `infer A` in parameter positions should combine by intersection. FIXED:
  `match_infer` (`src/ast/assignability.rs`) now tracks variance polarity
  (flipping in function-parameter positions) and `combine_candidates` unions
  covariant candidates (which take priority, as in tsc) but intersects
  all-contravariant ones. _Tests:_ `tests/conformance/conditionals_extra.nt`
  ("contravariant infer candidates intersect").

### Wrong-rejects (newtype returns a definite `false`)

- [x] **B8. `{}` / `Object` reject bare primitives.** `string <: {}` is true in
  TS (`{}` = any non-nullish value), as is `object <: {}`. newtype returns false.
  Also blocks `string <: string & {}` and makes `not(string <: {})` pass
  unsoundly. (`object` primitive must still reject primitives.)
- [x] **B9. Intersections of 3+ members not flattened.** `{a:1} & {b:2} & {c:3}
  <: {a:1, c:3}` fails — the nested intersection is merged per-group, not over the
  flattened member set. Same cause: `string & number & boolean <: never` fails.
- [x] **B10. null/undefined/void not disjoint in intersections.** `null & string
  <: never`, `undefined & string`, `null & undefined` all fail (TS reduces them
  to `never` under strict). `void & undefined == undefined` must stay.
- [x] **B11. Template-literal pattern matching.** Concrete-literal-vs-pattern,
  template-to-template, and all-concrete collapse are all modelled now:
  `template_subsumes` decides pattern-vs-pattern language inclusion (backtracking
  at character granularity over what each target hole absorbs), a hole-free
  template (`` `abc` ``, `` `a${'b'}c` ``) collapses to its string literal
  before relating, and `parse_template` treats concrete literal placeholders
  (`${'b'}`, `${1}`, `${true}`) as fixed runs. A template with an open hole is
  definitively not assignable to a single string literal. _Tests:_
  `tests/conformance/literals_extra.nt` (template pattern / collapse blocks).
- [x] **B12. `unknown` not absorbed into a union.** `unknown <: number |
  unknown` and `string | unknown == unknown` return false.
- [x] **B13. Numeric literals compared by surface text.** `1.0 == 1`, `1.50 ==
  1.5`, `0 == -0`, `1_000 == 1000` fail because `TypeNumber.ty` is the raw string.
- [x] **B14. Intersection of two unions not distributed/reduced.** `(1|2|3) &
  (2|3|4) <: 2|3` fails (no `&`-over-`|` distribution with `never` elimination).
- [x] **B15. Object with union-typed property not assignable to a union of
  objects.** `{a: 1 | 2} <: {a: 1} | {a: 2}` is true in TS. FIXED:
  `assignable_to_union` (`src/ast/assignability.rs`) now distributes the
  source's union-typed *discriminant* properties (declared by every target
  member, non-uniform, unit-typed in at least one) and requires every
  constituent to match some member — full cartesian distribution over
  non-discriminant properties is unsound (tsc rejects it too; verified).
  _Tests:_ `tests/conformance/discriminated_unions.nt`.
- [x] **B16. Required `T | undefined` property not assignable to optional
  property.** `{x: number | undefined} <: {x?: number}` is true under strict
  (no `exactOptionalPropertyTypes`); newtype rejects.
- [x] **B17. Shared-key object values not intersected on merge.** `{a: number} &
  {a: string} <: {a: never}` fails — the merge keeps the first `a` instead of
  intersecting the values (and does not recurse).
- [x] **B18. `this` parameters counted toward arity.** `(this: object) => void
  <: () => void` fails — `this` is parsed as a positional param. Needs grammar +
  arity erasure.
- [x] **B19. Arrays/tuples don't expose `length` / numeric index to object
  targets.** `number[] <: { length: number }` fails.

### Feature / parser gaps (newtype returns *indeterminate*; not wrong answers)

- [ ] **G1.** Any relation involving `any` is intentionally indeterminate.
- [x] **G2.** Top-level indexed access. DONE: reduced at both relation operands
  before the structural match (`reduce_access_leaf` → `index_type` in
  `src/ast/assignability.rs`, called at the top of `is_assignable_to_ctx`).
  Covered cases: string-literal keys `T['k']` and nested `T['a']['b']`
  (optional `x?: V` widens to `V | undefined` under tsc `--strict`); tuple
  numeric-literal keys `T[0]`, `T['length']` (literal arity), `T[number]`
  (element union); array `E[]`/`readonly E[]` `T[number]`/`T[0]` → `E`,
  `T['length']` → `number`; union KEYS `T['a'|'b']` (distribute, all-or-nothing);
  union object sides `(X|Y)['k']` (distribute, all-or-nothing); intersection
  object sides `(X&Y)['k']` (property types of the members carrying `k`,
  intersected). The TS renderer now parenthesizes a set-op/function/conditional/
  readonly object side (`access_object_doc` in `src/typescript.rs`). Covered by
  `tests/conformance/indexed_access.nt` and `src/test_harness.rs`.
  `keyof`-driven keys (`T[keyof T]`) are now reduced too — see G3. Out-of-bounds
  tuple indices, negative/fractional tuple indices, and accesses to a key absent
  from a union member are TS *errors*, so they are intentionally left unreduced.
  NOTE: negative/fractional numeric-literal indices on an ARRAY (`E[][-1]`,
  `E[][1.5]`) are NOT errors in tsgo — they yield the element type — so
  `index_array` correctly keeps reducing them (verified against tsgo 7.0).
- [~] **G3.** `keyof` reduction extended (`eval_keyof` in
  `src/ast/assignability.rs`). DONE: `keyof (A | B)` = `keyof A & keyof B` (the
  intersection of the members' key sets); `keyof (A & B)` = `keyof A | keyof B`
  (the union of key sets, for non-collapsing intersections); `keyof unknown` =
  `never`; `keyof never` = `string | number | symbol`; and `keyof`-driven
  indexed-access keys — `T[keyof T]` / `T[keyof U]` reduce the keyof key to its
  literal-key union and distribute through the existing union-key path
  (all-or-nothing). The TS renderer now parenthesizes a low-precedence `keyof`
  operand (`keyof (A | B)` no longer reparses as `(keyof A) | B`). Covered by
  `tests/conformance/keyof.nt` and `src/test_harness.rs`.
  STILL OPEN: `keyof` of primitives/arrays/tuples and object-literal keys that
  are numeric- or symbol-named beyond `keyof_string_keys` — these need
  apparent-member modelling (see G7) and stay `Both`. Also unmodelled: `keyof (A
  & B)` where members SHARE a key name — conflicting value types collapse the
  intersection to `never` in tsgo (making `keyof` = `string | number | symbol`),
  which the engine does not model, so any shared-key intersection stays
  indeterminate (`Both`) instead of reducing (only pairwise-disjoint key sets
  reduce). `keyof any` is desugared upstream.
- [x] **G4.** Builtin `Array(T)` / `ReadonlyArray(T)` equated with `T[]`
  (`src/ast/desugar.rs` desugars the application; verified against tsgo).
- [x] **G5.** The `Array(?U)` / `(?U)[]` infer pattern now matches tuple types:
  a tuple binds the pattern's element to the union of its element types
  (`match_infer` in `src/ast/assignability.rs`). _Tests:_
  `tests/conformance/conditionals_extra.nt` ("Array infer pattern matches
  tuples").
- [x] **G6.** Tuple-typed rest parameter `(...a: [A, B]) => …` expands to
  positional parameters in `split_params` (`src/ast/assignability.rs`).
  _Tests:_ `tests/conformance/functions_extra.nt`.
- [x] **G7.** Primitives' boxed/apparent member set modeled:
  `primitive_apparent_shape` (`src/ast/assignability.rs`) carries a curated
  es2020 member table for `string`/`number`/`boolean`/`bigint`/`symbol`
  (`true <: {valueOf: () => boolean}`, `"x" <: {length: number}`, string's
  numeric index signature, …). A member missing from the table is a definite
  reject (a wrong-reject for the long tail of unmodeled lib methods, never
  unsound). _Tests:_ `tests/conformance/primitives_extra.nt`.
- [x] **G8.** bigint literals (`1n`) parse: the lexer keeps the `n` suffix in
  the numeric literal's raw text, `get_primitive_type` classifies it as
  `bigint` (`1n <: bigint`, `1n </: number`, `1 </: bigint`). _Tests:_
  `tests/conformance/literals_extra.nt`.
- [x] **G9.** Tuple optional / rest / labeled elements (`[A, B?]`,
  `[A, ...B[]]`, `[a: A]`) parse and evaluate: `Tuple.items` is now
  `Vec<TupleElement>` (label/optional/rest metadata), tuple↔tuple relates by
  arity-range containment with optional elements reading as `T | undefined`,
  tuple→array unions the element read types, `T['length']` on an
  optional-arity tuple is the union of arities (`number` with a rest), and
  labels are erased for assignability. Shapes not modelled exactly
  (non-trailing rest `[A, ...B[], C]`, generic spreads `...T`,
  required-after-optional, rest tuples vs object targets, `infer` inside
  optional/rest tuple patterns) stay indeterminate/fail-to-match — never a
  wrong definite answer. _Tests:_ `tests/conformance/tuples_extra.nt`,
  `tests/corpus/typescript/tuple/{optional_element,rest_element,
  labeled_elements,labeled_optional_and_rest,optional_union_element}.txt`.
- [x] **G10.** Optional function parameters (`x?: T`) parse; an optional
  parameter contributes `T | undefined` and does not count toward required
  arity (`split_params` in `src/ast/assignability.rs`). _Tests:_
  `tests/conformance/functions_extra.nt`,
  `tests/corpus/typescript/expr/function_optional_param.txt`.
- [x] **G11.** Constructor types (`new () => T`) parse (`FunctionType.
  is_constructor`); construct and call signatures are never inter-assignable,
  and parameters/returns relate with the usual variance. _Tests:_
  `tests/conformance/functions_extra.nt`.
- [x] **G12.** Mapped modifier-removal (`-?`, `-readonly`) parses (dedicated
  `MinusQuestion`/`MinusReadonly` tokens; `map -readonly -? k in … do … end`)
  and `expand_mapped_type` strips the source modifiers on `Remove`. _Tests:_
  `tests/conformance/mapped_extra.nt`,
  `tests/corpus/typescript/map_expr/modifier_removal.txt`.

---

## ts-toolbelt port audit (2026-07-09): evaluator gaps found porting `examples/ts-toolbelt/`

All 198 ts-toolbelt source files are ported to `examples/ts-toolbelt/` (227
`.nt` files). Every file parses and transpiles; the issues below are all
*evaluator* gaps — asserts that tsgo proves true but newtype reports FAILED
(or worse). Faithful ports were kept; affected asserts carry
`// TODO: evaluator cannot prove this yet` comments (grep for it), except the
crash/hang cases whose asserts are commented out entirely.

### Crashes / non-termination (fix first)

- [ ] **T1. Unbound `infer` inside a template-literal pattern fatally
  overflows the stack** instead of failing the assert. Evaluating
  `String/Split.nt` / `String/At.nt` asserts crashes the compiler: the `?P`
  holes in `` `${?BS}${D}${?AS}` `` never bind ("cannot resolve AS/BS"
  warnings), and the recursive `__Split` loop then recurses forever. Asserts
  in both files are commented out. Repro: uncomment and run
  `target/debug/newtype --input examples/ts-toolbelt/String/Split.nt`.
- [ ] **T2. Recursive `_Path` evaluation never terminates.** In
  `Object/Path.nt` the loop guard `Pos(I) <: Length(P)` stays indeterminate,
  so `resolve_head` keeps unrolling the recursion. Asserts in
  `Object/Path.nt`, `Object/HasPath.nt`, and two in `List/Path.nt` are
  commented out. Separately, the transpiled `_Path` is directly recursive and
  tsgo reports TS2321 (excessively deep); the port should be redone with
  ts-toolbelt's original `{0: …, 1: O}[Extends<…>]` object-index trick so the
  TS side type-checks.

### Evaluator gaps (asserts FAILED; ports faithful, tsgo-true)

- [ ] **T3. Template literal over a *substituted* type parameter is not
  reduced.** `` `${N}` `` with `N = 0` does not reduce to `'0'` (a directly
  written `` `${0}` `` does). This is the highest-leverage gap: it blocks
  `Iteration/IterationOf.nt` and `Iteration/Key.nt` (where `` `${I[0]}` `` —
  indexed access inside a template literal — is also unreduced), and through
  them **all 12 `Number/*` files** (Add, Sub, Negate, Absolute, Range, the
  comparison operators) and downstream Iteration users (`Object/AtLeast`,
  deep-merge `Depth` machinery, `Function/ValidPath`,
  `Community/IncludesDeep`).
- [ ] **T4. Mapped type with a conditional body indexed by `[keyof O]` is not
  reduced.** The `{[K in keyof O]: cond ? K : never}[keyof O]` key-filtering
  idiom stays unevaluated, hitting every `*Keys` type:
  `Object/{Compulsory,NonNullable,Nullable,Optional,Readonly,Required,Select,
  Undefinable,Writable}Keys.nt` and the `List/*Keys.nt` ports built on them.
- [ ] **T5. `keyof O & K` intersections not reduced inside mapped-type key
  positions.** Blocks the `_Pick`/`_Omit` implementations and everything
  layered on them: `Object/{Pick,Omit,Readonly,Writable,Optional,Required,
  Nullable,NonNullable,Undefinable,Compulsory,Record,Update,Merge,Modify}.nt`
  and all of `Object/P/*.nt`.
- [ ] **T6. `infer` through rest-parameter function types.**
  `(...args: ?L) => any` never binds `L`, and `(...args: ?L) => ?R` likewise.
  Affects `Function/{Length,Parameters,Return,Promisify}.nt` (the `==` claims;
  some `<:` forms pass).
- [ ] **T7. `infer` through a generic-alias application.** `Curry(?UF)` in
  `Function/UnCurry.nt` never binds `UF` (the pattern would need the alias
  expanded before matching).
- [ ] **T8. `infer` over an intersection of function types** (the classic
  union→intersection `Union/Last` trick). `IntersectOf` (contravariant infer
  over a distributed union of function types) and `Last` fail; `Union/{ListOf,
  Pop}.nt` fail downstream of `Last`.
- [ ] **T9. Nested conditional over a distributed union parameter not
  reduced.** `Any/Extends.nt`: with `A1 = 'a' | 'b'`, the inner `A1 <: A2`
  conditional does not distribute, so `Extends('a'|'b','b')` should be
  `0 | 1` but isn't. Same root cause via `Extends` in `Any/Is.nt` and the
  tuple-lookup-indexed-by-`Is`/`Equals` idiom in
  `Union/{Filter,Intersect,Replace,Select}.nt` and `Object/Invert.nt`.
- [ ] **T10. Nested type applications passed as arguments are not reduced
  before the outer application evaluates.**
  `Or(IsStringLiteral(A), IsNumberLiteral(A))` in `Community/IsLiteral.nt`
  stays unevaluated even though each component reduces in isolation.
- [ ] **T11. `?U` infer inside a mapped-type pattern** is not matched
  (`Any/KnownKeys.nt`).
- [ ] **T12. Indexed conditional on a wrapped tuple.**
  `[A][if A <: any then 0 end]` (the `NoInfer` trick) is not reduced
  (`Function/NoInfer.nt`).
- [ ] **T13. Recursive aliases generally can't be *proven*.** They parse,
  transpile, and terminate (except T2), but claims about them stay FAILED:
  ~29 `List/*` files (Append, Concat, Reverse, Tail, Repeat, Zip …),
  `Union/ListOf`, `Number/Range`, `String/{Join,Length,Replace}` (also mixing
  T3's template-literal gap).

### Missing definitions (indeterminate, not wrong)

- [ ] **T14. TS lib globals unresolved.** `globalThis.Promise`
  (`Any/Promise.nt`, `Function/{Promisify,Compose,Pipe}` async variants),
  `Record` (`Any/Compute.nt`), and `Error`/`Date`/`RegExp`/`Generator`
  (`Misc/BuiltIn.nt`, warnings only) have no `.nt` definitions, so claims
  touching them are indeterminate. Needs a small ambient-lib prelude.
- [ ] **T15. Test coverage holes from the port:** `Object/{MergeAll,PatchAll,
  Paths}.nt` and `Object/P/_Internal.nt` compile clean but their TS
  `@example`s were not converted to unittest blocks.

---

## Unimplemented language features

- [x] **Optional property modifier** `{ a?: T }` — grammar/parser/printer now
  accept the postfix `?` on object-type properties (the assignability engine
  already modelled `ObjectProperty.optional`). _Tests:_
  `tests/corpus/typescript/object_literal/optional_modifier_postfix.txt`.

- [x] **Bare `keyof` operand** — `keyof T` is now an `expr_prefix` (not only the
  parenthesised `keyof(T)` builtin), so it parses anywhere an operand is
  expected, e.g. `assert keyof X <: …`. Precedence sits just above `pipe`:
  `keyof A[]` is `keyof (A[])`, `keyof A | B` is `(keyof A) | B`
  (`src/grammar.pest`, `src/parser/pratt.rs`, `src/parser.rs`). _Test:_
  `tests/corpus/typescript/expr/keyof_prefix.txt`.
  - [x] _Follow-up:_ resolved by the keyof evaluator (`eval_keyof` in
    `src/ast/assignability.rs`, see G3): `keyof { a: 1, b: 2 } <: string` is
    `true` and `keyof { a: 1 } == 'a'` holds. keyof of primitives/arrays/
    tuples remains indeterminate (see G3's STILL OPEN note).

- [x] **`readonly` arrays/tuples** — `readonly T[]` / `readonly [A, B]` now parse
  via a new `Ast::Readonly` wrapper (`expr_prefix`, rejected on non-array/tuple
  operands as TypeScript does). Assignability is one-directional — a mutable
  array/tuple is assignable to a `readonly` one but not the reverse
  (`src/ast/assignability.rs`), verified against `tsgo --strict`. _Tests:_
  `tests/corpus/typescript/expr/readonly_array.txt`, `readonly_tuple.txt`, and
  `assignability_tests` readonly cases. (`readonly` on object properties was
  already supported.)

- [x] **Interface `extends` clause** — `parse_interface` now reads the `#extends`
  ident (`src/parser.rs`) instead of hardcoding `None`, so
  `interface Foo extends Bar {}` renders correctly. _Test:_
  `pending::interface_extends` (passing, un-ignored).

- [x] **Namespace imports** (`import * as Foo from :a`) — `parse_import_statement`
  now lowers `namespace_import` to `ImportClause::Namespace` (`src/parser.rs`),
  rendering `import type * as Foo from 'a';`. _Test:_
  `pending::namespace_import` (passing, un-ignored).

- [x] **`map_expr` modifiers** (`readonly` / optional `?` / `as` remap) —
  `parse_map_expr` now extracts the `readonly`/`optional` modifier tags and the
  `remap_clause` (`src/parser.rs`). Also fixed a printer bug that dropped the
  space after `readonly` (`src/ast/pretty.rs`). _Test:_
  `pending::map_expr_modifiers` (passing, un-ignored).

- [x] **Equality extends operators** `=`, `!=`, `==`, `!==` — `expand_to_extends`
  (`src/ast/if_expr.rs`) now lowers these via mutual assignability:
  `a = b` → `(a <: b) and (b <: a)`; `a != b` → `(a </: b) or (b </: a)`. Strict
  forms (`==`/`!==`) map identically to the loose forms for now. Also fixed a
  grammar ordering bug where `=`/`!=` shadowed `==`/`!==` in the `extends_infix`
  ordered choice (`src/grammar.pest`). _Test:_ `pending::equality_operators`
  (passing with exact-output assertions, un-ignored).
  - [ ] _Follow-up:_ make the **strict** forms (`==`/`!==`) compile to the
    function-identity trick
    `(<T>() => T extends A ? 1 : 2) extends (<T>() => T extends B ? 1 : 2)` for
    true type-identity (distinct from the loose mutual-assignability forms), and
    tighten `pending::equality_operators::strict_*` to the new exact output.

- [ ] **Macro calls** (`name!(...)`) — `Ast::MacroCall` is `todo!()`
  (`src/runtime.rs`, inside `builtin::unquote`) and `unreachable!()` in the
  renderer (`src/typescript.rs`, "MacroCall should be desugared before this
  point"). `unquote!` is intended to evaluate its argument (`unquote!(1)` →
  `1`); `dbg!` is handled by the `dbg_expr` pass; `assert_equal!` output is
  undecided. _Tests:_ `pending::macro_calls`, and `tests/parser.rs`
  `unquote::evaluates_expression`.

- [x] **Double negation** `not (not (a <: b))` — allowed now: the claim
  grammar's `not` prefix accepts a nested `not` operand (`src/parser.rs`); the
  harness, desugarer, and test codegen already handled nesting recursively.
  _Tests:_ `tests/conformance/exotic_extra.nt` ("double negation").

- [ ] **`dbg!` around a relational claim** — `dbg!(number <: A['length'])` is a
  parse error: macro arguments are parsed with the type-expression pratt table
  (`argument_list` in `src/parser.rs`), while `<:`/`==`/`and`/`or`/`not` live in
  the separate claim grammar used by `if` conditions and `assert`s, so the two
  don't nest. Supporting it is new semantics, not just parsing: a claim isn't a
  type, so the natural report is the branch **decision** (then/else/never/both,
  as `reduce_conditional` already computes for `--trace-eval`) rather than
  `= <type>`. Would need a macro-call atom in the claim grammar, an erasure arm
  + watch kind in `src/ast/dbg_expr.rs`, and a decision-observation hook.
  Workaround today: mark the operands (`if dbg!(number) <: dbg!(A['length'])`)
  or use `--trace-eval`. Design direction undecided.

## Assignability engine (`is_assignable_to`)

- [x] **Implemented the assignability engine** — renamed `is_subtype` →
  `is_assignable_to` and the module `src/ast/subtype.rs` →
  `src/ast/assignability.rs` (the relation is TypeScript *assignability*, not
  strict subtyping). All previously-`todo!()` left-hand-side variants now have
  real structural logic mirroring the TS checker
  (`typescript-go/internal/checker/relater.go`) and the `assignmentCompat*`
  baselines. _Tests:_ `assignability_tests::is_assignable_to_extended` in
  `tests/ast.rs` (71 cases). The `pending::subtype_engine::*` totality stubs
  were removed.
  - [x] `Array` LHS — covariant element relation; arrays are objects (`<:`
    `{}`/`object`/`Object`); not assignable to a fixed-arity tuple.
  - [x] `Tuple` LHS — element-wise, same arity; tuple-to-array (`[A,B] <: T[]`);
    empty tuple assignable to any array; tuples are objects.
  - [x] `TypeLiteral` (non-empty) LHS — structural width/depth subtyping;
    optional target properties may be absent; index/computed keys → `Both`.
  - [x] Set operations — `UnionType` LHS (every member) / RHS (some member);
    `IntersectionType` LHS (some member, or merged object shape) / RHS (every
    member).
  - [x] `Access`, `ApplyGeneric`, `Builtin` (`keyof`), `Path`, and bare `Ident`
    LHS — these are unresolvable references (no type environment at this stage,
    since `let`-bindings are substituted upstream), so they are treated as
    **indeterminate** (`ExtendsResult::Both`), which `unquote` lowers to the
    union of both conditional branches. _Design note:_ `Both` is the sound
    over-approximation (it keeps both branches and matches the existing `any`
    precedent); a follow-up could add an `Indeterminate`/deferred variant that
    re-emits the conditional verbatim for `tsc` to resolve.
  - [x] **Implicit intersection reduction** — contradictory primitive
    intersections now reduce to `never` (`string & number`, `1 & 2`, …) in
    `intersection_source_assignable`, and a `never`-typed LHS makes an
    assignability assertion hold (`src/test_harness.rs` maps a `Never` relation
    leaf to `True` before negation), so `assert string & number <: string`
    passes. Inhabited intersections (`1 & number`, `string & 'a'`) are not
    reduced. _Tests:_ `assignability_tests` intersection rows, `test_harness`
    never cases.
  - [x] **keyof / mapped types / index signatures** — `keyof O` over an object
    literal evaluates to its key union; `{ [K in T]: V }` mapped types expand to
    an object literal over a known key set (incl. `keyof O` and homomorphic
    `O[K]` bodies); `string`/`number` index signatures relate structurally. All
    verified against `tsgo --strict`; unenumerable cases stay `Both` (sound).
  - _Known limitations (deferred, not yet handled):_
    - [ ] **keyof of non-objects** — `keyof T[]` / `keyof <primitive>` stay
      `Both` (e.g. `keyof string[] <: string` is indeterminate, not `true`).
    - [x] **Object index signatures** — `type_literal_assignable_to`
      (`src/ast/assignability.rs`) now relates `string`/`number` index
      signatures structurally via a key classifier
      (`Named`/`StringIndex`/`NumberIndex`/`Other`), matching `tsgo --strict`.
      Following TS, a source index signature does NOT supply named properties
      (`{ [k in string]: number } <: { a: number }` is `False`, not `True` as a
      stale TODO premise suggested); named props must satisfy a target string
      index; a target number index constrains only numeric-named props; and a
      `number` index relates to a `string` index (and vice versa) through the
      shared value-type check. Any key over a non-`string`/`number` iterable, a
      computed key, or a remapped index stays `Both` (conservative). _Tests:_
      `assignability_tests::is_assignable_to_extended` index-signature rows in
      `tests/ast.rs`.

## Bugs

- [x] **Dot-then-indexed access** `A.b[C]` no longer panics: the bug was
  parser precedence (the indexed-access postfix bound tighter than `.`, so
  `[C]` attached to `b` inside the dot's rhs). The postfix now shares `.`/`::`'s
  binding power, so `A.b[C]` parses as `(A.b)[C]` and renders `A['b'][C]`.
  _Test:_ `tests/corpus/typescript/expr/access_dot_then_index.txt` (promoted
  from `pending::known_bugs`).

- [x] **`unittest` statement emits a stray `;`** — already resolved: a
  `unittest` block renders to nothing, locked in by
  `tests/corpus/typescript/program/unittest_emits_nothing.txt` (the
  `pending::unittest_statement` module was removed).

- [x] **Intersection-of-unions dropped parens** — the `IntersectionType` printer
  did not parenthesise union members, so `(A | B) & (C | D)` rendered as
  `A | B & C | D` (a different type, since `&` binds tighter). Fixed in
  `src/ast/pretty.rs`; locked in by
  `tests/corpus/typescript/expr/intersection_of_unions.txt` and
  `union_in_intersection.txt`.

## Dead / unreachable code

- [x] **`MacroCall::eval()` was unused** — removed (`src/ast.rs`). The
  `runtime::builtin` helpers it dispatched to (`dbg`/`assert_equal`/`unquote`)
  remain, exercised by `src/runtime.rs`'s own tests; the `Ast::MacroCall` /
  `Ast::MappedType` `todo!()`s inside `builtin::unquote` are part of the open
  macro-calls feature above.

- [x] **Unreachable `PrefixOp::Infer` arm** — removed: `PrefixOp` is now just
  `Not`, and the `todo!()`/recursion arms in `src/ast/if_expr.rs` went with it
  (the chumsky parser only ever built `Ast::Infer`, never a prefix-op infer).
