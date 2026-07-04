use std::{collections::HashMap, rc::Rc, result};

use ::pretty::RcDoc as D;
use cond_expr::CondExpr;
use derivative::Derivative;
use if_expr::IfExpr;
use let_expr::LetExpr;
use match_expr::MatchExpr;
use newtype_macros_lib::ast_node;
use serde_derive::Serialize;

use crate::runtime::builtin;

pub(crate) mod errors;

pub type Bindings = HashMap<String, Ast>;

#[ast_node]
pub struct Path {
    pub segments: Vec<Ast>,
}

impl Path {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            span: self.span,
            segments: self.segments.iter().map(f).collect(),
        }
    }

    fn simplify(&self) -> Ast {
        let mut acc = vec![];

        for seg in self.segments.iter() {
            if let Ast::Path(path) = seg {
                acc.extend(path.segments.clone());
            } else {
                acc.push(seg.clone());
            }
        }

        Ast::Path(Path {
            span: self.span,
            segments: acc,
        })
    }
}

#[ast_node]
pub struct ExtendsExpr {
    pub lhs: Rc<Ast>,
    pub rhs: Rc<Ast>,
    pub then_branch: Rc<Ast>,
    pub else_branch: Rc<Ast>,
}

impl ExtendsExpr {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        let mut expr = self.clone();

        expr.lhs = Rc::new(f(&self.lhs));
        expr.rhs = Rc::new(f(&self.rhs));
        expr.then_branch = Rc::new(f(&self.then_branch));
        expr.else_branch = Rc::new(f(&self.else_branch));
        expr
    }

    pub fn new(
        span: Span,
        lhs: Rc<Ast>,
        rhs: Rc<Ast>,
        then_branch: Rc<Ast>,
        else_branch: Rc<Ast>,
    ) -> Self {
        if !lhs.is_typescript_feature() {
            dbg!(&lhs);
            unreachable!("value must be desugared before this point");
        }
        if !rhs.is_typescript_feature() {
            dbg!(&rhs);
            unreachable!("value must be desugared before this point");
        }
        if !then_branch.is_typescript_feature() {
            dbg!(&then_branch);
            unreachable!("value must be desugared before this point");
        }
        if !else_branch.is_typescript_feature() {
            dbg!(&else_branch);
            unreachable!("value must be desugared before this point");
        }

        Self {
            span,
            lhs,
            rhs,
            then_branch,
            else_branch,
        }
    }
}

#[ast_node]
pub struct Tuple {
    pub items: Vec<Ast>,
}

impl Tuple {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            items: self.items.iter().map(f).collect(),
            span: self.span,
        }
    }
}

#[ast_node]
pub struct ApplyGeneric {
    pub receiver: Rc<Ast>,
    pub args: Vec<Ast>,
}

pub type Result = result::Result<Ast, errors::Error>;

impl ApplyGeneric {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        let mut expr = self.clone();
        expr.receiver = f(&self.receiver).into();
        expr.args = self.args.iter().map(f).collect();
        expr
    }
}

#[ast_node]
pub struct MappedType {
    pub index: String,
    pub iterable: Rc<Ast>,
    pub remapped_as: Option<Rc<Ast>>,
    pub readonly_mod: Option<MappingModifier>,
    pub optional_mod: Option<MappingModifier>,
    pub body: Rc<Ast>,
}

impl MappedType {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        let mut expr = self.clone();
        expr.iterable = Rc::new(f(&self.iterable));
        expr.remapped_as = self.remapped_as.as_ref().map(|x| Rc::new(f(x)));
        expr.body = f(&self.body).into();
        expr
    }
}

#[ast_node]
pub struct TypeLiteral {
    pub properties: Vec<ObjectProperty>,
}

