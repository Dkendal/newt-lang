//! The newtype parser: source text → [`Ast`], via chumsky.
//!
//! Two stages:
//!
//! 1. [`lexer`] turns the source into a `(Token, SimpleSpan)` stream (see the
//!    module docs for the token design and its pest-fidelity subtleties).
//! 2. The token-level parsers below build the [`Ast`]. Operator precedence is
//!    encoded in two pratt tables — one for type expressions ([`Rule::expr`])
//!    and one for boolean/relational claims ([`Rule::extends_expr`]) — with the
//!    same relative binding order the old pest grammar used.
//!
//! Every grammar unit a test corpus or caller starts from is a [`Rule`]
//! variant; [`parse_source`] parses a full input as that unit (an inherent
//! `end()` guarantees the whole source is consumed).
//!
//! Recoverable syntax errors are returned as [`ParseError`]s. A handful of
//! *semantic* checks (`readonly` on a non-array, `not` on a non-relation, a
//! `where`/`defaults` clause naming an unknown type parameter, `::`/`|>` with
//! an invalid operand) panic with a rendered source excerpt, exactly as the
//! pest-based parser did; the CLI's panic hook presents these.

pub(crate) mod lexer;

use std::{collections::HashMap, rc::Rc, result::Result};

use crate::ast::*;
use crate::report;

use cond_expr::CondExpr;
use if_expr::IfExpr;
use let_expr::LetExpr;
use match_expr::MatchExpr;

use chumsky::input::{Input as _, ValueInput};
use chumsky::pratt::{infix, left, postfix, prefix};
use chumsky::prelude::*;

pub use lexer::{Kw, Token};

/// Token-stream span (byte offsets into the original source).
type TSpan = SimpleSpan<usize>;
type Extra<'t> = extra::Err<Rich<'t, Token, TSpan>>;
type P<'t, I, O> = Boxed<'t, 't, I, O, Extra<'t>>;

/// The source name used when rendering source excerpts for the parser's
/// semantic panics (the parser does not know the real filename).
const SOURCE_NAME: &str = "<input>";

/// A grammar start symbol. The corpus test macros reference these variants by
/// name (`newtype::parser::Rule, "expr"`), hence the lowercase names.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    program,
    expr,
    extends_expr,
    if_expr,
    map_expr,
    interface,
    object_literal,
    tuple,
    type_alias,
}

/// A single parse error: a source span and a human-readable message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub span: Span,
    pub message: String,
}

