use std::fmt::Display;

use pretty::RcDoc as D;

use crate::{
    ast::{
        cond_expr, match_expr, Access, ApplyGeneric, Ast, Builtin, BuiltinKeyword, ExtendsExpr,
        ExtendsInfixOp, ExtendsPrefixOp, FunctionType, Ident, ImportClause, ImportSpecifier,
        ImportStatement, InfixOp, Interface, IntersectionType, MappedType, MappingModifier,
        ObjectProperty, Parameter, Path, PrimitiveType, Program, PropertyKeyIndex, PropertyName,
        Tuple, TypeAlias, TypeLiteral, TypeParameter, UnionType,
    },
    pretty::{parens, string_literal, surround},
    typescript,
};

/// Render the object side of an indexed access `O[K]`, parenthesizing `O` when
/// it is a lower-precedence type constructor. An index access binds tighter than
/// a union/intersection (`(X | Y)["k"]` must keep its parens, else it reparses as
/// `X | Y["k"]`), a function type, a conditional, a `keyof`/builtin operator
/// (`(keyof A)["x"]` would reparse as `keyof (A["x"])`), or a `readonly` array,
/// so those operands are wrapped — mirroring the `Ast::Array` render arm.
fn access_object_doc(lhs: &Ast) -> D<'_, ()> {
    let needs_parens = lhs.is_set_op()
        || matches!(
            lhs,
            Ast::FunctionType(_)
                | Ast::ExtendsExpr(_)
                | Ast::Infer(_)
                | Ast::Builtin(_)
                | Ast::Readonly(_)
        );
    if needs_parens {
        parens(lhs.to_ts())
    } else {
        lhs.to_ts()
    }
}

pub trait Pretty {
    fn render_pretty_ts(&self, width: usize) -> String {
        let mut w = Vec::new();
        self.to_ts().render(width, &mut w).unwrap();
        String::from_utf8(w).unwrap()
    }

    fn to_ts(&self) -> ::pretty::RcDoc<'_, ()>;
}

impl typescript::Pretty for ApplyGeneric {
    fn to_ts(&self) -> D<'_, ()> {
        let sep = D::text(",").append(D::space());

        let generic_inner = D::intersperse(self.args.iter().map(|param| param.to_ts()), sep);

        let generic_params = D::text("<").append(generic_inner).append(D::text(">"));

        self.receiver.to_ts().append(generic_params)
    }
}

impl typescript::Pretty for TypeLiteral {
    fn to_ts(&self) -> D<'_, ()> {
        let props = &self.properties;

        let sep = D::text(",").append(D::line());

        let props = D::intersperse(props.iter().map(|prop| prop.to_ts()), sep);

        D::nil()
            .append("{")
            .append(D::line_())
            .append(props.nest(4))
            .append(D::line_())
            .append(D::text("}"))
            .group()
    }
}

impl typescript::Pretty for Interface {
    fn to_ts(&self) -> D<'_, ()> {
        let Interface {
            export,
            name,
            extends,
            params,
            definition,
            ..
        } = self;

        let doc = if *export {
            D::text("export").append(D::space())
        } else {
            D::nil()
        };

        let extends = match extends {
            Some(extends) => D::space()
                .append("extends")
                .append(D::space())
                .append(extends.as_ref().to_ts()),
            None => D::nil(),
        };

        let params_doc = match params {
            list if list.is_empty() => D::nil(),
            list => {
                let seperator = D::text(",").append(D::line());

                let params_body =
                    D::intersperse(list.iter().map(|param| param.to_ts().group()), seperator);

                D::text("<")
                    .append(D::line_().append(params_body).append(D::line_()).nest(4))
                    .append(D::text(">"))
                    .group()
            }
        };

        let body = if definition.is_empty() {
            D::text("{}")
        } else {
            let body = definition.iter().map(|p| p.to_ts());

            let body = D::intersperse(body, D::text(";").append(D::hardline())).append(";");

            let body = D::hardline().append(body).nest(4);

            D::nil()
                .append("{")
                .append(body)
                .append(D::hardline())
                .append("}")
        };

