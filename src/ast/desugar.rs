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

use std::rc::Rc;

use crate::ast::{
    ApplyGeneric, Assert, Ast, Builtin, BuiltinKeyword, Ident, Interface, PrimitiveType, UnionType,
    UnitTest,
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
                params: interface
                    .params
                    .iter()
                    .map(|param| param.map(|ty| ty.desugar_globals()))
                    .collect(),
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
        assert_eq!(
            desugar_ts("type A as Array(number)"),
            "type A = number[];\n\n"
        );
    }

    #[test]
    fn readonly_array_becomes_readonly_array_type() {
        assert_eq!(
            desugar_ts("type A as ReadonlyArray(number)"),
            "type A = readonly number[];\n\n"
        );
    }

    #[test]
    fn readonly_of_tuple_becomes_readonly_tuple() {
        assert_eq!(
            desugar_ts("type A as Readonly([1, 2])"),
            "type A = readonly [1, 2];\n\n"
        );
    }

    #[test]
    fn readonly_of_object_is_left_alone() {
        assert_eq!(
            desugar_ts("type A as Readonly({a: 1})"),
            "type A = Readonly<{a: 1}>;\n\n"
        );
    }

    #[test]
    fn array_with_wrong_arity_is_left_alone() {
        assert_eq!(
            desugar_ts("type A as Array(1, 2)"),
            "type A = Array<1, 2>;\n\n"
        );
    }

    #[test]
    fn keyof_any_becomes_key_union() {
        assert_eq!(
            desugar_ts("type A as keyof any"),
            "type A = string | number | symbol;\n\n"
        );
    }

    #[test]
    fn nested_sugar_desugars_bottom_up() {
        assert_eq!(
            desugar_ts("type A as ReadonlyArray(Array(number))"),
            "type A = readonly number[][];\n\n"
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
    fn desugars_inside_interface_type_parameter_constraints() {
        let ts = desugar_ts("interface I(T) where T <: Array(number) { x: T }");
        assert!(ts.contains("T extends number[]"), "{ts}");
    }

    #[test]
    fn array_of_infer_is_parenthesized_in_conditional() {
        use crate::typescript::Pretty;
        let src = "type ElemA(T) as if T <: Array(?U) then U else never end";
        let ts = crate::parser::parse_newtype_program(src)
            .unwrap()
            .simplify()
            .render_pretty_ts(120);
        assert!(ts.contains("(infer U)[]"), "{ts}");
    }

    #[test]
    fn array_of_function_type_is_parenthesized() {
        assert_eq!(
            desugar_ts("type A as Array(() => void)"),
            "type A = (() => void)[];\n\n"
        );
    }

    #[test]
    fn array_of_keyof_is_parenthesized() {
        let ts = desugar_ts("type A as Array(keyof {a: 1})");
        assert!(ts.contains("(keyof"), "{ts}");
        assert!(ts.contains(")[]"), "{ts}");
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