impl ParseError {
    /// Render this error against `source` as an underlined excerpt.
    pub fn render(&self, source_name: &str, source: &str) -> String {
        report::render_to_string(source_name, source, self.span, &self.message)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// All errors from one parse. Kept as a type alias so callers can iterate.
pub type ParserError = Vec<ParseError>;

/// Parse the entire `source` as the grammar unit `rule`.
pub fn parse_source(rule: Rule, source: &str) -> Result<Ast, ParserError> {
    let (tokens, lex_errs) = lexer::lex(source);
    if !lex_errs.is_empty() {
        return Err(lex_errs
            .into_iter()
            .map(|e| ParseError {
                span: Span::new(e.span().start, e.span().end),
                message: e.to_string(),
            })
            .collect());
    }
    let tokens = tokens.expect("lexing produced neither tokens nor errors");
    parse_tokens(rule, &tokens, source)
}

/// Parse a whole program to a single [`Ast::Program`].
pub fn parse_newtype_program(source: &str) -> Result<Ast, Box<ParserError>> {
    parse_source(Rule::program, source).map_err(Box::new)
}

/// Parse a whole program to its top-level items (the CLI entry point).
pub fn parse_newtype_program1(source: &str) -> Result<Vec<Ast>, Box<ParserError>> {
    parse_source(Rule::program, source)
        .map(|ast| vec![ast])
        .map_err(Box::new)
}

fn parse_tokens(rule: Rule, tokens: &[(Token, TSpan)], source: &str) -> Result<Ast, ParserError> {
    let eoi = TSpan::from(source.len()..source.len());
    let input = tokens.map(eoi, |(t, s)| (t, s));
    let parsers = build(source);

    let result = match rule {
        Rule::program => parsers.program.then_ignore(end()).parse(input),
        Rule::expr => parsers.expr.then_ignore(end()).parse(input),
        Rule::extends_expr => parsers.extends_expr.then_ignore(end()).parse(input),
        Rule::if_expr => parsers.if_expr.then_ignore(end()).parse(input),
        Rule::map_expr => parsers.map_expr.then_ignore(end()).parse(input),
        Rule::interface => parsers.interface.then_ignore(end()).parse(input),
        Rule::object_literal => parsers.object_literal.then_ignore(end()).parse(input),
        Rule::tuple => parsers.tuple.then_ignore(end()).parse(input),
        Rule::type_alias => parsers.type_alias.then_ignore(end()).parse(input),
    };

    result.into_result().map_err(|errs| {
        errs.into_iter()
            .map(|e| ParseError {
                span: Span::new(e.span().start, e.span().end),
                message: e.to_string(),
            })
            .collect()
    })
}

/// The named entry points, one per [`Rule`].
struct Parsers<'t, I>
where
    I: ValueInput<'t, Token = Token, Span = TSpan>,
{
    program: P<'t, I, Ast>,
    expr: P<'t, I, Ast>,
    extends_expr: P<'t, I, Ast>,
    if_expr: P<'t, I, Ast>,
    map_expr: P<'t, I, Ast>,
    interface: P<'t, I, Ast>,
    object_literal: P<'t, I, Ast>,
    tuple: P<'t, I, Ast>,
    type_alias: P<'t, I, Ast>,
}

fn sp(s: TSpan) -> Span {
    Span::new(s.start, s.end)
}

/// Strip the surrounding quotes off a raw string-literal token. Mirrors the old
/// parser's `trim_matches`, which strips *repeated* quote chars at both ends
/// (there are no escape sequences in the language).
fn trim_quotes(raw: &str) -> String {
    if raw.starts_with('"') {
        raw.trim_matches('"').to_string()
    } else {
        raw.trim_matches('\'').to_string()
    }
}

/// Desugar the pipe operator into a type application:
/// `A |> B` is `B(A)`, and `A |> F(X)` is `F(A, X)`.
fn pipe_to_application(lhs: Ast, rhs: Ast, op_span: Span, span: Span, src: &str) -> Ast {
    match rhs {
        Ast::Ident(_) => Ast::ApplyGeneric(ApplyGeneric {
            span,
            receiver: Rc::new(rhs),
            args: vec![lhs],
        }),
        Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) => {
            let mut args = args.clone();
            args.insert(0, lhs);
            Ast::ApplyGeneric(ApplyGeneric {
                span,
                receiver,
                args,
            })
        }
        _ => {
            let error = report::render_to_string(
                SOURCE_NAME,
                src,
                op_span,
                "the right-hand side of `|>` must be an identifier or a type application",
            );
            panic!("{error}");
        }
    }
}

/// Flatten `lhs :: rhs` into a single [`Ast::Path`]; both operands must be
/// identifiers or paths.
fn join_path(lhs: Ast, rhs: Ast, span: Span, src: &str) -> Ast {
    let mut segments = vec![];

    for side in [lhs, rhs] {
        match side {
            Ast::Path(Path {
                segments: inner, ..
            }) => segments.extend(inner),
            Ast::Ident(_) => segments.push(side),
            ast => {
                let error = report::render_to_string(
                    SOURCE_NAME,
                    src,
                    ast.as_span(),
                    "expected an identifier on this side of `::`",
                );
                panic!("{error}");
            }
        }
    }

    Ast::Path(Path { span, segments })
}

/// Merge the optional `(A, B)` parameter list, `defaults` clause and `where`
/// clause of a `type`/`interface` definition into ordered [`TypeParameter`]s.
///
/// # Panics
///
/// Panics (with a rendered source excerpt) when a `where`/`defaults` entry
/// names a type parameter that is not in the signature — same as the old
/// parser.
fn assemble_type_params(
    src: &str,
    params: Option<Vec<Ident>>,
    defaults: Option<Vec<(String, Ast, Span)>>,
    constraints: Option<Vec<(String, Ast, Span)>>,
) -> Vec<TypeParameter> {
    let mut order: Vec<String> = Vec::new();
    let mut by_name: HashMap<String, TypeParameter> = HashMap::new();

    for id in params.unwrap_or_default() {
        order.push(id.name.clone());
        by_name.insert(
            id.name.clone(),
            TypeParameter {
                span: id.span,
                name: id.name,
                constraint: None,
                default: None,
                rest: false,
            },
        );
    }

    let mut set = |name: String,
                   value: Ast,
                   span: Span,
                   field: fn(&mut TypeParameter) -> &mut Option<Ast>| {
        match by_name.get_mut(&name) {
            Some(param) => *field(param) = Some(value),
            None => {
                let error = report::render_to_string(
                    SOURCE_NAME,
                    src,
                    span,
                    &format!(r#"Type parameter "{name}", is missing from signature"#),
                );
                panic!("{error}");
            }
        }
    };

    for (name, body, span) in constraints.unwrap_or_default() {
        set(name, body, span, |p| &mut p.constraint);
    }

    for (name, value, span) in defaults.unwrap_or_default() {
        set(name, value, span, |p| &mut p.default);
    }

    order.iter().map(|name| by_name[name].clone()).collect()
}

/// Build every parser. `src` is captured so the semantic panics can render a
/// source excerpt.
fn build<'t, I>(src: &'t str) -> Parsers<'t, I>
where
    I: ValueInput<'t, Token = Token, Span = TSpan>,
{
    use Token as T;

    let kw = |k: Kw| just(T::Kw(k));
    // A contextual keyword: a plain identifier matched by text (`do`, `match`,
    // `cond`, `where`, `defaults`, `from`; `and`/`or` are handled in the
    // extends pratt table). These words stay usable as ordinary identifiers.
    let soft = |word: &'static str| {
        any()
            .filter(move |t: &Token| matches!(t, T::Ident(s) if s == word))
            .ignored()
            .labelled(word)
    };
    let comma = just(T::Comma);
    let lparen = just(T::LParen);
    let rparen = just(T::RParen);
    let lbracket = just(T::LBracket);
    let rbracket = just(T::RBracket);
    let lbrace = just(T::LBrace);
    let rbrace = just(T::RBrace);

