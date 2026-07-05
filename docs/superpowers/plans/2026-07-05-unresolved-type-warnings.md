# Unresolved Type-Reference Warnings Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Desugar built-in sugar aliases (`Array(T)` → `T[]`, `ReadonlyArray(T)` → `readonly T[]`, `Readonly(tuple/array)` → `readonly …`, `keyof any` → `string | number | symbol`) before resolution, then statically warn (via ariadne) on every remaining type reference that can't resolve — with a `--deny-unresolved` flag to promote warnings to errors — and add an `--exact-optional-property-types` flag gating the `x?: T` ⊇ `T | undefined` widening.

**Architecture:** Two new `src/ast/` submodules. `desugar.rs` rewrites sugar bottom-up (modeled on `rewrite_unique_symbols`, because `Ast::map` does not recurse into `UnitTest`/`Assert`/`Interface`), and is called at the front of `Ast::simplify`. `unresolved.rs` is an explicit recursive visitor with a scope stack that collects unresolved `Ident`s / `ApplyGeneric` heads, grouped by name. `report.rs` gains multi-label warning rendering; `main.rs` wires the pass in after parse+validate.

**Tech Stack:** Rust, chumsky-parsed AST in `src/ast.rs`, ariadne 0.6 for diagnostics, `cargo nextest` for tests.

**Spec:** `docs/superpowers/specs/2026-07-05-unresolved-type-warnings-design.md`

## Global Constraints

- This is a colocated **jj + git** repo: commit with `jj commit -m "..."`, never `git commit`. Each task's final step is one `jj commit`.
- Test runner is `cargo nextest run` (a plain `cargo test` also works for doctests, but nextest is canonical).
- Keep the repo `cargo fmt`-clean: run `cargo fmt` before every commit.
- The warning label text is exactly `cannot be resolved to a definition`; the report message is exactly ``cannot resolve type `<name>` `` (backticks around the name).
- Engine-known names that never warn: `Object`, `Function`, `Boolean`, `Number`, `String`, `Symbol`, `BigInt`.
- The pass never blocks evaluation or rendering; only `--deny-unresolved` affects the exit code.

---

### Task 1: Warning/multi-label rendering in `src/report.rs`

**Files:**
- Modify: `src/report.rs`

**Interfaces:**
- Consumes: existing `ReportSpan`, `clamp`, `report_to_string` in `src/report.rs`.
- Produces: `pub enum Severity { Warning, Error }` and
  `pub fn render_labeled(severity: Severity, source_name: &str, source: &str, message: &str, labels: &[(Span, String)], color: bool) -> String`.
  Task 5 (CLI) calls `render_labeled(Severity::Warning, …)`.

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module at the bottom of `src/report.rs`:

```rust
    #[test]
    fn renders_warning_with_multiple_labels() {
        let source = "type A as Foo\ntype B as Foo\n";
        let first = source.find("Foo").unwrap();
        let second = source.rfind("Foo").unwrap();
        let labels = vec![
            (Span::new(first, first + 3), "cannot be resolved to a definition".to_string()),
            (Span::new(second, second + 3), "cannot be resolved to a definition".to_string()),
        ];
        let out = render_labeled(
            Severity::Warning,
            "test.nt",
            source,
            "cannot resolve type `Foo`",
            &labels,
            false,
        );
        assert!(out.contains("Warning"), "{out}");
        assert!(out.contains("cannot resolve type `Foo`"), "{out}");
        assert!(out.contains("test.nt:1:11"), "{out}");
        // Both use sites are labeled.
        assert_eq!(out.matches("cannot be resolved to a definition").count(), 2, "{out}");
        assert!(!out.contains('\x1b'), "{out}");
    }

    #[test]
    fn renders_error_severity() {
        let source = "type A as Foo\n";
        let at = source.find("Foo").unwrap();
        let labels = vec![(Span::new(at, at + 3), "boom".to_string())];
        let out = render_labeled(Severity::Error, "x.nt", source, "bad", &labels, false);
        assert!(out.contains("Error"), "{out}");
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run renders_warning_with_multiple_labels renders_error_severity`
Expected: compile error — `Severity` and `render_labeled` not found.

- [ ] **Step 3: Implement `Severity` and `render_labeled`**

Add to `src/report.rs` (below `eprint`):

```rust
/// Severity of a rendered diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

impl Severity {
    fn kind(self) -> ReportKind<'static> {
        match self {
            Severity::Warning => ReportKind::Warning,
            Severity::Error => ReportKind::Error,
        }
    }
}

/// Render a diagnostic with one message and any number of labeled spans (one
/// per use site). The first label's span anchors the report header.
pub fn render_labeled(
    severity: Severity,
    source_name: &str,
    source: &str,
    message: &str,
    labels: &[(Span, String)],
    color: bool,
) -> String {
    let primary = labels
        .first()
        .map(|(span, _)| clamp(*span, source.len()))
        .unwrap_or(0..0);

    let mut report = Report::build(severity.kind(), (source_name.to_string(), primary))
        .with_config(
            Config::new()
                .with_index_type(IndexType::Byte)
                .with_color(color),
        )
        .with_message(message);

    for (span, label) in labels {
        let range = clamp(*span, source.len());
        report = report
            .with_label(Label::new((source_name.to_string(), range)).with_message(label));
    }

    report_to_string(&report.finish(), source_name, source)
}
```

Also extend the import at the top of the file:

```rust
use ariadne::{Config, IndexType, Label, Report, ReportKind, Source};
```

(already present — no change needed if identical).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run renders_warning_with_multiple_labels renders_error_severity`
Expected: 2 tests PASS. Also run `cargo nextest run report` to confirm the existing report tests still pass.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
jj commit -m "Add multi-label warning rendering to report"
```

---

### Task 2: Global sugar desugaring (`src/ast/desugar.rs`)

**Files:**
- Create: `src/ast/desugar.rs`
- Modify: `src/ast.rs` (module declaration, near line 1156 where the other `mod`s live)
- Modify: `src/ast/walk.rs` (call desugar at the front of `simplify`)
- Modify: `tests/corpus/typescript/type_alias/params_in_application.txt`

**Interfaces:**
- Consumes: `Ast::map` (`src/ast/walk.rs:10`), AST structs from `src/ast.rs`: `ApplyGeneric { receiver, args, span }`, `Builtin { name: BuiltinKeyword, argument, span }`, `BuiltinKeyword::Keyof`, `PrimitiveType`, `UnionType { types, span }`, `Assert`, `UnitTest`, `Interface`.
- Produces: `Ast::desugar_globals(&self) -> Ast` (public method), used by `Ast::simplify` and by Task 5's CLI wiring.

- [ ] **Step 1: Write the failing tests**