        doc.append("interface")
            .append(D::space())
            .append(name)
            .append(params_doc)
            .append(extends)
            .append(D::space())
            .append(body)
            .group()
    }
}

impl typescript::Pretty for FunctionType {
    fn to_ts(&self) -> D<'_, ()> {
        let sep = D::text(",").append(D::space());

        let params = D::intersperse(self.params.iter().map(|param| param.to_ts()), sep);

        let params = D::text("(").append(params).append(D::text(")")).group();

        let return_type = self.return_type.to_ts();

        params
            .append(D::space())
            .append("=>")
            .append(D::space())
            .append(return_type)
    }
}

impl typescript::Pretty for Parameter {
    fn to_ts(&self) -> D<'_, ()> {
        let kind = self.kind.to_ts();

        if self.ellipsis {
            D::text("...")
        } else {
            D::nil()
        }
        .append(self.name.clone())
        .append(":")
        .append(D::space())
        .append(kind)
    }
}

impl typescript::Pretty for Ast {
    fn to_ts(&self) -> D<'_, ()> {
        match self {
            Ast::Program(Program { statements, .. }) => {
                let mut doc = D::nil();
                for stmnt in statements {
                    // Compile-time-only statements (e.g. `unittest`) emit nothing
                    // and must not leave a stray `;` or blank lines behind.
                    if stmnt.is_zero_output() {
                        continue;
                    }
                    doc = doc
                        .append(stmnt.to_ts())
                        .append(D::hardline())
                        .append(D::hardline());
                }
                doc
            }
            Ast::TypeAlias(TypeAlias {
                export,
                name,
                params,
                body,
                ..
            }) => {
                let body = (*body).to_ts();

                let doc = if *export {
                    D::text("export").append(D::space())
                } else {
                    D::nil()
                };

                let params_doc = match params {
                    list if list.is_empty() => D::nil(),
                    list => {
                        let seperator = D::text(",").append(D::line());

                        let body = D::intersperse(
                            list.iter().map(|param| param.to_ts().group()),
                            seperator,
                        );

                        D::text("<")
                            .append(D::line_().append(body).append(D::line_()).nest(4))
                            .append(D::text(">"))
                            .group()
                    }
                };

                doc.append("type")
                    .append(D::space())
                    .append(name.pretty())
                    .append(params_doc)
                    .append(D::space())
                    .append("=")
                    .append(D::line().append(body).nest(4))
                    .group()
            }
            Ast::Ident(identifier) => identifier.pretty(),
            Ast::TypeNumber(inner) => D::text(inner.ty.clone()),
            Ast::Primitive(primitive, _) => D::text(primitive.to_string()),
            Ast::TypeString(inner) => string_literal(inner.ty.as_str()),
            Ast::TemplateString(inner) => D::text(inner.ty.clone()),
            Ast::IfExpr(..) => {
                unreachable!("IfExpr should be desugared before this point");
            }
            Ast::Access(Access {
                lhs,
                rhs,
                is_dot: true,
                ..
            }) => {
                let rhs = rhs
                    .as_ident()
                    .expect("rhs of dot access should be an ident");

                access_object_doc(lhs)
                    .append(D::text("["))
                    .append(string_literal(rhs.name.as_str()))
                    .append(D::text("]"))
                    .group()
            }
            Ast::Access(Access { lhs, rhs, .. }) => access_object_doc(lhs)
                .append(D::text("["))
                .append(rhs.to_ts())
                .append(D::text("]"))
                .group(),
            Ast::TypeLiteral(value) => value.to_ts(),
            Ast::ApplyGeneric(value) => value.to_ts(),
            Ast::Tuple(Tuple { items, .. }) => {
                let sep = D::text(",").append(D::space());

                let items = D::intersperse(items.iter().map(|item| item.to_ts()), sep);

                D::text("[").append(items).append(D::text("]"))
            }
            Ast::Array(node) => {
                let needs_parens = node.is_set_op()
                    || matches!(
                        node.as_ref(),
                        Ast::Infer(_)
                            | Ast::FunctionType(_)
                            | Ast::ExtendsExpr(_)
                            | Ast::Builtin(_)
                            | Ast::Readonly(_)
                    );

                let doc = if needs_parens {
                    parens(node.to_ts())
                } else {
                    node.to_ts()
                };

                doc.append(D::text("[]"))
            }
            Ast::Readonly(node) => D::text("readonly").append(D::space()).append(node.to_ts()),
            Ast::NeverKeyword(_) => D::text("never"),
            Ast::AnyKeyword(_) => D::text("any"),
            Ast::UnknownKeyword(_) => D::text("unknown"),
            Ast::TrueKeyword(_) => D::text("true"),
            Ast::FalseKeyword(_) => D::text("false"),
            Ast::Infer(value) => D::text("infer").append(D::space()).append(value.to_ts()),

            Ast::Builtin(Builtin { name, argument, .. }) => {
                // `keyof` (and the other type operators) bind tighter than a
                // union/intersection, function type, or conditional, so a
                // low-precedence operand must keep its parens — else
                // `keyof (A | B)` reparses as `(keyof A) | B`.
                let arg_doc = if argument.is_set_op()
                    || matches!(
                        argument.as_ref(),
                        Ast::FunctionType(_) | Ast::ExtendsExpr(_) | Ast::Infer(_)
                    ) {
                    parens(argument.to_ts())
                } else {
                    argument.to_ts()
                };
                name.to_ts().append(" ").append(arg_doc)
            }

            Ast::ExtendsExpr(ExtendsExpr {
                lhs,
                rhs,
                then_branch: then,
                else_branch: els,
                ..
            }) => {
                let condition_doc = lhs
                    .to_ts()
                    .append(D::space())
                    .append("extends")
                    .append(D::space())
                    .append(rhs.to_ts());

                let then_doc = D::line()
                    .append("?")
                    .append(D::space())
                    .append(then.to_ts())
                    .nest(4);

                let else_doc = D::line()
                    .append(":")
                    .append(D::space())
                    .append(els.to_ts())
                    .nest(4);

                condition_doc.append(then_doc).append(else_doc)
            }
            Ast::ExtendsInfixOp(ExtendsInfixOp {
                lhs,
                op: InfixOp::Extends,
                rhs,
                ..
            }) => lhs
                .to_ts()
                .append(D::space())
                .append("extends")
                .append(D::space())
                .append(rhs.to_ts())
                .group(),
            Ast::Statement(stmnt) if stmnt.is_zero_output() => stmnt.to_ts(),
            Ast::Statement(stmnt) => stmnt.to_ts().append(D::text(";")),
            Ast::MappedType(MappedType {
                index: key,
                iterable,
                remapped_as,
                readonly_mod,
                optional_mod,
                body,
                ..
            }) => {
                let remapped_as_doc = match remapped_as {
                    Some(remapped_as) => D::space()
                        .append("as")
                        .append(D::space())
                        .append(remapped_as.to_ts()),
                    None => D::nil(),
                };

                let lhs_doc = D::nil()
                    .append(key)
                    .append(D::space())
                    .append("in")
                    .append(D::space())
                    .append(iterable.to_ts())
                    .append(remapped_as_doc)
                    .group();

                let rhs_doc = body.to_ts();

                let rhs_doc = D::line().append(rhs_doc).nest(4).group();

                let readonly_doc = match readonly_mod {
                    Some(MappingModifier::Add) => D::text("readonly").append(D::space()),
                    Some(MappingModifier::Remove) => D::text("-readonly").append(D::space()),
                    None => D::nil(),
                };

                let optional_doc = match optional_mod {
                    Some(MappingModifier::Add) => D::text("?"),
                    Some(MappingModifier::Remove) => D::text("-?"),
                    None => D::nil(),
                };

                let inner_doc = D::line()
                    .append(readonly_doc)
                    .append("[")
                    .append(lhs_doc)
                    .append("]")
                    .append(optional_doc)
                    .append(":")
                    .append(rhs_doc)
                    .append(D::line())
                    .nest(4)
                    .group();

                D::nil().append("{").append(inner_doc).append("}").group()
            }
            Ast::LetExpr(..) => {
                unreachable!("LetExpr should be desugared before this point")
            }
            Ast::ImportStatement(ImportStatement {
                import_clause,
                module,
                ..
            }) => {
                let import_clause = import_clause.to_ts();

                D::text("import type")
                    .append(D::space())
                    .append(import_clause)
                    .append(D::space())
                    .append("from")
                    .append(D::space())
                    .append(string_literal(module))
            }
            Ast::Path(Path { segments, .. }) => {
                let sep = D::text(".");

                let segments = D::intersperse(segments.iter().map(|seg| seg.to_ts()), sep);

                segments
            }
            Ast::Interface(value) => {
                return value.to_ts();
            }
            Ast::UnitTest(_) => D::nil(),
            Ast::Assert(_) => {
                unreachable!("Assert only appears inside a unittest body, which is never rendered")
            }
            Ast::MacroCall(_) => unreachable!("MacroCall should be desugared before this point"),
            Ast::UnionType(UnionType { types, .. }) => {
                let sep = D::line().append(D::text("|")).append(D::space());
                D::intersperse(
                    types.iter().map(|t| match t {
                        Ast::IntersectionType(IntersectionType { .. }) => {
                            surround(t.to_ts(), "(", ")")
                        }
                        _ => t.to_ts(),
                    }),
                    sep,
                )
                .group()
            }
            Ast::IntersectionType(IntersectionType { types, .. }) => {
                let sep = D::line().append(D::text("&")).append(D::space());
                D::intersperse(
                    types.iter().map(|t| match t {
                        // `&` binds tighter than `|`, so a union member must be
                        // parenthesised or it changes meaning (`(A | B) & C`
                        // would otherwise print as `A | B & C` = `A | (B & C)`).
                        Ast::UnionType(UnionType { .. }) => surround(t.to_ts(), "(", ")"),
                        _ => t.to_ts(),
                    }),
                    sep,
                )
                .group()
            }
            Ast::NoOp(_) => D::nil(),
            // A reference to a declared unique symbol, used as a type.
            Ast::UniqueSymbol(sym) => D::text(format!("typeof {}", sym.name)),
            // The declaration. `unique symbol` is only legal in TS on a `const`
            // (a bare `type X = unique symbol` is an error), so emit a
            // `declare const` that the `typeof <name>` references resolve to.
            Ast::UniqueSymbolDecl(sym) => {
                D::text(format!("declare const {}: unique symbol", sym.name))
            }
            Ast::FunctionType(ty) => ty.to_ts(),
            node @ (Ast::ExtendsPrefixOp(_)
            | Ast::MatchExpr(_)
            | Ast::CondExpr(_)
            | Ast::ExtendsInfixOp(_)) => {
                panic!("{}", RenderError::UnreachableSyntaxSugar(node.clone()))
            }
        }
    }
}