    let ident =
        select! { T::Ident(name) = e => Ident { name, span: sp(e.span()) } }.labelled("identifier");
    let ident_name = select! { T::Ident(name) => name }.labelled("identifier");
    let string_raw = select! { T::Str(raw) => raw }.labelled("string");

    let mut expr = Recursive::declare();
    let mut extends_expr = Recursive::declare();

    // ---- Terms -------------------------------------------------------------

    let number = select! {
        T::Number(raw) = e => Ast::TypeNumber(TypeNumber { ty: raw, span: sp(e.span()) }),
    };

    let string_lit = select! {
        T::Str(raw) = e => Ast::TypeString(TypeString { ty: trim_quotes(&raw), span: sp(e.span()) }),
    };

    let template_string = select! {
        T::TemplateStr(raw) = e => Ast::TemplateString(TemplateString { ty: raw, span: sp(e.span()) }),
    };

    let keyword_term = select! {
        T::Kw(Kw::Any) = e => Ast::AnyKeyword(sp(e.span())),
        T::Kw(Kw::Unknown) = e => Ast::UnknownKeyword(sp(e.span())),
        T::Kw(Kw::Never) = e => Ast::NeverKeyword(sp(e.span())),
        T::Kw(Kw::True) = e => Ast::TrueKeyword(sp(e.span())),
        T::Kw(Kw::False) = e => Ast::FalseKeyword(sp(e.span())),
        T::Kw(Kw::String) = e => Ast::Primitive(PrimitiveType::String, sp(e.span())),
        T::Kw(Kw::Boolean) = e => Ast::Primitive(PrimitiveType::Boolean, sp(e.span())),
        T::Kw(Kw::Number) = e => Ast::Primitive(PrimitiveType::Number, sp(e.span())),
        T::Kw(Kw::Object) = e => Ast::Primitive(PrimitiveType::Object, sp(e.span())),
        T::Kw(Kw::Bigint) = e => Ast::Primitive(PrimitiveType::BigInt, sp(e.span())),
        T::Kw(Kw::Symbol) = e => Ast::Primitive(PrimitiveType::Symbol, sp(e.span())),
        T::Kw(Kw::Void) = e => Ast::Primitive(PrimitiveType::Void, sp(e.span())),
        T::Kw(Kw::Null) = e => Ast::Primitive(PrimitiveType::Null, sp(e.span())),
        T::Kw(Kw::Undefined) = e => Ast::Primitive(PrimitiveType::Undefined, sp(e.span())),
    };

