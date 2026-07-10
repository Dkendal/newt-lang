//! Pending tests for features that are **not yet implemented** in the parser /
//! renderer / simplifier.
//!
//! Every test here is `#[ignore]`d, so a normal `cargo test` run stays green.
//! They are executable specifications you can use to drive — and verify — a
//! future implementation:
//!
//! ```text
//! cargo test --test pending -- --ignored        # run them all
//! cargo test --test pending typeof -- --ignored  # run one group
//! ```
//!
//! Each currently fails (it panics on a `todo!()`/`unreachable!()`, or asserts
//! output the parser silently drops today). When the corresponding feature is
//! implemented, its test should go green; remove the `#[ignore]` then — or, even
//! better, promote the case into the corpus under `tests/corpus/`.
//!
//! Two assertion styles are used:
//!
//! * [`assert_renders_like`] — the expected TypeScript is well-defined (the
//!   renderer already supports it; only the parser/lowering is missing). The
//!   comparison ignores insignificant whitespace so it pins behaviour without
//!   being brittle about exact spacing.
//! * [`assert_renders_ok`] — the *semantics* are an open design decision
//!   (there is no specified output yet), so we only assert the pipeline runs to
//!   a non-empty result. Tighten this into an `assert_renders_like` once the
//!   intended output is decided.

use newtype::parser::Rule;

#[macro_use]
mod common;

/// Collapse runs of whitespace so comparisons aren't brittle about spacing.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Assert `source` (parsed from `rule`) renders to `expected`, ignoring
/// insignificant whitespace.
fn assert_renders_like(rule: Rule, source: &str, expected: &str) {
    let actual = newtype::corpus::render(rule, source.trim());
    pretty_assertions::assert_eq!(
        norm(expected),
        norm(&actual),
        "\n  source:   {}\n  rendered: {}",
        source.trim(),
        actual.trim()
    );
}

/// Assert `source` (parsed from `rule`) renders to *something* without panicking.
/// Use for features whose exact output is still an open question.
fn assert_renders_ok(rule: Rule, source: &str) {
    let actual = newtype::corpus::render(rule, source.trim());
    assert!(
        !actual.trim().is_empty(),
        "expected a non-empty render for: {}",
        source.trim()
    );
}

/// `interface Foo extends Bar { ... }`. The renderer already emits the
/// `extends` clause (src/ast/pretty.rs), but `parse_interface` hardcodes
/// `extends = None` (src/parser.rs), so the clause is parsed then dropped — the
/// interface renders today as `interface Foo {}`.
mod interface_extends {
    use super::*;

    #[test]
    fn empty_body() {
        assert_renders_like(
            Rule::interface,
            "interface Foo extends Bar {}",
            "interface Foo extends Bar {}",
        );
    }

    #[test]
    fn with_members() {
        assert_renders_like(
            Rule::interface,
            "interface Foo extends Bar { x: 1 }",
            "interface Foo extends Bar { x: 1; }",
        );
    }
}

/// Namespace imports: `import * as Foo from 'a'`. The renderer supports
/// `ImportClause::Namespace` (`* as <alias>`), but the parser lowering panics
/// with "not yet implemented" (src/parser.rs).
mod namespace_import {
    use super::*;

    #[test]
    fn star_as_alias() {
        assert_renders_like(
            Rule::program,
            "import * as Foo from 'a'",
            "import type * as Foo from 'a';",
        );
    }
}

/// Mapped-type modifiers. The `MappedType` renderer already handles
/// `readonly` / optional `?` / `as` remap (src/ast/pretty.rs), but
/// `parse_map_expr` hardcodes them to `None` (src/parser.rs), so they are
/// parsed then dropped — these render today as a plain `{ [k in t]: 1 }`.
mod map_expr_modifiers {
    use super::*;

    #[test]
    fn readonly() {
        assert_renders_like(
            Rule::map_expr,
            "map readonly k in t do 1 end",
            "{ readonly [k in t]: 1 }",
        );
    }

