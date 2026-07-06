# newtype

`newtype` is a compiler for a small language (`.nt`) that models a
TypeScript-like **structural type system** — and lets you *unit test types* at
compile time. A `.nt` program is a set of `type` aliases, `interface`s, and
`unittest` blocks. The compiler:

1. **Evaluates** every `assert` inside a `unittest` at compile time and reports
   ok/FAILED,
2. **Transpiles** the program to TypeScript, and
3. With `--generate-tests`, **emits TypeScript type-level assertions** so the
   same claims can be re-checked by `tsc`/`tsgo`.

TypeScript is the ground truth: correctness means *agreeing with `tsgo`
(tsc 7.0) under `--strict`*, and a conformance harness checks exactly that.

## A taste

```text
type Box(T) as {value: T}

unittest "boxes" do
  assert Box(1) <: {value: number}
  assert not(Box(string) <: {value: number})
end
```

```console
$ newtype --input box.nt
unittest "boxes"
  ok      Box(1) <: {value: number}
  ok      not(Box(string) <: {value: number})

2 assertion(s): 2 passed, 0 failed
type Box<T> = {value: T};
```

The assertion report goes to stderr; the transpiled TypeScript goes to stdout.

## Language tour

### Type aliases and interfaces

Aliases are declared with `type Name as Body` (or `=` for simple bodies).
Interfaces look like TypeScript's, with `extends` and structural bodies:

```text
type Primitive as
  boolean | string | number | bigint | symbol | undefined | null

interface Bird {
  fly: () => void,
}

interface Duck {
  fly: () => void,
  quack: () => void,
}
```

All the familiar type expressions are available: literals (`1`, `'hi'`,
`true`), object types, tuples (`[A, B]`), arrays (`T[]`), functions with
variance-aware parameters (`(string) => number`), unions, intersections,
`readonly`, optional properties (`x?: T`), template literals
(`` `a${string}` ``), and index signatures.

### Unit tests for types

`unittest` blocks contain `assert` claims that are evaluated at compile time.
The core relation is `<:` — structural assignability, TypeScript's `extends`:

```text
unittest "structural assignability" do
  assert string <: unknown
  assert {x: ''} <: {x: string}
  assert Duck <: Bird           // a duck is a bird...
  assert not(Bird <: Duck)      // ...but not vice versa
  assert () => never <: () => any
  assert not((string) => any <: (unknown) => any)
end
```

Claims compose with `==` (mutual assignability), `not`, `and`, `or`, and the
negated relation `</:`:

```text
unittest "keyof object yields union of keys" do
  assert keyof {x: number, y: string} == 'x' | 'y'
  assert 'a' <: keyof {a: number, b: string}
  assert not('d' <: keyof {a: number, b: string})
end
```

Assertions are four-valued under the hood (`True | False | Never | Both`);
only a definite `True` passes — a claim involving `any` or an unresolvable
reference is *indeterminate* and fails rather than silently passing.

### Generics: parameters, defaults, constraints

Type parameters use call syntax. `defaults` supplies omitted arguments (and
may reference earlier parameters); `where` clauses constrain them —
applications that violate a constraint are compile errors:

```text
type Pair(A, B) as [A, B]

type Wrap(A, B) defaults B = A as [A, B]

type Num(T) where T <: number as T

export type At(A, K)
where
  A <: any,
  K <: string | number | symbol
as ...

unittest "generics" do
  assert Pair(1, 2) == [1, 2]
  assert Wrap(number) == [number, number]
  assert Num(5) <: number
end
```

### Conditionals: `if`, `cond`, `match`

Instead of TypeScript's ternary `extends` chains, newtype has structured
conditionals. They desugar to TypeScript conditional types when they appear in
a `type`-alias body:

```text
type Choose(T) as if T <: string then 1 else 2 end

// Missing else = never, just like an unmatched conditional
type NonNull(T) as if T <: null | undefined then never else T end
```