    let ident_term = select! {
        T::Ident(name) = e => Ast::Ident(Ident { name, span: sp(e.span()) }),
    };

    // ---- Object literals ----------------------------------------------------

    // Object member names are deliberately permissive: any identifier-shaped
    // word, including every reserved word (`string`, `type`, `any`, ...).
    let property_key_name = select! {
        T::Ident(name) => name,
        T::Kw(k) => k.as_str().to_string(),
    }
    .labelled("property name");

    // `k in T` / `k in T as R` — a mapped-type index signature.
    let index_property_key = ident
        .then_ignore(kw(Kw::In))
        .then(expr.clone())
        .then(kw(Kw::As).ignore_then(expr.clone()).or_not())
        .map_with(|((index, iterable), remapped_as), e| PropertyKeyIndex {
            span: sp(e.span()),
            key: index.name,
            iterable,
            remapped_as,
        })
        .boxed();

    let property_key_inner = choice((
        property_key_name.map(ObjectPropertyKey::LiteralPropertyName),
        index_property_key
            .clone()
            .map(ObjectPropertyKey::Index)
            .or(ident.map(ObjectPropertyKey::Computed))
            .delimited_by(lbracket.clone(), rbracket.clone()),
    ));

    let object_property = kw(Kw::Readonly)
        .or_not()
        .then(property_key_inner)
        .then(just(T::Question).or_not())
        .then_ignore(just(T::Colon))
        .then(expr.clone())
        .map_with(
            |(((readonly, key), postfix_opt), value), e| ObjectProperty {
                span: sp(e.span()),
                readonly: readonly.is_some(),
                optional: postfix_opt.is_some(),
                key,
                value,
            },
        );

    let object_literal = object_property
        .separated_by(comma.clone())
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(lbrace.clone(), rbrace.clone())
        .map_with(|properties, e| TypeLiteral {
            properties,
            span: sp(e.span()),
        })
        .boxed();

    // ---- Tuples -------------------------------------------------------------

    // `[]` with no gap lexes as one token (also the array postfix); a bracketed
    // list takes no trailing comma.
    let tuple = choice((
        just(T::BracketPair).map_with(|_, e| {
            Ast::Tuple(Tuple {
                span: sp(e.span()),
                items: vec![],
            })
        }),
        expr.clone()
            .separated_by(comma.clone())
            .collect::<Vec<_>>()
            .delimited_by(lbracket.clone(), rbracket.clone())
            .map_with(|items, e| {
                Ast::Tuple(Tuple {
                    span: sp(e.span()),
                    items,
                })
            }),
    ))
    .boxed();

    // ---- Function types ------------------------------------------------------

    let named_parameter = just(T::Ellipsis)
        .or_not()
        .then(ident_name)
        .then_ignore(just(T::Colon))
        .then(expr.clone())
        .map_with(|((ellipsis, name), kind), e| Parameter {
            span: sp(e.span()),
            ellipsis: ellipsis.is_some(),
            name,
            kind,
        });
    let named_parameters = named_parameter
        .separated_by(comma.clone())
        .allow_trailing()
        .at_least(1)
        .collect::<Vec<_>>();

    // Unnamed parameters get synthesized positional names; a rest parameter is
    // named `rest`.
    let unnamed_parameter = just(T::Ellipsis)
        .or_not()
        .then(expr.clone())
        .map_with(|(ellipsis, kind), e| (ellipsis.is_some(), kind, sp(e.span())));
    let unnamed_parameters = unnamed_parameter
        .separated_by(comma.clone())
        .allow_trailing()
        .at_least(1)
        .collect::<Vec<_>>()
        .map(|params| {
            params
                .into_iter()
                .enumerate()
                .map(|(idx, (ellipsis, kind, span))| Parameter {
                    span,
                    ellipsis,
                    name: if ellipsis {
                        "rest".to_string()
                    } else {
                        format!("arg{idx}")
                    },
                    kind,
                })
                .collect::<Vec<_>>()
        });