impl TypeLiteral {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            properties: self
                .properties
                .clone()
                .into_iter()
                .map(|prop| prop.map(|ty| f(ty)))
                .collect(),
            span: self.span,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, ObjectProperty> {
        self.properties.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, ObjectProperty> {
        self.properties.iter_mut()
    }
}

#[ast_node]
pub struct Interface {
    pub export: bool,
    pub name: String,
    pub extends: Option<Rc<Ast>>,
    pub params: Vec<TypeParameter>,
    pub definition: Vec<ObjectProperty>,
}

#[ast_node]
pub struct UnitTest {
    pub name: String,
    pub body: Vec<Ast>,
}

/// A single `assert <claim>` statement inside a `unittest` body. The `claim` is
/// an `extends_expr` (e.g. `A <: B`, `A == B`, `not (A <: B)`), so it carries
/// the same relational operators used by `if`/`cond`/`where`.
#[ast_node]
pub struct Assert {
    pub claim: Rc<Ast>,
}

#[ast_node]
pub struct MacroCall {
    pub name: String,
    pub args: Vec<Ast>,
}

impl MacroCall {
    fn eval(&self) -> Ast {
        let name = self.name.strip_suffix("!").unwrap();

        match name {
            "dbg!" => match self.args.as_slice() {
                [node] => builtin::dbg(node.to_owned()),
                _ => panic!("dbg! expects exactly one argument"),
            },
            "assert_equal!" => match self.args.as_slice() {
                [lhs, rhs] => builtin::assert_equal(lhs.to_owned(), rhs.to_owned()),
                _ => panic!("assert_equal! expects exactly two arguments"),
            },
            "unquote!" => match self.args.as_slice() {
                [node] => builtin::unquote(node.to_owned()),
                _ => panic!("unquote! expects exactly one argument"),
            },
            id => unimplemented!("macro {} not implemented", id),
        }
    }
}

#[ast_node]
pub struct FunctionType {
    pub params: Vec<Parameter>,
    pub return_type: Rc<Ast>,
}

#[ast_node]
pub struct Parameter {
    pub ellipsis: bool,
    pub name: String,
    pub kind: Ast,
}

impl FunctionType {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            params: self
                .params
                .iter()
                .map(|p| Parameter {
                    ellipsis: p.ellipsis,
                    name: p.name.clone(),
                    kind: f(&p.kind),
                    span: p.span,
                })
                .collect(),
            return_type: f(&self.return_type).into(),
            span: self.span,
        }
    }
}

#[ast_node]
pub struct Access {
    pub lhs: Rc<Ast>,
    pub rhs: Rc<Ast>,
    pub is_dot: bool,
}

impl Access {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            lhs: f(&self.lhs).into(),
            rhs: f(&self.rhs).into(),
            is_dot: self.is_dot,
            span: self.span,
        }
    }
}

#[ast_node]
pub struct IntersectionType {
    pub types: Vec<Ast>,
}

impl IntersectionType {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            types: self.types.iter().map(f).collect(),
            span: self.span,
        }
    }
}

#[ast_node]
pub struct Builtin {
    pub name: BuiltinKeyword,
    pub argument: Rc<Ast>,
}

impl Builtin {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            argument: f(&self.argument).into(),
            ..self.clone()
        }
    }
}

#[ast_node]
pub struct UnionType {
    pub types: Vec<Ast>,
}

impl UnionType {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            types: self.types.iter().map(f).collect(),
            span: self.span,
        }
    }
}

#[ast_node]
pub struct ExtendsInfixOp {
    pub lhs: Rc<Ast>,
    pub op: InfixOp,
    pub rhs: Rc<Ast>,
}

impl ExtendsInfixOp {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            lhs: f(&self.lhs).into(),
            rhs: f(&self.rhs).into(),
            ..self.clone()
        }
    }
}

#[ast_node]
pub struct ExtendsPrefixOp {
    pub op: PrefixOp,
    pub value: Rc<Ast>,
}

impl ExtendsPrefixOp {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            value: f(&self.value).into(),
            ..self.clone()
        }
    }
}

#[ast_node]
pub struct ImportStatement {
    pub import_clause: ImportClause,
    pub module: String,
}

#[ast_node]
pub struct TypeAlias {
    pub export: bool,
    pub name: Ident,
    pub params: Vec<TypeParameter>,
    pub body: Rc<Ast>,
}

impl TypeAlias {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            body: f(&self.body).into(),
            params: self.params.iter().map(|ty| ty.map(&f)).collect(),
            ..self.clone()
        }
    }
}

