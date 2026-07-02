# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`newtype` is a compiler for a small `.nt` language that models a TypeScript-like
**structural type system**. A `.nt` program is a set of `type` aliases,
`interface`s, and `unittest` blocks. The compiler does three things in one pass:

1. **Evaluates** every `assert` claim inside a `unittest` at compile time
   (structural assignability, `==`, `not`/`and`/`or`), reporting ok/FAILED to
   stderr.
2. **Transpiles** the program to TypeScript (`type Foo as 1` → `type Foo = 1;`).
3. With `--generate-tests`, **emits TypeScript type-level assertions** so the
   same claims can be checked by `tsc`/`tsgo` (each `assert` becomes a
   `type _newtype_test__… = Assert<…>` alias).

The language is the ground truth for TypeScript's behavior, so correctness is
defined as *agreeing with `tsgo` (tsc 7.0) under `--strict`*.

## Commands

```sh
cargo build                       # build (binary: target/debug/newtype)
cargo nextest run                 # Rust test suite (preferred runner)
cargo nextest run <substring>     # run a single test / filtered set, e.g. `case_075`
cargo test --test pending -- --ignored   # run the #[ignore]d pending specs
mise run tr                       # = cargo nextest run
mise run tc                       # conformance: cross-check newtype vs tsgo
mise run test                     # both: Rust tests, then conformance
cargo fmt                         # format (the repo is kept fmt-clean)
```

Parser tests in `tests/parser.rs` are `insta` snapshot tests
(`tests/snapshots/`); after an intentional parse-tree change, review and accept
with `cargo insta review`.

Run the compiler directly:

```sh
target/debug/newtype --input FILE.nt              # transpile + run asserts (report on stderr)
target/debug/newtype --generate-tests --input F   # emit the TypeScript test file to stdout
echo '…' | target/debug/newtype --stdin --stdin-filename F.nt --source-map MAP.json
```

### Conformance harness (the newtype-vs-tsgo oracle)

`scripts/conformance.py [FILE.nt …]` feeds the *same* `.nt` source to both
newtype and `tsgo`, then checks they **agree per assertion** (newtype passes an
assert iff tsgo type-checks the generated alias, attributed back to `.nt` lines
via the emitted source map). It requires `tsgo` on `PATH` (installed via
`mise`; tsc 7.0). A `PASS`/`FAIL` split is a real divergence; a "both fail" is
still agreement. With no arguments it runs `examples/test.nt` and every
`tests/conformance/*.nt`. This is the primary tool for finding and verifying
type-system bugs — write a probe `.nt` with assertions known to hold in TS, run
it, and a `DISAGREE` row is a bug. See `TODO.md` for the audit log of known
divergences.

## Architecture

Pipeline: **lex → parse → simplify → (evaluate asserts | render TS | codegen tests)**.

- **Parser** — chumsky 0.13, two stages: `src/parser/lexer.rs` turns source
  into a `(Token, SimpleSpan)` stream (keyword table in `Kw`), then the
  token-level parsers in `src/parser.rs` build the `Ast` in `src/ast.rs`.
  Operator precedence lives in two chumsky **pratt tables** inside
  `src/parser.rs` — one for type expressions, one for boolean/relational
  `assert` claims. Every grammar start symbol is a `Rule` variant and
  `parse_source(Rule::…, src)` is the entry point (the corpus test macros
  reference `Rule` variants by name). Recoverable syntax errors come back as
  `ParseError`s; a handful of *semantic* checks panic with a rendered source
  excerpt, which the CLI's panic hook (`src/panic_report.rs`) recovers into a
  diagnostic. Fields/spans on AST structs come from the `#[ast_node]` attribute
  macro in the `newtype-macros` crate.

- **Diagnostics** — `src/report.rs` renders every source-anchored error (parse
  errors, validation, assert failures, recovered panic spans) as an ariadne
  underlined-excerpt report; all pretty errors funnel through it.