impl typescript::Pretty for ImportClause {
    fn to_ts(&self) -> D<'_, ()> {
        match self {
            ImportClause::Named(specifiers) => {
                let sep = D::text(",").append(D::line());

                let specifiers =
                    D::intersperse(specifiers.iter().map(typescript::Pretty::to_ts), sep);

                D::text("{")
                    .append(
                        D::nil()
                            .append(D::line())
                            .append(specifiers)
                            .append(D::line())
                            .nest(4),
                    )
                    .append(D::text("}"))
                    .group()
            }
            ImportClause::Namespace { alias } => D::text("*")
                .append(D::space())
                .append("as")
                .append(D::space())
                .append(alias.to_ts()),
        }
    }
}

impl typescript::Pretty for ImportSpecifier {
    fn to_ts(&self) -> D<'_, ()> {
        let alias_doc = match &self.alias {
            Some(alias) => D::space()
                .append("as")
                .append(D::space())
                .append(alias.to_ts()),

            None => D::nil(),
        };

        self.module_export_name.to_ts().append(alias_doc)
    }
}

impl typescript::Pretty for BuiltinKeyword {
    fn to_ts(&self) -> D<'_, ()> {
        match self {
            BuiltinKeyword::Keyof => D::text("keyof"),
        }
    }
}