    let parameters = named_parameters
        .or(unnamed_parameters)
        .or_not()
        .map(Option::unwrap_or_default)
        .delimited_by(lparen.clone(), rparen.clone());

    let function_type = parameters
        .then_ignore(just(T::FatArrow))
        .then(expr.clone())
        .map_with(|(params, return_type), e| {
            Ast::FunctionType(FunctionType {
                span: sp(e.span()),
                params,
                return_type: Rc::new(return_type),
            })
        })
        .boxed();

    // ---- Sugar expressions (desugared during `simplify`) ---------------------

    let if_expr = kw(Kw::If)
        .ignore_then(extends_expr.clone())
        .then_ignore(kw(Kw::Then))
        .then(expr.clone())
        .then(kw(Kw::Else).ignore_then(expr.clone()).or_not())
        .then_ignore(kw(Kw::End))
        .map_with(|((condition, then_branch), else_branch), e| {
            let span = sp(e.span());
            Ast::IfExpr(IfExpr {
                span,
                condition: Rc::new(condition),
                then_branch: Rc::new(then_branch),
                else_branch: Some(Rc::new(else_branch.unwrap_or(Ast::NeverKeyword(span)))),
            })
        })
        .boxed();

    let else_arm = kw(Kw::Else)
        .ignore_then(just(T::Arrow))
        .ignore_then(expr.clone());