Create `src/ast/desugar.rs` with only the test module for now:

```rust
//! Pre-resolution desugaring of global sugar aliases.
//!
//! Some built-in TypeScript types are alternate spellings of forms the engine
//! already understands. Rewriting them into the core form *before* any
//! identifier resolution means they are never treated as (unresolvable) type
//! references:
//!
//! - `Array(T)`         → `T[]`
//! - `ReadonlyArray(T)` → `readonly T[]`
//! - `Readonly(T)`      → `readonly T`, only when `T` is a tuple or array
//! - `keyof any`        → `string | number | symbol`
//!
//! Anything the rewrite doesn't cover — wrong arity, a bare `Array` ident,
//! `Readonly` of a non-tuple/array (TypeScript's mapped-type `Readonly<T>` is
//! not implemented) — is left untouched and surfaces through the
//! unresolved-reference warning pass instead.

#[cfg(test)]
mod tests {
    use crate::ast::Ast;
    use crate::parser::parse_newtype_program;

    /// Parse, desugar, and render, so assertions read as TypeScript.
    fn desugar_ts(src: &str) -> String {
        use crate::typescript::Pretty;
        parse_newtype_program(src)
            .unwrap()
            .desugar_globals()
            .render_pretty_ts(120)
    }

    #[test]
    fn array_application_becomes_array_type() {
        assert_eq!(desugar_ts("type A as Array(number)"), "type A = number[];\n");
    }

    #[test]
    fn readonly_array_becomes_readonly_array_type() {
        assert_eq!(
            desugar_ts("type A as ReadonlyArray(number)"),
            "type A = readonly number[];\n"
        );
    }

    #[test]
    fn readonly_of_tuple_becomes_readonly_tuple() {
        assert_eq!(
            desugar_ts("type A as Readonly([1, 2])"),
            "type A = readonly [1, 2];\n"
        );
    }

    #[test]
    fn readonly_of_object_is_left_alone() {
        assert_eq!(
            desugar_ts("type A as Readonly({a: 1})"),
            "type A = Readonly<{ a: 1 }>;\n"
        );
    }

    #[test]
    fn array_with_wrong_arity_is_left_alone() {
        assert_eq!(
            desugar_ts("type A as Array(1, 2)"),
            "type A = Array<1, 2>;\n"
        );
    }

    #[test]
    fn keyof_any_becomes_key_union() {
        assert_eq!(
            desugar_ts("type A as keyof any"),
            "type A = string | number | symbol;\n"
        );
    }

    #[test]
    fn nested_sugar_desugars_bottom_up() {
        assert_eq!(
            desugar_ts("type A as ReadonlyArray(Array(number))"),
            "type A = readonly number[][];\n"
        );
    }

    #[test]
    fn desugars_inside_interfaces_and_assert_claims() {
        let ts = desugar_ts(
            "interface I { xs: Array(number) }\n\
             unittest \"t\" do\n  assert [1] <: Array(number)\nend",
        );
        assert!(ts.contains("xs: number[]"), "{ts}");
        // Assert claims are evaluated, not rendered; check the evaluated result
        // separately below.
    }

    #[test]
    fn desugared_claim_evaluates() {
        let src = "unittest \"t\" do\n  assert [1] <: Array(number)\n  assert [1] <: ReadonlyArray(number)\nend";
        let program = parse_newtype_program(src).unwrap().simplify();
        let mut out = Vec::new();
        let report = crate::test_harness::run(
            &program,
            src,
            "<test>",
            crate::test_harness::Config::default(),
            &mut out,
        )
        .unwrap();
        assert_eq!(report.passed, 2, "{}", String::from_utf8_lossy(&out));
    }
}
```

Then declare the module in `src/ast.rs`, next to the other module declarations (around line 1156, where `pub(crate) mod if_expr;` etc. live):

```rust
mod desugar;
```

**Note:** the exact rendered strings above (`"type A = number[];\n"`, spacing of `{ a: 1 }`, trailing newline) are the plan author's expectation of the pretty-printer. On the first run, if a test fails only on formatting (extra newline, spacing), adjust the expected string to the actual pretty-printer output — the shape (`number[]`, `readonly number[]`, `Array<1, 2>` preserved) is what matters.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run desugar`
Expected: compile error — `desugar_globals` not found.

- [ ] **Step 3: Implement `desugar_globals`**

Add above the test module in `src/ast/desugar.rs`:

```rust
use std::rc::Rc;

use crate::ast::{
    ApplyGeneric, Assert, Ast, Builtin, BuiltinKeyword, Ident, Interface, PrimitiveType,
    UnionType, UnitTest,
};

impl Ast {
    /// Rewrite global sugar aliases bottom-up across the whole tree, including
    /// `unittest` bodies, `assert` claims, and `interface` definitions (which
    /// [`Ast::map`] does not recurse into).
    pub fn desugar_globals(&self) -> Ast {
        let node = match self {
            Ast::UnitTest(ut) => Ast::UnitTest(UnitTest {
                span: ut.span,
                name: ut.name.clone(),
                body: ut.body.iter().map(|node| node.desugar_globals()).collect(),
            }),
            Ast::Assert(assert) => Ast::Assert(Assert {
                span: assert.span,
                claim: Rc::new(assert.claim.desugar_globals()),
            }),
            Ast::Interface(interface) => Ast::Interface(Interface {
                definition: interface
                    .definition
                    .iter()
                    .map(|prop| prop.clone().map(|ty| ty.desugar_globals()))
                    .collect(),
                extends: interface
                    .extends
                    .as_ref()
                    .map(|e| Rc::new(e.desugar_globals())),
                ..interface.clone()
            }),
            other => other.map(|child| child.desugar_globals()),
        };
        rewrite(node)
    }
}

/// Rewrite one (already child-desugared) node if it is a sugar alias.
fn rewrite(node: Ast) -> Ast {
    match &node {
        Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) => {
            let Ast::Ident(Ident { name, .. }) = receiver.as_ref() else {
                return node;
            };
            match (name.as_str(), args.as_slice()) {
                ("Array", [element]) => Ast::Array(Rc::new(element.clone())),
                ("ReadonlyArray", [element]) => {
                    Ast::Readonly(Rc::new(Ast::Array(Rc::new(element.clone()))))
                }
                // TypeScript's mapped-type `Readonly<T>` over objects is not
                // implemented; only the tuple/array form is sugar.
                ("Readonly", [inner @ (Ast::Tuple(_) | Ast::Array(_))]) => {
                    Ast::Readonly(Rc::new(inner.clone()))
                }
                _ => node,
            }
        }
        Ast::Builtin(Builtin {
            name: BuiltinKeyword::Keyof,
            argument,
            span,
        }) if matches!(argument.as_ref(), Ast::AnyKeyword(_)) => Ast::UnionType(UnionType {
            types: vec![
                Ast::Primitive(PrimitiveType::String, *span),
                Ast::Primitive(PrimitiveType::Number, *span),
                Ast::Primitive(PrimitiveType::Symbol, *span),
            ],
            span: *span,
        }),
        _ => node,
    }
}
```