#[ast_node]
pub struct Program {
    pub statements: Vec<Ast>,
}

impl Program {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            statements: self.statements.iter().map(f).collect(),
            span: self.span,
        }
    }
}

#[ast_node]
pub enum Ast {
    #[serde(rename(serialize = "."))]
    Access(Access),
    #[serde(rename(serialize = "macro"))]
    MacroCall(MacroCall),
    #[serde(rename(serialize = "apply"))]
    ApplyGeneric(ApplyGeneric),
    Array(Rc<Ast>),
    #[serde(rename(serialize = "readonly"))]
    Readonly(Rc<Ast>),
    #[serde(rename(serialize = "|"))]
    UnionType(UnionType),
    #[serde(rename(serialize = "&"))]
    IntersectionType(IntersectionType),
    Assert(Assert),
    Builtin(Builtin),
    CondExpr(CondExpr),
    ExtendsInfixOp(ExtendsInfixOp),
    ExtendsExpr(ExtendsExpr),
    Infer(Rc<Ast>),
    ExtendsPrefixOp(ExtendsPrefixOp),
    Ident(Ident),
    #[serde(rename = "if")]
    IfExpr(IfExpr),
    #[serde(rename = "import")]
    ImportStatement(ImportStatement),
    #[serde(rename = "let")]
    LetExpr(LetExpr),
    MappedType(MappedType),
    #[serde(rename = "match")]
    MatchExpr(MatchExpr),
    #[serde(rename = "::")]
    Path(Path),
    #[serde(rename = "number")]
    TypeNumber(TypeNumber),
    TypeLiteral(TypeLiteral),
    #[ast_node(span)]
    Primitive(PrimitiveType),
    Program(Program),
    Statement(Rc<Ast>),
    UnitTest(UnitTest),
    TypeString(TypeString),
    TemplateString(TemplateString),
    Tuple(Tuple),
    #[serde(rename(serialize = "type"))]
    TypeAlias(TypeAlias),
    #[serde(rename(serialize = "never"))]
    #[ast_node(span)]
    NeverKeyword(),
    #[serde(rename(serialize = "unknown"))]
    #[ast_node(span)]
    TrueKeyword(),
    #[ast_node(span)]
    FalseKeyword(),
    Interface(Interface),
    FunctionType(FunctionType),
    #[ast_node(span)]
    UnknownKeyword(),
    #[serde(rename = "any")]
    #[ast_node(span)]
    AnyKeyword(),
    #[ast_node(span)]
    NoOp(),
    /// A reference to a declared `unique symbol` used as a type; renders as
    /// `typeof <name>`. Bare `Ident` references to a declared unique symbol are
    /// rewritten into this during `simplify` (computed keys are left as `Ident`,
    /// so they still render as `[<name>]`).
    UniqueSymbol(UniqueSymbol),
    /// The top-level `unique symbol <name>` declaration statement; renders as
    /// `declare const <name>: unique symbol`.
    UniqueSymbolDecl(UniqueSymbol),
}

impl Ast {
    pub fn to_sexp(&self) -> serde_lexpr::Result<serde_lexpr::Value> {
        serde_lexpr::to_value(self)
    }

    /// Operators that the `not` prefix operator can be applied to.
    pub fn is_compatible_with_not_prefix_op(&self) -> bool {
        match self {
            Ast::ExtendsPrefixOp(ExtendsPrefixOp { op, .. }) if op.is_not() => true,
            Ast::ExtendsInfixOp(ExtendsInfixOp { .. }) => true,
            _ => false,
        }
    }

    pub fn is_top_type(&self) -> bool {
        matches!(self, Ast::AnyKeyword(_) | Ast::UnknownKeyword(_))
    }

    pub fn is_empty_object(&self) -> bool {
        matches!(self, Ast::TypeLiteral(TypeLiteral { properties, .. }) if properties.is_empty())
    }

    pub fn is_nullish(&self) -> bool {
        matches!(
            self,
            Ast::Primitive(
                PrimitiveType::Undefined | PrimitiveType::Null | PrimitiveType::Void,
                _
            )
        )
    }

