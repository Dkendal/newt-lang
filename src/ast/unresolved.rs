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
        assert_eq!(
            refs("import { Foo } from \"./m.nt\"\ntype A as Foo"),
            vec![]
        );
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