If the compiler complains about field/variant shapes (e.g. `Ast::Primitive`'s
second field, `ObjectProperty::map` taking `self` by value), match the actual
definitions in `src/ast.rs` — `ObjectProperty::map` is `fn map(self, f)` (by
value, private to the `ast` module; submodules can call it), and
`Ast::Primitive(PrimitiveType, Span)` carries its span as a second tuple
field per the `#[ast_node(span)]` attribute.

- [ ] **Step 4: Wire desugaring into `simplify`**

In `src/ast/walk.rs`, at the front of `pub fn simplify` (line ~108), desugar
before the unique-symbol rewrite so every consumer of `simplify` sees core
forms:

```rust
    pub fn simplify(&self) -> Self {
        let bindings: Bindings = Default::default();

        let identity = |node, ctx| (node, ctx);

        // Rewrite global sugar aliases (`Array(T)` → `T[]`, …) before anything
        // resolves identifiers, then rewrite references to declared `unique
        // symbol`s across the whole program (including `assert` claims, which
        // the desugaring traverse below does not reach), then desugar
        // conditionals in type-alias bodies.
        let desugared = self.desugar_globals();

        let symbols = desugared.unique_symbols();
        let rewritten = if symbols.is_empty() {
            desugared
        } else {
            desugared.rewrite_unique_symbols(&symbols)
        };
```

(The remainder of `simplify` is unchanged and continues from `rewritten`.)

- [ ] **Step 5: Run desugar tests**

Run: `cargo nextest run desugar`
Expected: all Task 2 tests PASS (after any formatting-string adjustments noted in Step 1).

- [ ] **Step 6: Update the corpus fixture and run the full suite**

`tests/corpus/typescript/type_alias/params_in_application.txt` currently expects `Array(x)` to render as `Array<x>`; desugaring changes the rendering. Update the expected block:

```
Generic type alias whose param is used in a type application body

=======

type A(x) as Array(x)

=======

type A<x> = x[]