    fn is_non_nullish(&self) -> bool {
        !self.is_nullish()
    }

    fn is_non_primitive(&self) -> bool {
        !self.is_primitive()
    }

    fn is_object_interface(&self) -> bool {
        self.has_identifier("Object")
    }

    /// The global `Function` interface — every function type is assignable to it.
    fn is_function_interface(&self) -> bool {
        self.has_identifier("Function")
    }

    fn has_identifier(&self, test: &str) -> bool {
        matches!(self, Ast::Ident(Ident{name, ..}) if name == test)
    }

    fn is_primitive(&self) -> bool {
        matches!(self, Ast::Primitive(_, _))
    }

    pub fn is_bottom_type(&self) -> bool {
        matches!(self, Ast::NeverKeyword(_))
    }

    pub fn get_primitive_type(&self) -> Option<PrimitiveType> {
        type P = PrimitiveType;

        let value = match self {
            Ast::TypeString(_) => P::String,

            Ast::TypeNumber(_) => P::Number,

            Ast::TrueKeyword(_) => P::Boolean,

            Ast::FalseKeyword(_) => P::Boolean,

            Ast::TypeLiteral(_) => P::Object,

            // A `unique symbol` is a subtype of `symbol`, so it carries the
            // `symbol` primitive for assignability (`X <: symbol`, `X <: Symbol`,
            // `X <: symbol | …`). Distinctness (`X <: Y` false, `symbol <: X`
            // false) falls out of the structural arms, since a `UniqueSymbol`
            // target matches no primitive/wrapper arm.
            Ast::UniqueSymbol(_) => P::Symbol,

            Ast::Ident(Ident { name, .. }) => match name.as_str() {
                "String" => P::String,
                "Number" => P::Number,
                "Boolean" => P::Boolean,
                "Object" => P::Object,
                "Symbol" => P::Symbol,
                "BigInt" => P::BigInt,
                _ => return None,
            },
            _ => return None,
        };

        Some(value)
    }

    pub fn is_object_wrapper(&self) -> bool {
        matches!(self, Ast::Ident(Ident{name, ..}) if matches!(name.as_str(),
                "Boolean" | "Number" | "String" | "Object" | "Symbol" | "BigInt"
        ))
    }

    pub fn is_object_wrapper_for(&self, object_wrapper_name: &str) -> bool {
        matches!(self, Ast::Ident(Ident{name, ..}) if name.as_str() == object_wrapper_name)
    }

    pub fn is_string_object_wrapper(&self) -> bool {
        self.is_object_wrapper_for("String")
    }

    pub fn is_number_object_wrapper(&self) -> bool {
        self.is_object_wrapper_for("Number")
    }

    pub fn is_boolean_object_wrapper(&self) -> bool {
        self.is_object_wrapper_for("Boolean")
    }

    pub fn is_object_object_wrapper(&self) -> bool {
        self.is_object_wrapper_for("Object")
    }

    pub fn is_symbol_object_wrapper(&self) -> bool {
        self.is_object_wrapper_for("Symbol")
    }

    pub fn is_bigint_object_wrapper(&self) -> bool {
        self.is_object_wrapper_for("BigInt")
    }

    fn is_well_known_symbol(&self) -> bool {
        if let Ast::Access(Access {
            lhs,
            rhs,
            is_dot: true,
            ..
        }) = self
        {
            if let (Ast::Ident(left), Ast::Ident(right)) = (&**lhs, &**rhs) {
                if left.name == "Symbol"
                    && matches!(
                        right.name.as_str(),
                        "hasInstance"
                            | "isConcatSpreadable"
                            | "match"
                            | "replace"
                            | "search"
                            | "species"
                            | "split"
                            | "toPrimitive"
                            | "toStringTag"
                            | "unscopables"
                            | "iterator"
                            | "asyncIterator"
                            | "matchAll"
                            | "dispose"
                            | "asyncDispose"
                            | "metadata"
                    )
                {
                    return true;
                }
            }
        }
        return false;
    }