    #[test]
    fn optional() {
        assert_renders_like(
            Rule::map_expr,
            "map optional k in t do 1 end",
            "{ [k in t]?: 1 }",
        );
    }

    #[test]
    fn remap() {
        assert_renders_like(
            Rule::map_expr,
            "map k in t as r do 1 end",
            "{ [k in t as r]: 1 }",
        );
    }
}

/// Equality operators in extends conditions: `=`, `!=`, `==`, `!==`.
/// `expand_to_extends` (src/ast/if_expr.rs) lowers these via *mutual
/// assignability*: `a = b` becomes `(a extends b) and (b extends a)`, and
/// `a != b` becomes `(a not-extends b) or (b not-extends a)`. The strict forms
/// (`==`/`!==`) currently map identically to the loose forms — TypeScript's type
/// level can't distinguish them without the function-identity trick, which the
/// current AST can't express cleanly.
mod equality_operators {
    use super::*;

    #[test]
    fn equals() {
        assert_renders_like(
            Rule::if_expr,
            "if a = b then c else d end",
            "a extends b ? b extends a ? c : d : d",
        );
    }

    #[test]
    fn not_equals() {
        assert_renders_like(
            Rule::if_expr,
            "if a != b then c else d end",
            "a extends b ? b extends a ? d : c : c",
        );
    }

    // `==` maps identically to `=` (see module doc).
    #[test]
    fn strict_equals() {
        assert_renders_like(
            Rule::if_expr,
            "if a == b then c else d end",
            "a extends b ? b extends a ? c : d : d",
        );
    }

    // `!==` maps identically to `!=` (see module doc).
    #[test]
    fn strict_not_equals() {
        assert_renders_like(
            Rule::if_expr,
            "if a !== b then c else d end",
            "a extends b ? b extends a ? d : c : c",
        );
    }
}

/// Macro calls (`name!(...)`). `Ast::MacroCall` is `todo!()` in src/runtime.rs
/// and `unreachable!()` in the renderer, so any macro panics during
/// simplify/render. `unquote!` has a defined intent (evaluate its argument; see
/// the `#[ignore]`d `unquote::evaluates_expression` test in tests/parser.rs);
/// the others have no specified output yet.
mod macro_calls {
    use super::*;

    #[ignore = "macro expansion unimplemented: Ast::MacroCall is todo!() in src/runtime.rs"]
    #[test]
    fn unquote_evaluates_argument() {
        // Intended: unquote! evaluates its argument at compile time.
        assert_renders_like(Rule::expr, "unquote!(1)", "1");
    }

    #[ignore = "macro expansion unimplemented: Ast::MacroCall is todo!() in src/runtime.rs; output undecided"]
    #[test]
    fn generic_macro_call_runs() {
        assert_renders_ok(Rule::expr, "dbg!(A)");
    }
}

// NOTE: the `unittest_statement` pending module has been removed: a `unittest`
// block now parses (`assert`-led body) and renders to nothing, with no stray
// `;`. That behavior is locked in by the corpus fixture
// `tests/corpus/typescript/program/unittest_emits_nothing.txt`.

// NOTE: the `subtype_engine` pending module (Access / ApplyGeneric / Array /
// Builtin / Path / non-empty TypeLiteral / Ident / Tuple LHS) has been removed:
// the assignability engine (src/ast/assignability.rs, formerly subtype.rs) now
// implements all of those arms. Their behavior is locked in by
// `assignability_tests::is_assignable_to_extended` in tests/ast.rs.

// NOTE: the `known_bugs` pending module has been removed: the dot-then-indexed
// access bug (`A.b[C]` panicking in the renderer) was fixed by giving the
// indexed-access postfix the same binding power as `.`, and the case now lives
// in the corpus as tests/corpus/typescript/expr/access_dot_then_index.txt. The
// intersection-of-unions parenthesisation bugs were fixed earlier; their cases
// live in tests/corpus/typescript/expr/intersection_of_unions.txt and
// union_in_intersection.txt.