    let match_arm = expr
        .clone()
        .then_ignore(just(T::Arrow))
        .then(expr.clone())
        .map_with(|(pattern, body), e| match_expr::Arm {
            span: sp(e.span()),
            pattern,
            body,
        });
    let match_expr_p = soft("match")
        .ignore_then(expr.clone())
        .then_ignore(soft("do"))
        .then(
            match_arm
                .separated_by(comma.clone())
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .then(comma.clone().ignore_then(else_arm.clone()).or_not())
        .then_ignore(comma.clone().or_not())
        .then_ignore(kw(Kw::End))
        .map_with(|((value, arms), else_arm), e| {
            let span = sp(e.span());
            Ast::MatchExpr(MatchExpr {
                span,
                value: Rc::new(value),
                arms,
                else_arm: Rc::new(else_arm.unwrap_or(Ast::NeverKeyword(span))),
            })
        })
        .boxed();

    let cond_arm = extends_expr
        .clone()
        .then_ignore(just(T::Arrow))
        .then(expr.clone())
        .map_with(|(condition, body), e| cond_expr::Arm {
            span: sp(e.span()),
            condition,
            body,
        });
    let cond_expr_p = soft("cond")
        .ignore_then(soft("do"))
        .ignore_then(
            cond_arm
                .separated_by(comma.clone())
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .then(comma.clone().ignore_then(else_arm.clone()).or_not())
        .then_ignore(comma.clone().or_not())
        .then_ignore(kw(Kw::End))
        .map_with(|(arms, else_arm), e| {
            let span = sp(e.span());
            Ast::CondExpr(CondExpr {
                span,
                arms,
                else_arm: Rc::new(else_arm.unwrap_or(Ast::NeverKeyword(span))),
            })
        })
        .boxed();

    let let_binding = ident_name.then_ignore(just(T::Eq)).then(expr.clone());
    let let_expr_p = kw(Kw::Let)
        .ignore_then(
            let_binding
                .separated_by(comma.clone())
                .allow_trailing()
                .at_least(1)
                .collect::<Vec<_>>(),
        )
        .then_ignore(kw(Kw::In))
        .then(expr.clone())
        .map_with(|(bindings, body), e| {
            Ast::LetExpr(LetExpr {
                span: sp(e.span()),
                bindings: bindings.into_iter().collect(),
                body: Rc::new(body),
            })
        })
        .boxed();

    let map_expr_p = kw(Kw::Map)
        .ignore_then(kw(Kw::Readonly).or_not())
        .then(just(T::Question).or_not())
        .then(index_property_key.clone())
        .then_ignore(soft("do"))
        .then(expr.clone())
        .then_ignore(kw(Kw::End))
        .map_with(|(((readonly, optional), index_key), body), e| {
            Ast::MappedType(MappedType {
                span: sp(e.span()),
                index: index_key.key,
                iterable: Rc::new(index_key.iterable),
                remapped_as: index_key.remapped_as.map(Rc::new),
                readonly_mod: readonly.map(|_| MappingModifier::Add),
                optional_mod: optional.map(|_| MappingModifier::Add),
                body: Rc::new(body),
            })
        })
        .boxed();

    // ---- Calls ----------------------------------------------------------------

    let argument_list = expr
        .clone()
        .separated_by(comma.clone())
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(lparen.clone(), rparen.clone())
        .boxed();

    // The macro name keeps its trailing `!` (`MacroCall::eval` strips it).
    let macro_call = select! { T::MacroIdent(name) => name }
        .then(argument_list.clone())
        .map_with(|(name, args), e| {
            Ast::MacroCall(MacroCall {
                span: sp(e.span()),
                name,
                args,
            })
        });

    // ---- The expression pratt table --------------------------------------------

    // Primary-expression alternatives, in the old grammar's order (`match`/
    // `cond` before a bare identifier; parenthesized expression last).
    let atom = choice((
        if_expr.clone(),
        map_expr_p.clone(),
        match_expr_p,
        cond_expr_p,
        let_expr_p,
        macro_call,
        function_type,
        number,
        keyword_term,
        template_string,
        string_lit,
        ident_term,
        tuple.clone(),
        object_literal.clone().map(Ast::TypeLiteral),
        expr.clone().delimited_by(lparen.clone(), rparen.clone()),
    ))
    .boxed();

    let indexed_access = expr
        .clone()
        .delimited_by(lbracket.clone(), rbracket.clone());

    // Binding order (loosest first): `|` < `&` < `|>` < keyof/readonly <
    // application < `[]` < `?` (infer) < `::`/`.` < `[expr]`. Union and
    // intersection build strictly binary nodes; n-ary flattening happens in
    // `simplify`.
    let expr_pratt = atom.pratt((
        infix(left(1), just(T::Union), |lhs: Ast, _, rhs: Ast, e| {
            Ast::UnionType(UnionType {
                types: vec![lhs, rhs],
                span: sp(e.span()),
            })
        }),
        infix(
            left(2),
            just(T::Intersection),
            |lhs: Ast, _, rhs: Ast, e| {
                Ast::IntersectionType(IntersectionType {
                    types: vec![lhs, rhs],
                    span: sp(e.span()),
                })
            },
        ),
        infix(
            left(3),
            just(T::PipeOp).map_with(|_, e| e.span()),
            move |lhs: Ast, op_span: TSpan, rhs: Ast, e| {
                pipe_to_application(lhs, rhs, sp(op_span), sp(e.span()), src)
            },
        ),
        prefix(4, kw(Kw::Keyof), |_, argument: Ast, e| {
            Ast::Builtin(Builtin {
                name: BuiltinKeyword::Keyof,
                argument: Rc::new(argument),
                span: sp(e.span()),
            })
        }),
        // `readonly` is only permitted on array and tuple literal types (the
        // same restriction TypeScript enforces).
        prefix(
            4,
            kw(Kw::Readonly),
            move |_, operand: Ast, _e| match operand {
                Ast::Array(_) | Ast::Tuple(_) => Ast::Readonly(Rc::new(operand)),
                other => {
                    let error = report::render_to_string(
                        SOURCE_NAME,
                        src,
                        other.as_span(),
                        "readonly type modifier is only permitted on array and tuple \
                     literal types",
                    );
                    panic!("{error}");
                }
            },
        ),
        postfix(
            5,
            argument_list.clone(),
            |receiver: Ast, args: Vec<Ast>, e| {
                Ast::ApplyGeneric(ApplyGeneric {
                    span: sp(e.span()),
                    receiver: Rc::new(receiver),
                    args,
                })
            },
        ),
        postfix(6, just(T::BracketPair), |lhs: Ast, _, _e| {
            Ast::Array(Rc::new(lhs))
        }),
        prefix(7, just(T::Question), |_, value: Ast, _e| {
            Ast::Infer(Rc::new(value))
        }),
        infix(left(8), just(T::Colon2), move |lhs: Ast, _, rhs: Ast, e| {
            join_path(lhs, rhs, sp(e.span()), src)
        }),
        infix(left(8), just(T::Dot), |lhs: Ast, _, rhs: Ast, e| {
            Ast::Access(Access {
                lhs: Rc::new(lhs),
                rhs: Rc::new(rhs),
                is_dot: true,
                span: sp(e.span()),
            })
        }),
        postfix(9, indexed_access, |lhs: Ast, rhs: Ast, e| {
            Ast::Access(Access {
                lhs: Rc::new(lhs),
                rhs: Rc::new(rhs),
                is_dot: false,
                span: sp(e.span()),
            })
        }),
    ));
    expr.define(expr_pratt.boxed());

    // ---- The extends (boolean/relational) pratt table ---------------------------

    // A claim's primary is a plain type expression; a parenthesized claim only
    // matches after the expression alternative fails on the relational operator
    // inside — same backtracking the pest grammar relied on.
    let extends_primary = expr
        .clone()
        .or(extends_expr
            .clone()
            .delimited_by(lparen.clone(), rparen.clone()))
        .boxed();

    let bool_op = select! {
        T::Ident(s) if s == "and" => InfixOp::And,
        T::Ident(s) if s == "or" => InfixOp::Or,
    };
    let relation_op = select! {
        T::Extends => InfixOp::Extends,
        T::NotExtends => InfixOp::NotExtends,
        T::EqEq => InfixOp::StrictEquals,
        T::NeqStrict => InfixOp::StrictNotEquals,
        T::Eq => InfixOp::Equals,
        T::Neq => InfixOp::NotEquals,
    };

    let fold_extends_infix =
        |lhs: Ast,
         op: InfixOp,
         rhs: Ast,
         e: &mut chumsky::input::MapExtra<'t, '_, I, Extra<'t>>| {
            Ast::ExtendsInfixOp(ExtendsInfixOp {
                lhs: Rc::new(lhs),
                op,
                rhs: Rc::new(rhs),
                span: sp(e.span()),
            })
        };

    // `and`/`or` share one (loosest) level; the relations share the next; `not`
    // binds tightest, so `not A <: B` is `(not A) <: B` — rejected below with a
    // parenthesization hint.
    let extends_pratt = extends_primary.pratt((
        infix(left(1), bool_op, fold_extends_infix),
        infix(left(2), relation_op, fold_extends_infix),
        prefix(
            3,
            kw(Kw::Not).map_with(|_, e| e.span()),
            move |op_span: TSpan, value: Ast, e| {
                if !value.is_extends_infix_op() {
                    let error_not = report::render_to_string(
                        SOURCE_NAME,
                        src,
                        sp(op_span),
                        "`not` may only be used with an extends expression",
                    );
                    let error_expr = report::render_to_string(
                        SOURCE_NAME,
                        src,
                        sp(e.span()),
                        "expected an extends expression",
                    );
                    panic!(
                        "{error_not}\n{error_expr}\nHint: You might have forgotten to wrap the expression in parentheses, `not` has higher precedence than other operators."
                    );
                }

                Ast::ExtendsPrefixOp(ExtendsPrefixOp {
                    op: PrefixOp::Not,
                    value: Rc::new(value),
                    span: sp(e.span()),
                })
            },
        ),
    ));
    extends_expr.define(extends_pratt.boxed());

    // ---- Statements ---------------------------------------------------------

    let export = kw(Kw::Export).or_not().map(|e| e.is_some());

    // `(A, B, C)` — no trailing comma.
    let type_parameters = ident
        .separated_by(comma.clone())
        .at_least(1)
        .collect::<Vec<_>>()
        .delimited_by(lparen.clone(), rparen.clone());

    let default_entry = ident_name
        .then_ignore(just(T::Eq))
        .then(expr.clone())
        .map_with(|(name, value), e| (name, value, sp(e.span())));
    let defaults_clause = soft("defaults").ignore_then(
        default_entry
            .separated_by(comma.clone())
            .allow_trailing()
            .at_least(1)
            .collect::<Vec<_>>(),
    );

    let constraint_entry = ident_name
        .then_ignore(just(T::Extends))
        .then(expr.clone())
        .map_with(|(name, body), e| (name, body, sp(e.span())));
    let where_clause = soft("where").ignore_then(
        constraint_entry
            .separated_by(comma.clone())
            .allow_trailing()
            .at_least(1)
            .collect::<Vec<_>>(),
    );

    let definition_options = type_parameters
        .or_not()
        .then(defaults_clause.or_not())
        .then(where_clause.or_not())
        .map(move |((params, defaults), constraints)| {
            assemble_type_params(src, params, defaults, constraints)
        })
        .boxed();

    let type_alias = export
        .clone()
        .then_ignore(kw(Kw::Type))
        .then(ident)
        .then(definition_options.clone())
        .then_ignore(kw(Kw::As))
        .then(expr.clone())
        .map_with(|(((export, name), params), body), e| {
            Ast::TypeAlias(TypeAlias {
                span: sp(e.span()),
                export,
                name,
                params,
                body: Rc::new(body),
            })
        })
        .boxed();

    let interface = export
        .clone()
        .then_ignore(kw(Kw::Interface))
        .then(ident_name)
        .then(definition_options.clone())
        .then(kw(Kw::Extends).ignore_then(expr.clone()).or_not())
        .then(object_literal.clone())
        .map_with(|((((export, name), params), extends), body), e| {
            Ast::Interface(Interface {
                span: sp(e.span()),
                export,
                name,
                params,
                extends: extends.map(Rc::new),
                definition: body.properties,
            })
        })
        .boxed();

    let unique_symbol_decl = kw(Kw::Unique)
        .ignore_then(kw(Kw::Symbol))
        .ignore_then(ident_name)
        .map_with(|name, e| {
            Ast::UniqueSymbolDecl(UniqueSymbol {
                name,
                span: sp(e.span()),
            })
        });

    let import_specifier =
        ident
            .then(kw(Kw::As).ignore_then(ident).or_not())
            .map_with(|(name, alias), e| ImportSpecifier {
                span: sp(e.span()),
                module_export_name: name,
                alias,
            });
    let named_import = import_specifier
        .separated_by(comma.clone())
        .allow_trailing()
        .collect::<Vec<_>>()
        .delimited_by(lbrace.clone(), rbrace.clone())
        .map(ImportClause::Named);
    let namespace_import = just(T::Star)
        .ignore_then(kw(Kw::As))
        .ignore_then(ident)
        .map(|alias| ImportClause::Namespace { alias });
    let import_statement = kw(Kw::Import)
        .ignore_then(named_import.or(namespace_import))
        .then_ignore(soft("from"))
        .then(string_raw.map(|raw| trim_quotes(&raw)))
        .map_with(|(import_clause, module), e| {
            Ast::ImportStatement(ImportStatement {
                import_clause,
                module,
                span: sp(e.span()),
            })
        });

    // The unittest name keeps its surrounding quotes.
    let assert_stmt = kw(Kw::Assert)
        .ignore_then(extends_expr.clone())
        .map_with(|claim, e| {
            Ast::Assert(Assert {
                span: sp(e.span()),
                claim: Rc::new(claim),
            })
        });
    let unittest = kw(Kw::Unittest)
        .ignore_then(string_raw)
        .then_ignore(soft("do"))
        .then(assert_stmt.repeated().collect::<Vec<_>>())
        .then_ignore(kw(Kw::End))
        .map_with(|(name, body), e| {
            Ast::UnitTest(UnitTest {
                span: sp(e.span()),
                name,
                body,
            })
        });

    let statement = choice((
        type_alias.clone(),
        interface.clone(),
        unique_symbol_decl,
        import_statement,
        unittest,
    ))
    .map(|inner| Ast::Statement(Rc::new(inner)))
    .labelled("a statement")
    .boxed();

    let program = statement
        .repeated()
        .collect::<Vec<_>>()
        .map_with(|statements, e| {
            Ast::Program(Program {
                statements,
                span: sp(e.span()),
            })
        })
        .boxed();

    Parsers {
        program,
        expr: expr.boxed(),
        extends_expr: extends_expr.boxed(),
        if_expr,
        map_expr: map_expr_p,
        interface,
        object_literal: object_literal.map(Ast::TypeLiteral).boxed(),
        tuple,
        type_alias,
    }
}