    pub fn get_object_wrapper(&self) -> Option<Ast> {
        let name = match self {
            Ast::Primitive(p, _) => {
                type P = PrimitiveType;

                match p {
                    P::Boolean => "Boolean",
                    P::Number => "Number",
                    P::String => "String",
                    P::Object => "Object",
                    P::Symbol => "Symbol",
                    P::BigInt => "BigInt",
                    P::Void | P::Undefined | P::Null => return None,
                }
            }
            ast if ast.is_object_interface() => return None,
            _ => "Object",
        };
        Some(Ast::Ident(Ident {
            name: name.to_string(),
            span: self.as_span(),
        }))
    }

    /// Anything that returns true is a feature that has a direct equivalent in TypeScript.
    /// Anything that's false is a feature that needs to be desugared.
    pub fn is_typescript_feature(&self) -> bool {
        matches!(
            self,
            Ast::IntersectionType { .. }
                | Ast::ExtendsExpr(_)
                | Ast::Ident(_)
                | Ast::Infer { .. }
                | Ast::ImportStatement { .. }
                | Ast::MappedType(_)
                | Ast::Path(_)
                | Ast::NeverKeyword(_)
                | Ast::TypeNumber(_)
                | Ast::TypeLiteral(_)
                | Ast::Primitive(..)
                | Ast::Program(_)
                | Ast::Statement(_)
                | Ast::TypeString(_)
                | Ast::TemplateString(_)
                | Ast::Tuple(_)
                | Ast::UnknownKeyword(_)
                | Ast::Interface(_)
                | Ast::TrueKeyword(_)
                | Ast::FalseKeyword(_)
                | Ast::UnionType { .. }
                | Ast::Access { .. }
                | Ast::AnyKeyword(_)
                | Ast::ApplyGeneric(_)
                | Ast::Array(_)
                | Ast::Readonly(_)
                | Ast::FunctionType(_)
                | Ast::Builtin { .. }
                | Ast::UniqueSymbol(_)
                | Ast::UniqueSymbolDecl(_)
        )
    }

    /// Returns `true` if the node is [`ExtendsPrefixOp`].
    ///
    /// [`ExtendsPrefixOp`]: Ast::ExtendsPrefixOp
    #[must_use]
    pub fn is_extends_prefix_op(&self) -> bool {
        matches!(self, Self::ExtendsPrefixOp { .. })
    }

    /// Returns `true` if the ast is [`ExtendsInfixOp`].
    ///
    /// [`ExtendsInfixOp`]: Ast::ExtendsInfixOp
    #[must_use]
    pub fn is_extends_infix_op(&self) -> bool {
        matches!(self, Self::ExtendsInfixOp { .. })
    }

    /// Returns `true` if the ast is [`InfixOp`].
    ///
    /// [`InfixOp`]: Ast::InfixOp
    #[must_use]
    pub fn is_set_op(&self) -> bool {
        matches!(self, Self::UnionType { .. } | Self::IntersectionType { .. })
    }

    #[must_use]
    pub fn is_union(&self) -> bool {
        matches!(self, Self::UnionType { .. })
    }

    #[must_use]
    pub fn is_intersection(&self) -> bool {
        matches!(self, Self::IntersectionType { .. })
    }

    #[must_use]
    pub fn is_ident(&self) -> bool {
        matches!(self, Self::Ident(_))
    }

    #[must_use]
    pub fn is_ident_with_name(&self, name: &String) -> bool {
        matches!(self, Self::Ident(Ident { name: n, .. }) if n == name)
    }

    /// Compile-time-only statements that contribute no TypeScript output, so
    /// they must not get a trailing `;` or surrounding blank lines when rendered
    /// (e.g. a `unittest` block).
    #[must_use]
    pub fn is_zero_output(&self) -> bool {
        match self {
            Ast::Statement(inner) => inner.is_zero_output(),
            Ast::UnitTest(_) | Ast::NoOp(_) => true,
            _ => false,
        }
    }

    pub fn as_ident(&self) -> Option<&Ident> {
        if let Self::Ident(v) = self {
            Some(v)
        } else {
            None
        }
    }