impl Display for PrimitiveType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrimitiveType::Boolean => write!(f, "boolean"),
            PrimitiveType::Number => write!(f, "number"),
            PrimitiveType::String => write!(f, "string"),
            PrimitiveType::Object => write!(f, "object"),
            PrimitiveType::Symbol => write!(f, "symbol"),
            PrimitiveType::BigInt => write!(f, "bigint"),
            PrimitiveType::Void => write!(f, "void"),
            PrimitiveType::Undefined => write!(f, "undefined"),
            PrimitiveType::Null => write!(f, "null"),
        }
    }
}

impl typescript::Pretty for PropertyName {
    fn to_ts(&self) -> D<'_, ()> {
        match self {
            PropertyName::Index(index) => surround(index.to_ts(), "[", "]").group(),
            PropertyName::LiteralPropertyName(key) => D::text(key.clone()),
            PropertyName::ComputedPropertyName(expr) => {
                let mut inner = D::nil();

                if expr.is_well_known_symbol() {
                    if let Ast::Access(Access { rhs, .. }) = expr {
                        if let Ast::Ident(Ident { name, .. }) = &**rhs {
                            inner = inner.append("Symbol.").append(name.clone());
                        }
                    }
                } else {
                    inner = inner.append(expr.to_ts());
                }

                return surround(inner, "[", "]").group();
            }
        }
    }
}