`cond` replaces nested `if`-chains, and `match` scrutinizes one type against a
sequence of patterns:

```text
type IsLeafNode(T) as
  cond do
    not(T <: Record(string, any)) -> true,
    T <: Hole -> true,
    T == any -> true,
    else -> false,
  end

type ExtractSubcapture(T) as
  match T do
    Primitive | BuiltIn -> never,
    object -> T[Exclude(keyof(T), keyof([]) | keyof({}))],
  end
```

### Inference with `?`

`?U` is newtype's `infer U` — bind a piece of the matched type inside a
conditional:

```text
type ElemOf(T)    as if T <: Array(?U) then U else never end
type ReturnOf(T)  as if T <: (any) => ?R then R else never end
type Unwrap(T)    as if T <: Promise(?U) then U else never end
type Fst(T)       as if T <: [?A, ?B] then A else never end
```

### Mapped types and `keyof`

`map k in K do V end` is `{ [k in K]: V }`:

```text
type Obj as {a: number, b: string, c: boolean}

unittest "mapped types" do
  // homomorphic identity
  assert map k in keyof(Obj) do Obj[k] end == Obj
  // value transformation
  assert map k in keyof(Obj) do boolean end
      == {a: boolean, b: boolean, c: boolean}
  // over a literal key union
  assert map k in 'a' | 'b' do number end == {a: number, b: number}
end
```

### `let` bindings and the pipeline operator

`let name = T in body` names an intermediate type; `|>` threads a type through
a chain of unary (or partially-applied) generic aliases:

```text
type ExpandCapture(T) as
  T
  |> HoleInnerType()
  |> CapturePattern()
  |> RecursiveExpandCapture()

export type At(A, K) as
  let value = A[K] in
  let keys = keyof A in
  if K <: keys then value else undefined end
```

### Everything else

- `unique symbol name` declares a unique symbol usable as a property key
  (`readonly [__kind__]: Label`).
- `export` marks declarations for the emitted TypeScript.
- `dbg!(claim)` marks a sub-claim for evaluation tracing (see `--trace-eval`).
- Namespaced references like `Union::Merge(...)` address types from imported
  ts-toolbelt-style libraries.

See `examples/` (in particular `examples/match-ts.nt`, a port of the
`match-ts` pattern-matching library, and `examples/ts-toolbelt.nt`) and
`tests/conformance/*.nt` for substantial, real programs.

## CLI

```sh
newtype --input FILE.nt              # transpile to stdout, assert report on stderr
newtype --input FILE.nt -o OUT.ts    # write the TypeScript to a file
newtype --generate-tests --input F   # emit tsc-checkable type-level assertions
echo '…' | newtype --stdin --stdin-filename F.nt --source-map MAP.json
```

Useful flags: `--fail-fast` (stop at the first failed assert),
`--deny-unresolved` (unresolved type references become hard errors),
`--exact-optional-property-types` (mirror the tsc flag), and `--trace-eval`
(trace every generic instantiation and conditional decision, with source-anchored
`dbg!` stacktraces).

With `--generate-tests`, each `assert` becomes a TypeScript alias like
`type _newtype_test__… = Assert<…>` — feeding the output to `tsgo --strict
--noEmit` re-checks every claim with the real TypeScript checker, and
`--source-map` maps failures back to `.nt` lines.

## Building and testing

```sh
cargo build                       # binary: target/debug/newtype
cargo nextest run                 # Rust test suite
mise run tc                       # conformance: cross-check newtype vs tsgo
mise run test                     # both
```

The conformance harness (`scripts/conformance.py`) feeds the same `.nt` source
to both newtype and `tsgo` and requires them to **agree per assertion** — this
is the primary oracle for the type engine. It needs `tsgo` on `PATH`
(installed via `mise`). See `CLAUDE.md` for architecture notes and `TODO.md`
for the audit log of known divergences.