    pub fn as_span(&self) -> Span {
        match self {
            Ast::Access(x) => x.span,
            Ast::AnyKeyword(x) => *x,
            Ast::Assert(x) => x.span,
            Ast::ApplyGeneric(x) => x.span,
            Ast::Array(ast) => ast.as_span(),
            Ast::Readonly(ast) => ast.as_span(),
            Ast::Builtin(x) => x.span,
            Ast::CondExpr(x) => x.span,
            Ast::ExtendsExpr(x) => x.span,
            Ast::ExtendsInfixOp(x) => x.span,
            Ast::ExtendsPrefixOp(x) => x.span,
            Ast::FalseKeyword(x) => *x,
            Ast::FunctionType(x) => x.span,
            Ast::Ident(x) => x.span,
            Ast::IfExpr(x) => x.span,
            Ast::ImportStatement(x) => x.span,
            Ast::Infer(ast) => ast.as_span(),
            Ast::Interface(x) => x.span,
            Ast::IntersectionType(x) => x.span,
            Ast::LetExpr(x) => x.span,
            Ast::MacroCall(x) => x.span,
            Ast::MappedType(x) => x.span,
            Ast::MatchExpr(x) => x.span,
            Ast::NeverKeyword(x) => *x,
            Ast::NoOp(x) => *x,
            Ast::TypeNumber(x) => x.span,
            Ast::Path(x) => x.span,
            Ast::Primitive(_, x) => *x,
            Ast::Program(x) => x.span,
            Ast::Statement(ast) => ast.as_span(),
            Ast::TypeString(x) => x.span,
            Ast::TemplateString(x) => x.span,
            Ast::TrueKeyword(x) => *x,
            Ast::Tuple(x) => x.span,
            Ast::TypeAlias(x) => x.span,
            Ast::TypeLiteral(x) => x.span,
            Ast::UnionType(x) => x.span,
            Ast::UnitTest(x) => x.span,
            Ast::UnknownKeyword(x) => *x,
            Ast::UniqueSymbol(x) => x.span,
            Ast::UniqueSymbolDecl(x) => x.span,
        }
    }
}

#[ast_node(transparent)]
pub struct TypeNumber {
    pub ty: String,
}

#[ast_node(transparent)]
pub struct TypeString {
    pub ty: String,
}

#[ast_node(transparent)]
pub struct TemplateString {
    pub ty: String,
}

#[derive(Debug, Clone, Copy, Eq, Serialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub fn start(&self) -> usize {
        self.start
    }

    pub fn end(&self) -> usize {
        self.end
    }

    pub fn merge(&self, other: &Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }
}