impl typescript::Pretty for PropertyKeyIndex {
    fn to_ts(&self) -> D<'_, ()> {
        let PropertyKeyIndex {
            key,
            iterable,
            remapped_as,
            ..
        } = self;

        let remapped_as = match remapped_as {
            Some(remapped_as) => D::space()
                .append("as")
                .append(D::space())
                .append(remapped_as.to_ts()),
            None => D::nil(),
        };

        D::nil()
            .append(key)
            .append(D::space())
            .append("in")
            .append(D::space())
            .append(iterable.to_ts())
            .append(remapped_as)
    }
}

impl typescript::Pretty for ObjectProperty {
    fn to_ts(&self) -> D<'_, ()> {
        let readonly = if self.readonly {
            D::text("readonly").append(D::space())
        } else {
            D::nil()
        };

        let optional = if self.optional {
            D::text("?")
        } else {
            D::nil()
        };

        let doc = D::nil();

        doc.append(readonly)
            .append(self.key.to_ts())
            .append(optional)
            .append(D::text(":"))
            .append(D::space())
            .append(self.value.to_ts())
    }
}

impl typescript::Pretty for Ident {
    fn to_ts(&self) -> D<'_, ()> {
        D::text(self.name.clone())
    }
}

impl typescript::Pretty for TypeParameter {
    fn to_ts(&self) -> D<'_, ()> {
        let rest = if self.rest { D::text("...") } else { D::nil() };

        let constraint = match &self.constraint {
            Some(constraint) => D::space()
                .append("extends")
                .append(D::space())
                .append(constraint.to_ts()),
            None => D::nil(),
        };

        let default_value = match &self.default {
            Some(value) => D::space()
                .append("=")
                .append(D::space())
                .append(value.to_ts()),
            None => D::nil(),
        };

        D::nil()
            .append(rest)
            .append(self.name.clone())
            .append(constraint)
            .append(default_value)
    }
}

use thiserror::Error;
#[derive(Error, Debug)]
enum RenderError {
    #[error("AST should be desugared before this point {0:#?}")]
    UnreachableSyntaxSugar(Ast),
}