=======
```

(Match the existing fixture's exact expected formatting — if the corpus
expects a trailing semicolon or newline convention, keep it consistent with
the old expected block, changing only `Array<x>` → `x[]`.)

Run: `cargo nextest run`
Expected: full suite PASS. If other corpus/snapshot expectations legitimately
changed rendering of `Array(...)`/`ReadonlyArray(...)`, update them the same
way; any *evaluation* regression (assertions corpus, `tests/ast.rs`) is a real
bug in the rewrite — fix the rewrite, don't update those expectations.

- [ ] **Step 7: Format and commit**

```bash
cargo fmt
jj commit -m "Desugar Array/ReadonlyArray/Readonly/keyof-any sugar before resolution"
```

---

### Task 3: Unresolved-reference pass core (`src/ast/unresolved.rs`)

**Files:**
- Create: `src/ast/unresolved.rs`
- Modify: `src/ast.rs` (module declaration)
- Modify: `src/ast/type_env.rs` (make `top_level_nodes` `pub(crate)`, line ~306)

**Interfaces:**
- Consumes: `Ast` and struct definitions from `src/ast.rs`; `top_level_nodes` from `src/ast/type_env.rs`; `Ast::prewalk` from `src/ast/walk.rs` (signature: `prewalk<Context: Clone, F: Fn(Self, Context) -> (Self, Context)>(&self, ctx, pre: &F) -> (Self, Context)`).
- Produces:
  ```rust
  pub struct UnresolvedRef { pub name: String, pub spans: Vec<Span> }
  pub fn unresolved_references(program: &Ast) -> Vec<UnresolvedRef>
  ```
  Order: by first use site, source order. Task 5 (CLI) consumes this.

- [ ] **Step 1: Make `top_level_nodes` crate-visible**

In `src/ast/type_env.rs` line ~306 change:

```rust
fn top_level_nodes(program: &Ast) -> impl Iterator<Item = &Ast> {
```

to:

```rust
pub(crate) fn top_level_nodes(program: &Ast) -> impl Iterator<Item = &Ast> {
```

Run: `cargo build` — expected: clean (possibly an unused-visibility lint-free build).

- [ ] **Step 2: Write the failing core tests**

Create `src/ast/unresolved.rs`:

```rust
//! Static detection of unresolved type references.
//!
//! Walks the parsed, desugared (pre-`simplify`) program and reports every
//! type reference — a bare `Ident` in type position, or the `Ident` head of a
//! generic application — that does not resolve to a top-level definition, an
//! imported name, an engine-known global (`Object`, `Function`, the object
//! wrappers), or a lexically scoped binder (type parameters, `infer`
//! bindings, mapped-type and index-signature keys, `let` bindings, `match`
//! arm binders).
//!
//! The pass is purely additive: it never blocks evaluation or rendering. The
//! CLI renders each result as an ariadne warning (or, with
//! `--deny-unresolved`, an error).

use std::collections::{HashMap, HashSet};

use crate::ast::type_env::top_level_nodes;
use crate::ast::{
    Ast, Ident, ImportClause, Interface, PropertyName, Span, TypeAlias, TypeParameter,
};

/// Names the assignability engine understands semantically without a
/// definition: the `Object`/`Function` interfaces and the object wrappers.
const ENGINE_KNOWN: [&str; 7] = [
    "Object", "Function", "Boolean", "Number", "String", "Symbol", "BigInt",
];

/// All use sites of one unresolved name, in source order.
#[derive(Debug, PartialEq, Eq)]
pub struct UnresolvedRef {
    pub name: String,
    pub spans: Vec<Span>,
}

/// Collect every unresolved type reference in `program`, grouped by name and
/// ordered by first use site.
pub fn unresolved_references(program: &Ast) -> Vec<UnresolvedRef> {
    let mut collector = Collector {
        globals: collect_globals(program),
        scopes: Vec::new(),
        refs: Vec::new(),
    };

    for node in top_level_nodes(program) {
        collector.visit(node);
    }

    group(collector.refs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_newtype_program;

    /// Parse + desugar `src` (mirroring the CLI) and return each unresolved
    /// name with its use-site count.
    fn refs(src: &str) -> Vec<(String, usize)> {
        let program = parse_newtype_program(src).unwrap().desugar_globals();
        unresolved_references(&program)
            .into_iter()
            .map(|r| (r.name, r.spans.len()))
            .collect()
    }

    #[test]
    fn undefined_bare_ident_warns() {
        assert_eq!(refs("type A as Foo"), vec![("Foo".to_string(), 1)]);
    }

    #[test]
    fn undefined_generic_head_and_args_warn() {
        assert_eq!(
            refs("type A as Foo(Bar)"),
            vec![("Foo".to_string(), 1), ("Bar".to_string(), 1)]
        );
    }

    #[test]
    fn defined_alias_interface_and_symbol_resolve() {
        let src = "type T as 1\n\
            interface I { x: number }\n\
            unique symbol S\n\
            type A as [T, I, S]";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn definition_order_does_not_matter() {
        assert_eq!(refs("type A as B\ntype B as 1"), vec![]);
    }

    #[test]
    fn multiple_uses_group_under_one_name() {
        assert_eq!(
            refs("type A as Foo\ntype B as Foo"),
            vec![("Foo".to_string(), 2)]
        );
    }

    #[test]
    fn engine_known_globals_do_not_warn() {
        let src = "unittest \"t\" do\n\
            \x20 assert () => void <: Function\n\
            \x20 assert {} <: Object\n\
            \x20 assert string <: String\n\
            end";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn desugared_array_sugar_does_not_warn() {
        assert_eq!(refs("type A as ReadonlyArray(Array(number))"), vec![]);
    }

    #[test]
    fn named_import_resolves() {
        assert_eq!(refs("import { Foo } from \"./m.nt\"\ntype A as Foo"), vec![]);
    }

    #[test]
    fn aliased_import_resolves_the_alias_not_the_original() {
        assert_eq!(
            refs("import { Foo as Bar } from \"./m.nt\"\ntype A as [Bar, Foo]"),
            vec![("Foo".to_string(), 1)]
        );
    }

    #[test]
    fn namespace_import_resolves() {
        assert_eq!(refs("import * as NS from \"./m.nt\"\ntype A as NS"), vec![]);
    }

    #[test]
    fn assert_claims_are_scanned() {
        assert_eq!(
            refs("unittest \"t\" do\n  assert Foo <: number\nend"),
            vec![("Foo".to_string(), 1)]
        );
    }

    #[test]
    fn spans_point_at_the_use_site() {
        let src = "type A as Foo";
        let program = parse_newtype_program(src).unwrap().desugar_globals();
        let found = unresolved_references(&program);
        assert_eq!(found.len(), 1);
        let span = found[0].spans[0];
        assert_eq!(&src[span.start()..span.end()], "Foo");
    }
}
```

**Syntax note for the test author:** if `unique symbol S` or the import forms
fail to parse, check the corpus (`tests/corpus/`) and
`src/parser.rs` (`import_statement`, line ~958) for the exact concrete
syntax and adjust the test source — the parser, not the test, is ground
truth. `Span` exposes `start()`/`end()` accessor methods (used as
`assert_span.start()` in `src/test_harness.rs:306`).

Declare the module in `src/ast.rs` next to the other module declarations:

```rust
pub mod unresolved;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run unresolved`
Expected: compile error — `Collector`, `collect_globals`, `group` not defined.

- [ ] **Step 4: Implement globals collection, the visitor core, and grouping**

Add between the `UnresolvedRef` block and the tests:

```rust
/// The names a program defines at the top level: `type` aliases, `interface`s,
/// `unique symbol`s (mirroring `TypeEnv::from_program`), the local names bound
/// by `import` statements, and the engine-known globals.
fn collect_globals(program: &Ast) -> HashSet<String> {
    let mut names: HashSet<String> = ENGINE_KNOWN.iter().map(|s| s.to_string()).collect();

    for node in top_level_nodes(program) {
        match node {
            Ast::TypeAlias(TypeAlias { name, .. }) => {
                names.insert(name.name.clone());
            }
            Ast::Interface(Interface { name, .. }) => {
                names.insert(name.clone());
            }
            Ast::UniqueSymbolDecl(sym) => {
                names.insert(sym.name.clone());
            }
            Ast::ImportStatement(import) => match &import.import_clause {
                ImportClause::Named(specifiers) => {
                    for specifier in specifiers {
                        let local = specifier
                            .alias
                            .as_ref()
                            .unwrap_or(&specifier.module_export_name);
                        names.insert(local.name.clone());
                    }
                }
                ImportClause::Namespace { alias } => {
                    names.insert(alias.name.clone());
                }
            },
            _ => {}
        }
    }

    names
}

/// Group flat `(name, span)` sightings into one entry per name, preserving
/// first-sighting order (which is source order for a top-down walk).
fn group(refs: Vec<(String, Span)>) -> Vec<UnresolvedRef> {
    let mut grouped: Vec<UnresolvedRef> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();

    for (name, span) in refs {
        match index.get(&name) {
            Some(&at) => grouped[at].spans.push(span),
            None => {
                index.insert(name.clone(), grouped.len());
                grouped.push(UnresolvedRef {
                    name,
                    spans: vec![span],
                });
            }
        }
    }

    grouped
}

struct Collector {
    globals: HashSet<String>,
    /// Lexical scopes, innermost last. Pushed around binders (type parameters,
    /// `infer`, mapped-type keys, `let`, `match` arms).
    scopes: Vec<HashSet<String>>,
    refs: Vec<(String, Span)>,
}

impl Collector {
    fn resolved(&self, name: &str) -> bool {
        self.globals.contains(name) || self.scopes.iter().any(|scope| scope.contains(name))
    }

    fn reference(&mut self, name: &str, span: Span) {
        if !self.resolved(name) {
            self.refs.push((name.to_string(), span));
        }
    }

    fn scoped(&mut self, names: HashSet<String>, f: impl FnOnce(&mut Self)) {
        self.scopes.push(names);
        f(self);
        self.scopes.pop();
    }

    /// Push the type parameters of a definition, visit their constraints and
    /// defaults, then the definition's own contents.
    fn with_params(&mut self, params: &[TypeParameter], f: impl FnOnce(&mut Self)) {
        let names = params.iter().map(|p| p.name.clone()).collect();
        self.scoped(names, |collector| {
            for param in params {
                if let Some(constraint) = &param.constraint {
                    collector.visit(constraint);
                }
                if let Some(default) = &param.default {
                    collector.visit(default);
                }
            }
            f(collector);
        });
    }

    fn visit(&mut self, ast: &Ast) {
        match ast {
            Ast::Ident(Ident { name, span }) => self.reference(name, *span),

            Ast::ApplyGeneric(apply) => {
                match apply.receiver.as_ref() {
                    Ast::Ident(Ident { name, span }) => self.reference(name, *span),
                    other => self.visit(other),
                }
                for arg in &apply.args {
                    self.visit(arg);
                }
            }

            Ast::TypeAlias(TypeAlias { params, body, .. }) => {
                self.with_params(params, |collector| collector.visit(body));
            }

            Ast::Interface(Interface {
                params,
                extends,
                definition,
                ..
            }) => {
                self.with_params(params, |collector| {
                    if let Some(extends) = extends {
                        collector.visit(extends);
                    }
                    for property in definition {
                        collector.visit_property(property);
                    }
                });
            }

            Ast::Statement(inner) | Ast::Array(inner) | Ast::Readonly(inner) => self.visit(inner),

            Ast::Program(program) => {
                for statement in &program.statements {
                    self.visit(statement);
                }
            }

            Ast::UnitTest(unittest) => {
                for statement in &unittest.body {
                    self.visit(statement);
                }
            }

            Ast::Assert(assert) => self.visit(&assert.claim),

            Ast::ExtendsInfixOp(op) => {
                self.visit(&op.lhs);
                self.visit(&op.rhs);
            }

            Ast::ExtendsPrefixOp(op) => self.visit(&op.value),

            // Property access: `A['x']` has a type-expression rhs; `A.x`'s rhs
            // is a property name, not a type reference.
            Ast::Access(access) => {
                self.visit(&access.lhs);
                if !access.is_dot {
                    self.visit(&access.rhs);
                }
            }

            Ast::UnionType(union) => {
                for ty in &union.types {
                    self.visit(ty);
                }
            }

            Ast::IntersectionType(intersection) => {
                for ty in &intersection.types {
                    self.visit(ty);
                }
            }

            Ast::Tuple(tuple) => {
                for item in &tuple.items {
                    self.visit(item);
                }
            }

            Ast::TypeLiteral(literal) => {
                for property in &literal.properties {
                    self.visit_property(property);
                }
            }

            Ast::FunctionType(function) => {
                for parameter in &function.params {
                    self.visit(&parameter.kind);
                }
                self.visit(&function.return_type);
            }

            Ast::Builtin(builtin) => self.visit(&builtin.argument),

            Ast::MacroCall(call) => {
                for arg in &call.args {
                    self.visit(arg);
                }
            }

            // `A::B::…`: only the head segment is a reference into this
            // program's namespace; later segments are members of it.
            Ast::Path(path) => {
                if let Some(head) = path.segments.first() {
                    self.visit(head);
                }
            }

            // Binders are handled in Task 4; visit children without scoping
            // for now so nothing is silently skipped.
            Ast::MappedType(mapped) => {
                self.visit(&mapped.iterable);
                if let Some(remap) = &mapped.remapped_as {
                    self.visit(remap);
                }
                self.visit(&mapped.body);
            }

            Ast::LetExpr(let_expr) => {
                for value in let_expr.bindings.values() {
                    self.visit(value);
                }
                self.visit(&let_expr.body);
            }

            Ast::IfExpr(if_expr) => {
                self.visit(&if_expr.condition);
                self.visit(&if_expr.then_branch);
                if let Some(else_branch) = &if_expr.else_branch {
                    self.visit(else_branch);
                }
            }

            Ast::CondExpr(cond) => {
                for arm in &cond.arms {
                    self.visit(&arm.condition);
                    self.visit(&arm.body);
                }
                self.visit(&cond.else_arm);
            }

            Ast::MatchExpr(match_expr) => {
                self.visit(&match_expr.value);
                for arm in &match_expr.arms {
                    self.visit(&arm.pattern);
                    self.visit(&arm.body);
                }
                self.visit(&match_expr.else_arm);
            }

            Ast::ExtendsExpr(extends) => {
                self.visit(&extends.lhs);
                self.visit(&extends.rhs);
                self.visit(&extends.then_branch);
                self.visit(&extends.else_branch);
            }

            // `?X` declares X; it is not a reference.
            Ast::Infer(_) => {}

            // Leaves with no type references inside.
            Ast::TypeNumber(_)
            | Ast::TypeString(_)
            | Ast::TemplateString(_)
            | Ast::Primitive(_, _)
            | Ast::NeverKeyword(_)
            | Ast::TrueKeyword(_)
            | Ast::FalseKeyword(_)
            | Ast::UnknownKeyword(_)
            | Ast::AnyKeyword(_)
            | Ast::NoOp(_)
            | Ast::UniqueSymbol(_)
            | Ast::UniqueSymbolDecl(_)
            | Ast::ImportStatement(_) => {}
        }
    }

    fn visit_property(&mut self, property: &crate::ast::ObjectProperty) {
        match &property.key {
            // `[S]: T` — a computed key references a declared unique symbol.
            PropertyName::ComputedPropertyName(key) => {
                self.visit(key);
                self.visit(&property.value);
            }
            // `[K in Iter]: T` — handled with scoping in Task 4.
            PropertyName::Index(index) => {
                self.visit(&index.iterable);
                if let Some(remap) = &index.remapped_as {
                    self.visit(remap);
                }
                self.visit(&property.value);
            }
            PropertyName::LiteralPropertyName(_) => self.visit(&property.value),
        }
    }
}
```

The `match` in `visit` is deliberately **exhaustive** (no `_` arm): a new
`Ast` variant must fail compilation here so the author decides how it scopes,
mirroring the `Ast::map` lesson in CLAUDE.md. If the compiler reports pattern
shapes that don't match (`Ast::Primitive(_, _)` arity, keyword variants), fix
the pattern to the actual variant definitions in `src/ast.rs:444-515`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run unresolved`
Expected: all Task 3 tests PASS.

- [ ] **Step 6: Format and commit**

```bash
cargo fmt
jj commit -m "Add static unresolved type-reference detection"
```

---

### Task 4: Binder scoping in the unresolved pass

**Files:**
- Modify: `src/ast/unresolved.rs`

**Interfaces:**
- Consumes/Produces: same public API as Task 3 (`unresolved_references`); this task only makes the visitor scope-aware for `infer`, mapped types, index signatures, `let`, and `match`.

- [ ] **Step 1: Write the failing binder tests**

Append inside the `tests` module of `src/ast/unresolved.rs`:

```rust
    #[test]
    fn type_params_are_in_scope_for_body_where_and_defaults() {
        let src = "type F(A, B) where A <: B defaults B = A as [A, B]";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn interface_params_are_in_scope() {
        assert_eq!(refs("interface Box(T) { value: T }"), vec![]);
    }

    #[test]
    fn infer_binds_in_if_condition_and_then_branch() {
        let src = "type Elem(T) as if T <: Array(?U) then U else never end";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn infer_does_not_leak_into_else_branch() {
        let src = "type Elem(T) as if T <: Array(?U) then U else U end";
        assert_eq!(refs(src), vec![("U".to_string(), 1)]);
    }

    #[test]
    fn match_arm_infer_binds_in_that_arm_only() {
        let src = "type F(T) as match T do Array(?U) -> U, else -> never end";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn cond_arm_infer_binds_in_that_arm() {
        let src = "type F(T) as cond do T <: Array(?U) -> U, else -> never end";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn mapped_type_index_binds_in_body_and_remap() {
        let src = "type M(O) as map K in keyof O do O[K] end";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn index_signature_key_binds_in_value() {
        let src = "type M(O) as { [K in keyof O]: O[K] }";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn let_bindings_are_in_scope_for_body_and_values() {
        let src = "type A as let a = 1, b = a in [a, b]";
        assert_eq!(refs(src), vec![]);
    }

    #[test]
    fn let_bindings_do_not_leak() {
        assert_eq!(refs("type A as [let a = 1 in a, a]"), vec![("a".to_string(), 1)]);
    }

    #[test]
    fn dot_access_rhs_is_not_a_reference() {
        // `T.foo`'s `foo` is a property name; only `T` must resolve.
        assert_eq!(refs("type A(T) as T.foo"), vec![]);
    }

    #[test]
    fn shadowing_resolves_to_the_inner_binder() {
        let src = "type T as 1\ntype F(T) as T";
        assert_eq!(refs(src), vec![]);
    }
```

**Syntax note:** confirm the concrete forms against the corpus before
debugging failures — `map K in … do … end` (`tests/corpus/typescript/map_expr/`),
`match … do pattern -> body, else -> … end`
(`tests/corpus/newtype/program/match_expands_to_ifs.txt`),
`let a = 1, b = 2 in …`
(`tests/corpus/newtype/program/multi_let_inlines.txt`), infer is `?U`
(`tests/conformance/conditionals.nt:55`), `cond do … end`
(`tests/corpus/newtype/program/cond_four_arms_expands.txt`). If `T.foo` or a
form above doesn't parse, adapt the test to a parseable equivalent (the
corpus is ground truth) rather than weakening the scoping assertion. If some
sugar (e.g. `where`+`defaults` together) parses differently, split the test
into two.

- [ ] **Step 2: Run tests to verify the scoping ones fail**

Run: `cargo nextest run unresolved`
Expected: the Task 3 tests still pass; the new binder tests FAIL (names like
`U`, `K`, `a` are reported because nothing scopes them yet). Tests that
already pass with the unscoped visitor (e.g. `type_params_…`, which Task 3
implemented) are fine — verify at minimum `infer_…`, `mapped_…`,
`index_signature_…`, `let_…`, and `match_arm_…` fail first.

- [ ] **Step 3: Add an infer-collection helper and scope the binder arms**

Add near `group` in `src/ast/unresolved.rs`:

```rust
/// The names declared by `?X` infer patterns anywhere inside `ast` (used to
/// scope a conditional's condition/pattern over its success branch).
fn infer_bindings(ast: &Ast) -> HashSet<String> {
    use std::cell::RefCell;

    let names = RefCell::new(HashSet::new());
    ast.prewalk((), &|node, ()| {
        if let Ast::Infer(inner) = &node {
            if let Ast::Ident(Ident { name, .. }) = inner.as_ref() {
                names.borrow_mut().insert(name.clone());
            }
        }
        (node, ())
    });
    names.into_inner()
}
```

Replace the Task 3 placeholder arms in `Collector::visit`:

```rust
            Ast::MappedType(mapped) => {
                self.visit(&mapped.iterable);
                self.scoped(HashSet::from([mapped.index.clone()]), |collector| {
                    if let Some(remap) = &mapped.remapped_as {
                        collector.visit(remap);
                    }
                    collector.visit(&mapped.body);
                });
            }

            Ast::LetExpr(let_expr) => {
                let names = let_expr.bindings.keys().cloned().collect();
                self.scoped(names, |collector| {
                    for value in let_expr.bindings.values() {
                        collector.visit(value);
                    }
                    collector.visit(&let_expr.body);
                });
            }

            Ast::IfExpr(if_expr) => {
                let infers = infer_bindings(&if_expr.condition);
                self.scoped(infers, |collector| {
                    collector.visit(&if_expr.condition);
                    collector.visit(&if_expr.then_branch);
                });
                if let Some(else_branch) = &if_expr.else_branch {
                    self.visit(else_branch);
                }
            }

            Ast::CondExpr(cond) => {
                for arm in &cond.arms {
                    let infers = infer_bindings(&arm.condition);
                    self.scoped(infers, |collector| {
                        collector.visit(&arm.condition);
                        collector.visit(&arm.body);
                    });
                }
                self.visit(&cond.else_arm);
            }

            Ast::MatchExpr(match_expr) => {
                self.visit(&match_expr.value);
                for arm in &match_expr.arms {
                    let infers = infer_bindings(&arm.pattern);
                    self.scoped(infers, |collector| {
                        collector.visit(&arm.pattern);
                        collector.visit(&arm.body);
                    });
                }
                self.visit(&match_expr.else_arm);
            }

            Ast::ExtendsExpr(extends) => {
                self.visit(&extends.lhs);
                let infers = infer_bindings(&extends.rhs);
                self.scoped(infers, |collector| {
                    collector.visit(&extends.rhs);
                    collector.visit(&extends.then_branch);
                });
                self.visit(&extends.else_branch);
            }
```

And in `visit_property`, scope the index-signature key:

```rust
            // `[K in Iter]: T` — the key is in scope for the remap and value.
            PropertyName::Index(index) => {
                self.visit(&index.iterable);
                self.scoped(HashSet::from([index.key.clone()]), |collector| {
                    if let Some(remap) = &index.remapped_as {
                        collector.visit(remap);
                    }
                    collector.visit(&property.value);
                });
            }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run unresolved`
Expected: all unresolved tests PASS. Then `cargo nextest run` — full suite PASS.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
jj commit -m "Scope binders in the unresolved-reference pass"
```

---

### Task 5: CLI wiring — warnings, `--deny-unresolved`, exit code

**Files:**
- Modify: `src/main.rs`
- Create: `tests/cli.rs`

**Interfaces:**
- Consumes: `newtype::ast::unresolved::unresolved_references` (Task 3/4), `newtype::report::{render_labeled, Severity}` (Task 1), `Ast::desugar_globals` (Task 2).
- Produces: the `newtype` binary emits warnings to stderr and honors `--deny-unresolved`.

- [ ] **Step 1: Write the failing integration test**

Create `tests/cli.rs` (uses Cargo's built-in `CARGO_BIN_EXE_<name>` env var —
no new dependencies):

```rust
//! End-to-end CLI checks for the unresolved-reference warnings.

use std::io::Write;
use std::process::{Command, Stdio};

/// Run the compiled `newtype` binary with `args`, feeding `source` on stdin.
/// Returns (exit_ok, stderr).
fn run(source: &str, args: &[&str]) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_newtype"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn the newtype binary");

    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(source.as_bytes())
        .expect("failed to write the program to stdin");

    let output = child.wait_with_output().expect("failed to wait for newtype");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn unresolved_reference_warns_but_exits_zero() {
    let (ok, stderr) = run("type A as Foo", &[]);
    assert!(ok, "warnings alone must not fail the run:\n{stderr}");
    assert!(stderr.contains("cannot resolve type `Foo`"), "{stderr}");
    assert!(stderr.contains("cannot be resolved to a definition"), "{stderr}");
}

#[test]
fn deny_unresolved_exits_nonzero() {
    let (ok, stderr) = run("type A as Foo", &["--deny-unresolved"]);
    assert!(!ok, "--deny-unresolved must fail the run:\n{stderr}");
    assert!(stderr.contains("cannot resolve type `Foo`"), "{stderr}");
}

#[test]
fn resolved_program_emits_no_warning() {
    let (ok, stderr) = run(
        "type Foo as 1\nunittest \"t\" do\n  assert Foo <: number\nend",
        &[],
    );
    assert!(ok, "{stderr}");
    assert!(!stderr.contains("cannot resolve type"), "{stderr}");
}

#[test]
fn deny_unresolved_with_clean_program_exits_zero() {
    let (ok, stderr) = run("type Foo as 1\ntype A as Foo", &["--deny-unresolved"]);
    assert!(ok, "{stderr}");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run --test cli`
Expected: `deny_unresolved_exits_nonzero` FAILS (unknown flag) and
`unresolved_reference_warns_but_exits_zero` FAILS (no warning emitted).

- [ ] **Step 3: Wire the pass into `src/main.rs`**

Add the flag to `Args` (after `fail_fast`):

```rust
    /// Treat unresolved type references as errors: render them with error
    /// severity and exit non-zero (evaluation and rendering still run).
    #[clap(long)]
    deny_unresolved: bool,
```

In `main`, inside the `Ok(ast)` arm, after the `validate` block and **before**
`let simplified = ast.simplify();`, replace that line with:

```rust
            // Rewrite global sugar aliases (`Array(T)` → `T[]`, …) before the
            // unresolved pass so those spellings never count as references,
            // then report every type reference the file can't resolve.
            // Warnings never block evaluation or rendering; with
            // `--deny-unresolved` they turn the exit code non-zero below.
            let desugared = ast.desugar_globals();

            let unresolved = newtype::ast::unresolved::unresolved_references(&desugared);
            let severity = if args.deny_unresolved {
                newtype::report::Severity::Error
            } else {
                newtype::report::Severity::Warning
            };
            for reference in &unresolved {
                let labels: Vec<_> = reference
                    .spans
                    .iter()
                    .map(|span| (*span, "cannot be resolved to a definition".to_string()))
                    .collect();
                eprintln!(
                    "{}",
                    newtype::report::render_labeled(
                        severity,
                        &source_name,
                        input,
                        &format!("cannot resolve type `{}`", reference.name),
                        &labels,
                        true,
                    )
                );
            }

            let simplified = desugared.simplify();
```

And extend the exit-code decision at the bottom of the `Ok` arm:

```rust
            // Non-zero exit on any assertion failure — or, with
            // `--deny-unresolved`, on any unresolved reference — after
            // rendering completes.
            if report.has_failures() || (args.deny_unresolved && !unresolved.is_empty()) {
                std::process::exit(1);
            }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo nextest run --test cli`
Expected: all 4 PASS. Then `cargo nextest run` — full suite PASS.

- [ ] **Step 5: Format and commit**

```bash
cargo fmt
jj commit -m "Warn on unresolved type references; add --deny-unresolved"
```

---

### Task 6: End-to-end verification and example cleanup

**Files:**
- Modify: `examples/ts-toolbelt.nt` (stale TODO comments, lines 43-45)

**Interfaces:**
- Consumes: everything above. No new interfaces.

- [ ] **Step 1: Run the motivating example**

```bash
cargo build
target/debug/newtype --input examples/ts-toolbelt.nt > /dev/null
```

Expected on stderr: `unittest "At(A, K)"` with `ok      At(User, 'id') <: number`
(the `ReadonlyArray(A)` body now desugars to `readonly A[]`, so `List`
resolves and the claim evaluates definitively) and **no**
`cannot resolve type` warnings.

If the assert still fails as indeterminate or now fails as `false`, stop and
investigate the assignability of object types vs `readonly any[]`
(`src/ast/assignability.rs`) before touching the example — that would be an
engine bug this feature has surfaced, and it should be reported back to the
user, not papered over.

- [ ] **Step 2: Update the example's stale TODOs**

In `examples/ts-toolbelt.nt`, delete the two lines that are now implemented
and keep the one that isn't:

```
// TODO allow for types in unittest blocks that aren't emitted
```

(i.e. remove `// TODO raise on unresolved types` and
`// TODO include built-in types`, and un-comment nothing else — the
commented-out `// type ReadonlyArray(T) as T[]` on line 35 can also be
deleted since the engine now desugars `ReadonlyArray` natively.)

- [ ] **Step 3: Full test suite and conformance**

```bash
cargo nextest run
mise run tc
```

Expected: nextest fully green; conformance reports **no `DISAGREE` rows**
(the desugared `Array(?U)` → `(?U)[]` forms must still agree with tsgo). If
conformance shows a new divergence, the desugar rewrite changed evaluation
semantics — fix the rewrite (Task 2), don't adjust the conformance fixtures.

- [ ] **Step 4: Format and commit**

```bash
cargo fmt
jj commit -m "Verify unresolved warnings end to end; clean up ts-toolbelt example"
```

---

### Task 7: `--exact-optional-property-types` flag

**Files:**
- Modify: `src/ast/type_env.rs` (`ResolveCtx`, line ~416)
- Modify: `src/ast/assignability.rs` (`property_relation`, line ~831)
- Modify: `src/test_harness.rs` (`Config`, `run`, `evaluate`)
- Modify: `src/main.rs` (flag)
- Modify: `tests/cli.rs` (integration test)

**Interfaces:**
- Consumes: `ResolveCtx::{empty, new}` (`src/ast/type_env.rs`), `test_harness::Config { fail_fast }`, the Task 5 CLI test harness `run(source, args)` in `tests/cli.rs`.
- Produces: `ResolveCtx::with_exact_optional_property_types(self, bool) -> Self` and `ResolveCtx::exact_optional_property_types(&self) -> bool`; `test_harness::Config` gains `pub exact_optional_property_types: bool`.

Semantics (TS-accurate; the default already matches tsgo `--strict`):
- Default: an optional target `x?: T` accepts a source property typed `T | undefined` (including plain `undefined`) — the existing widening at `src/ast/assignability.rs:842`.
- With the flag: the widening is disabled; the source property type must be assignable to `T` itself.
- Unchanged in both modes: an optional *source* property is never assignable to a required target (`{x?: T} <: {x: T | undefined}` stays false), and a source may omit an optional target property.

- [ ] **Step 1: Write the failing harness tests**

Append to the `tests` module in `src/test_harness.rs`:

```rust
    /// Like `run_src` but with `exact_optional_property_types` enabled.
    fn run_src_exact(src: &str) -> (Report, String) {
        let program = parse_newtype_program(src).unwrap().simplify();
        let mut out = Vec::new();
        let report = run(
            &program,
            src,
            "<test>",
            Config {
                exact_optional_property_types: true,
                ..Config::default()
            },
            &mut out,
        )
        .unwrap();
        (report, String::from_utf8(out).unwrap())
    }

    #[test]
    fn optional_target_accepts_undefined_by_default() {
        let src = "unittest \"t\" do\n\
            \x20 assert { x: number | undefined } <: { x?: number }\n\
            \x20 assert { x: undefined } <: { x?: number }\n\
            \x20 assert { x: number } <: { x?: number }\n\
            \x20 assert {} <: { x?: number }\n\
            \x20 assert not ({ x?: number } <: { x: number | undefined })\n\
            end";
        let (report, out) = run_src(src, false);
        assert_eq!(report, Report { passed: 5, failed: 0 }, "{out}");
    }

    #[test]
    fn exact_optional_rejects_undefined_sources() {
        let src = "unittest \"t\" do\n\
            \x20 assert not ({ x: number | undefined } <: { x?: number })\n\
            \x20 assert not ({ x: undefined } <: { x?: number })\n\
            \x20 assert { x: number } <: { x?: number }\n\
            \x20 assert {} <: { x?: number }\n\
            \x20 assert not ({ x?: number } <: { x: number | undefined })\n\
            end";
        let (report, out) = run_src_exact(src);
        assert_eq!(report, Report { passed: 5, failed: 0 }, "{out}");
    }
```

(Optional-property syntax `x?: number` is exercised by
`tests/corpus/typescript/object_literal/optional_modifier_postfix.txt`; if the
inline object spelling differs, mirror that fixture.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run optional_target_accepts exact_optional_rejects`
Expected: compile error — `Config` has no field `exact_optional_property_types`.

- [ ] **Step 3: Thread the flag through `Config` and `ResolveCtx`**

`src/test_harness.rs` — extend `Config`:

```rust
/// Configuration for a harness run.
#[derive(Debug, Clone, Copy, Default)]
pub struct Config {
    /// Stop at the first failing assertion instead of evaluating the rest.
    pub fail_fast: bool,
    /// Mirror TypeScript's `exactOptionalPropertyTypes`: an optional target
    /// property `x?: T` no longer accepts `T | undefined` sources.
    pub exact_optional_property_types: bool,
}
```

Pass the config into `evaluate` — change the signature and the call site in `run`:

```rust
            match evaluate(&assert.claim, &env, config) {
```

```rust
fn evaluate(claim: &Ast, env: &TypeEnv, config: Config) -> Outcome {
```

and inside `evaluate`, where the context is built (line ~203):

```rust
    let ctx = ResolveCtx::new(env)
        .with_exact_optional_property_types(config.exact_optional_property_types);
```

`src/ast/type_env.rs` — add the field to `ResolveCtx` and default it to
`false` in **both** constructors (`empty()` line ~424 and `new()` just below):

```rust
pub struct ResolveCtx<'a> {
    env: Option<&'a TypeEnv>,
    assumptions: RefCell<HashSet<(String, String)>>,
    exact_optional_property_types: bool,
}
```

```rust
            exact_optional_property_types: false,
```

(one line added inside each constructor's struct literal), plus:

```rust
    /// Mirror TypeScript's `exactOptionalPropertyTypes` for assignability
    /// checks made through this context.
    pub fn with_exact_optional_property_types(mut self, on: bool) -> Self {
        self.exact_optional_property_types = on;
        self
    }

    pub fn exact_optional_property_types(&self) -> bool {
        self.exact_optional_property_types
    }
```

If `ResolveCtx` has other constructors or `Clone`/derive impls the compiler
flags, initialize the new field to `false` there too (it must be inherited
wherever an existing ctx is passed along — the field travels with the
borrowed ctx, so recursive checks need no changes).

`src/ast/assignability.rs` — gate the widening in `property_relation`
(line ~842):

```rust
        // An optional target property `x?: T` has effective type `T | undefined`
        // under --strict without `exactOptionalPropertyTypes`, so the source
        // value need only be assignable to `T | undefined`. With
        // `exactOptionalPropertyTypes` the widening is disabled and the source
        // must be assignable to `T` itself.
        if target.optional && !ctx.exact_optional_property_types() {
```

(the body of the `if` is unchanged).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run optional_target_accepts exact_optional_rejects`
Expected: both PASS. Then `cargo nextest run` — full suite PASS (the default
path is behavior-identical).

- [ ] **Step 5: Add the CLI flag and integration test**

`src/main.rs` — add to `Args` after `deny_unresolved`:

```rust
    /// Mirror TypeScript's `exactOptionalPropertyTypes`: an optional property
    /// `x?: T` no longer accepts `T | undefined` sources in assertions.
    #[clap(long)]
    exact_optional_property_types: bool,
```

and extend the harness config in `main`:

```rust
                test_harness::Config {
                    fail_fast: args.fail_fast,
                    exact_optional_property_types: args.exact_optional_property_types,
                },
```

Append to `tests/cli.rs`:

```rust
#[test]
fn exact_optional_property_types_flag_changes_optional_assignability() {
    let src = "unittest \"t\" do\n  assert { x: number | undefined } <: { x?: number }\nend";
    let (ok_default, _) = run(src, &[]);
    assert!(ok_default, "widening holds by default");
    let (ok_exact, stderr) = run(src, &["--exact-optional-property-types"]);
    assert!(!ok_exact, "the widened source must be rejected:\n{stderr}");
}
```

Run: `cargo nextest run --test cli`
Expected: all cli tests PASS.

- [ ] **Step 6: Conformance sanity check**

```bash
mise run tc
```

Expected: no `DISAGREE` rows — the default path matches tsgo `--strict`
(which does not enable `exactOptionalPropertyTypes`), and the flag is off in
the conformance harness.

- [ ] **Step 7: Format and commit**

```bash
cargo fmt
jj commit -m "Add --exact-optional-property-types flag"
```