impl PartialEq for Span {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl From<Rc<Ast>> for Ast {
    fn from(value: Rc<Ast>) -> Self {
        (*value).clone()
    }
}

impl From<&Rc<Ast>> for Ast {
    fn from(value: &Rc<Ast>) -> Self {
        (**value).clone()
    }
}

impl From<if_expr::IfExpr> for Ast {
    fn from(v: if_expr::IfExpr) -> Self {
        Self::IfExpr(v)
    }
}

impl From<match_expr::MatchExpr> for Ast {
    fn from(v: match_expr::MatchExpr) -> Self {
        Self::MatchExpr(v)
    }
}

impl From<cond_expr::CondExpr> for Ast {
    fn from(v: cond_expr::CondExpr) -> Self {
        Self::CondExpr(v)
    }
}

impl From<Rc<Ast>> for Box<Ast> {
    fn from(value: Rc<Ast>) -> Self {
        Box::new((*value).clone())
    }
}

impl From<&Rc<Ast>> for Box<Ast> {
    fn from(value: &Rc<Ast>) -> Self {
        Box::new((**value).clone())
    }
}

impl From<ExtendsExpr> for Ast {
    fn from(v: ExtendsExpr) -> Self {
        Ast::ExtendsExpr(v)
    }
}

#[derive(Derivative, Clone, Eq, Serialize)]
#[derivative(PartialEq)]
#[derivative(Debug)]
#[serde(rename_all = "snake_case")]
pub enum ImportClause {
    Named(Vec<ImportSpecifier>),
    Namespace { alias: Ident },
}

#[ast_node]
pub struct ImportSpecifier {
    pub module_export_name: Ident,
    pub alias: Option<Ident>,
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinKeyword {
    Keyof,
}

#[derive(Derivative, Clone, Eq, Serialize)]
#[derivative(PartialEq)]
#[derivative(Debug)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveType {
    Boolean,
    Number,
    String,
    Object,
    Symbol,
    BigInt,
    Void,
    Undefined,
    Null,
}

#[derive(Derivative, Clone, Eq, Serialize)]
#[derivative(PartialEq)]
#[derivative(Debug)]
#[serde(rename_all = "snake_case")]
pub enum PrefixOp {
    Infer,
    Not,
}

impl PrefixOp {
    /// Returns `true` if the prefix op is [`Not`].
    ///
    /// [`Not`]: PrefixOp::Not
    #[must_use]
    pub fn is_not(&self) -> bool {
        matches!(self, Self::Not)
    }
}

#[derive(Derivative, Clone, Eq, Serialize)]
#[derivative(PartialEq)]
#[derivative(Debug)]
#[serde(rename_all = "snake_case")]
pub enum InfixOp {
    Extends,
    NotExtends,
    Equals,
    NotEquals,
    StrictEquals,
    StrictNotEquals,
    And,
    Or,
}

#[derive(Derivative, Clone, Eq, Serialize)]
#[derivative(PartialEq)]
#[derivative(Debug)]
#[serde(rename_all = "snake_case")]
pub enum MappingModifier {
    Add,
    Remove,
}

#[derive(Derivative, Clone, Eq, Serialize)]
#[derivative(PartialEq)]
#[derivative(Debug)]
#[serde(rename_all = "snake_case")]
pub enum PropertyName {
    Index(PropertyKeyIndex),
    LiteralPropertyName(String),
    ComputedPropertyName(Ast),
}

impl PropertyName {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        match self {
            PropertyName::Index(index) => PropertyName::Index(index.map(f)),
            PropertyName::ComputedPropertyName(id) => {
                PropertyName::ComputedPropertyName(id.clone())
            }
            _ => self.clone(),
        }
    }
}

#[ast_node]
pub struct PropertyKeyIndex {
    pub key: String,
    pub iterable: Ast,
    pub remapped_as: Option<Ast>,
}

impl PropertyKeyIndex {
    fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            iterable: f(&self.iterable),
            remapped_as: self.remapped_as.as_ref().map(f),
            ..self.clone()
        }
    }
}

#[ast_node]
pub struct ObjectProperty {
    pub readonly: bool,
    pub optional: bool,
    pub key: PropertyName,
    pub value: Ast,
}

impl ObjectProperty {
    fn map<F>(self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            value: f(&self.value),
            key: self.key.map(f),
            ..self
        }
    }
}

#[ast_node(transparent)]
pub struct Ident {
    pub name: String,
}

/// The name of a `unique symbol` declaration/reference. Its identity is the
/// name (the `span` is ignored in equality, like every other AST node), so two
/// references to the same declared unique symbol compare equal.
#[ast_node]
pub struct UniqueSymbol {
    pub name: String,
}

impl Ident {
    pub(crate) fn pretty(&self) -> D<'_, ()> {
        D::text(&self.name)
    }
}

#[ast_node]
pub struct TypeParameter {
    pub name: String,
    pub constraint: Option<Ast>,
    pub default: Option<Ast>,
    pub rest: bool,
}

impl TypeParameter {
    pub fn map<F>(&self, f: F) -> Self
    where
        F: Fn(&Ast) -> Ast,
    {
        Self {
            constraint: self.constraint.as_ref().map(&f),
            default: self.default.as_ref().map(&f),
            ..self.clone()
        }
    }

    pub fn new(
        name: String,
        constraint: Option<Ast>,
        default: Option<Ast>,
        rest: bool,
        span: Span,
    ) -> Self {
        Self {
            name,
            constraint,
            default,
            rest,
            span,
        }
    }
}

pub(crate) mod if_expr;

pub(crate) mod match_expr;

pub(crate) mod cond_expr;

pub(crate) mod let_expr;

mod pretty;

pub mod validate;

mod assignability;

pub(crate) mod type_env;

mod walk;