- **AST + assignability engine** — `src/ast.rs` plus the `src/ast/` submodules.
  The heart is **`src/ast/assignability.rs`**: `Ast::is_assignable_to_ctx` is one
  big match implementing TypeScript's structural assignable relation (objects,
  unions/intersections, tuples/arrays, functions with variance, mapped/keyof,
  conditionals/infer, readonly, template literals). It returns an
  **`ExtendsResult`** (`src/extends_result.rs`): `True | False | Never | Both`,
  where `Both` means *indeterminate* (the type involves `any` or an unresolvable
  reference) and `Never` means the LHS is the bottom type. The `.and`/`.or`
  combinators fold component checks; this four-valued algebra is load-bearing —
  when changing the engine, get the `Both`/`Never` cases right, not just
  true/false.

- **Type environment** — `src/ast/type_env.rs` builds a symbol table from
  top-level `type`/`interface`s and resolves named references on demand
  (`resolve_head`), including generic application, defaults, `where`-constraints,
  and **distributive conditional types** (`distribute_or_substitute`). `substitute`
  walks a body replacing type parameters; it relies on `Ast::map` in
  `src/ast/walk.rs` having an arm for *every* node kind that can contain a type
  parameter (a missing arm silently skips substitution — a past soundness bug).

- **Desugaring** — `if`/`cond`/`match`/`let` expressions (`src/ast/if_expr.rs`,
  `cond_expr.rs`, `match_expr.rs`, `let_expr.rs`) are lowered during `simplify()`
  (`src/ast/walk.rs`). Only conditionals in a `type`-alias body desugar; inline
  conditionals in an `assert` do not.

- **Rendering** — `src/ast/pretty.rs` + `src/pretty.rs` render the AST to
  TypeScript via the `pretty` crate.

- **Assert harness** — `src/test_harness.rs` (`run`/`evaluate`) reduces each
  relational claim over the `ExtendsResult` algebra. Only a definite `True`
  passes; `Both` (indeterminate) and `False` fail. It also rejects ill-typed
  programs (wrong generic arity, violated `where` constraints) as errors.

- **Test codegen** — `src/test_codegen.rs` (`--generate-tests`) lowers each
  `assert` into a `Assert<…>`/`Extends`/`Equals`/`Not`/… application. Helper
  definitions are collected into one `BTreeSet<Helper>` per program and rendered
  **once** inside a single `// START/END Newtype Test Helpers` fence. It also
  builds the Source Map v3 (`--source-map`) relating emitted lines to `.nt`
  source lines.

- **CLI** — `src/main.rs` (clap). Reads `--input` or stdin; always evaluates
  asserts (stderr) and renders; `--generate-tests` switches the rendered body to
  the TS test file.

## Tests

Three layers; the first two run under `cargo nextest`:

- **Corpus tests** — fixture files under `tests/corpus/` in pest-test format
  (`name ======= input ======= expected`). The `typescript_tests` /
  `equivalent_tests` / `assertion_tests` attribute macros (in `newtype-macros`,
  driven by `src/corpus.rs`) generate one `#[test]` per fixture. `build.rs`
  re-runs when `tests/corpus/` changes so added/removed fixtures are picked up.
  `tests/corpus/typescript` = render-to-TS, `tests/corpus/newtype` = newtype
  expression equivalence, `tests/corpus/assertions` = render *and* evaluate every
  `assert` (end-to-end).
- **Unit tests** — e.g. `tests/ast.rs` parameterizes `is_assignable_to` over
  `(source, target, ExtendsResult)` cases via `rstest`; expectations mirror the
  TypeScript checker. `tests/parser.rs` snapshots parse trees with `insta`.
- **Conformance** — `tests/conformance/*.nt`, checked against `tsgo` by
  `scripts/conformance.py` (not part of `cargo nextest`; run via `mise run tc`).
  `*_extra.nt` files hold edge cases added from audits.

`tests/pending.rs` holds `#[ignore]`d specs for not-yet-implemented features;
when one is fixed, un-ignore it or promote it into the corpus.

## Conventions

- This is a colocated **jj + git** repo; the working copy is a jj change. Use
  `jj describe`/`jj commit` rather than `git commit`, and `jj log`/`jj st`.
- When fixing or extending the type engine, verify against the conformance
  harness (the canonical oracle), not just the Rust unit tests — and add the new
  cases to `tests/conformance/`.