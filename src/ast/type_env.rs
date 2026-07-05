//! A global type environment for resolving top-level names during assignability
//! checks (used by the `unittest` harness).
//!
//! Programs are otherwise transpiled symbolically — `type Foo as 1` becomes
//! `type Foo = 1` and references to `Foo` stay as identifiers — so the
//! assignability engine, on its own, treats every named reference as a free type
//! variable. To make assertions like `assert Foo <: number` meaningful, this
//! module builds a symbol table from a program's top-level `type` aliases and
//! `interface`s and resolves references (including generic applications) to their
//! definitions on demand.
//!
//! Each distinct instantiation (`Foo`, `Id(1)`, …) is expanded at most once and
//! interned in a [`slotmap::SlotMap`], so repeated and mutually-referential
//! definitions are cheap. Genuinely recursive definitions don't expand forever:
//! resolution is *lazy* (one level at a time, driven by the engine) and the
//! engine carries a coinductive [`ResolveCtx::assumptions`] set, so a relation
//! encountered while it is still being proven is taken to hold.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use slotmap::{new_key_type, SlotMap};

use crate::ast::{
    ApplyGeneric, Ast, ExtendsExpr, Ident, Interface, IntersectionType, Span, Tuple, TypeAlias,
    TypeLiteral, TypeParameter, UnionType, UniqueSymbol,
};
use crate::extends_result::ExtendsResult;

new_key_type! {
    /// Handle for an interned, resolved type instantiation.
    pub struct TypeId;
}

/// One parameter of a definition: its name, optional default (which may
/// reference earlier parameters), and whether it is a rest parameter absorbing
/// the remaining arguments.
struct Param {
    name: String,
    default: Option<Ast>,
    rest: bool,
    /// The `where T <: BOUND` constraint's bound type, if any.
    constraint: Option<Ast>,
}

impl Param {
    fn from(param: &TypeParameter) -> Self {
        Self {
            name: param.name.clone(),
            default: param.default.clone(),
            rest: param.rest,
            constraint: param.constraint.clone(),
        }
    }
}

/// A named, possibly-parameterized type definition (a `type` alias or an
/// `interface`, unified into one shape: parameters plus a body).
struct Def {
    params: Vec<Param>,
    body: Ast,
}

/// A generic application with the wrong number of type arguments.
pub struct ArityError {
    pub span: Span,
    pub message: String,
}

/// Symbol table built from a program's top-level definitions, with an interner
/// for resolved instantiations.
pub struct TypeEnv {
    defs: HashMap<String, Def>,
    arena: RefCell<SlotMap<TypeId, Ast>>,
    cache: RefCell<HashMap<String, TypeId>>,
}

impl TypeEnv {
    /// Build an environment from the top-level `type` aliases and `interface`s of
    /// `program`. Other statements are ignored.
    pub fn from_program(program: &Ast) -> Self {
        let mut defs = HashMap::new();

        for node in top_level_nodes(program) {
            match node {
                Ast::TypeAlias(TypeAlias {
                    name, params, body, ..
                }) => {
                    defs.insert(
                        name.name.clone(),
                        Def {
                            params: params.iter().map(Param::from).collect(),
                            body: (**body).clone(),
                        },
                    );
                }
                Ast::Interface(interface) => {
                    defs.insert(interface.name.clone(), interface_def(interface));
                }
                // Register each `unique symbol` so bare references to it in
                // assert claims (which are not rewritten by `simplify`) resolve
                // to the distinct `UniqueSymbol` type at evaluation time.
                Ast::UniqueSymbolDecl(sym) => {
                    defs.insert(
                        sym.name.clone(),
                        Def {
                            params: vec![],
                            body: Ast::UniqueSymbol(UniqueSymbol {
                                name: sym.name.clone(),
                                span: sym.span,
                            }),
                        },
                    );
                }
                _ => {}
            }
        }

        Self {
            defs,
            arena: RefCell::new(SlotMap::with_key()),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Whether the environment has no definitions (so resolution is a no-op).
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// If `ast` is a reference to a known definition — a bare `Ident`, or an
    /// `ApplyGeneric` whose receiver names one — return its body with any type
    /// arguments substituted for parameters. Otherwise `None`.
    pub fn resolve_head(&self, ast: &Ast) -> Option<Ast> {
        match ast {
            Ast::Ident(Ident { name, .. }) => self.instantiate(name, &[]),
            Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) => match receiver.as_ref() {
                Ast::Ident(Ident { name, .. }) => self.instantiate(name, args),
                _ => None,
            },
            _ => None,
        }
    }

    /// Expand `name(args)` to its definition's body, substituting `args` for the
    /// definition's parameters. Interns and caches the result.
    fn instantiate(&self, name: &str, args: &[Ast]) -> Option<Ast> {
        let def = self.defs.get(name)?;

        let key = instantiation_key(name, args);
        if let Some(id) = self.cache.borrow().get(&key) {
            return Some(self.arena.borrow()[*id].clone());
        }

        let body = if def.params.is_empty() {
            def.body.clone()
        } else {
            let bindings = bind_params(&def.params, args);
            distribute_or_substitute(&def.body, &bindings)
        };

        let id = self.arena.borrow_mut().insert(body.clone());
        self.cache.borrow_mut().insert(key, id);
        Some(body)
    }

    /// Report generic applications in `claim` that reference a known definition
    /// with the wrong number of type arguments. Applications of unknown names are
    /// left to resolve as free variables (indeterminate), not flagged here.
    pub fn arity_errors(&self, claim: &Ast) -> Vec<ArityError> {
        let errors = RefCell::new(Vec::new());

        claim.prewalk((), &|node, ()| {
            if let Some(error) = self.check_application(&node) {
                errors.borrow_mut().push(error);
            }
            (node, ())
        });

        errors.into_inner()
    }

    /// Report generic applications in `claim` whose type arguments violate a
    /// `where` constraint on the referenced definition (TypeScript TS2344). The
    /// argument bound to each constrained parameter must be assignable to the
    /// constraint's bound (with earlier parameters substituted in, so `where B <:
    /// A` is checked against the actual `A`). Only a *definite* failure is
    /// reported — an indeterminate relation (a free variable, `any`) is allowed.
    pub fn constraint_errors(&self, claim: &Ast, ctx: &ResolveCtx) -> Vec<ArityError> {
        let errors = RefCell::new(Vec::new());

        claim.prewalk((), &|node, ()| {
            if let Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) = &node {
                if let Ast::Ident(Ident { name, .. }) = receiver.as_ref() {
                    if let Some(def) = self.defs.get(name) {
                        if def.params.iter().any(|p| p.constraint.is_some()) {
                            let bindings = bind_params(&def.params, args);
                            for param in &def.params {
                                let (Some(constraint), Some(value)) =
                                    (&param.constraint, bindings.get(&param.name))
                                else {
                                    continue;
                                };
                                let bound = substitute(constraint, &bindings);
                                if value.is_assignable_to_ctx(&bound, ctx) == ExtendsResult::False {
                                    errors.borrow_mut().push(ArityError {
                                        span: node.as_span(),
                                        message: format!(
                                            "type argument for `{}` does not satisfy its \
                                            constraint in generic `{name}`",
                                            param.name
                                        ),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            (node, ())
        });

        errors.into_inner()
    }

    fn check_application(&self, node: &Ast) -> Option<ArityError> {
        let Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) = node else {
            return None;
        };
        let Ast::Ident(Ident { name, .. }) = receiver.as_ref() else {
            return None;
        };
        let def = self.defs.get(name)?;

        let has_rest = def.params.iter().any(|p| p.rest);
        let max = def.params.iter().filter(|p| !p.rest).count();
        let required = def
            .params
            .iter()
            .filter(|p| !p.rest && p.default.is_none())
            .count();
        let found = args.len();

        if found >= required && (has_rest || found <= max) {
            return None;
        }

        let expectation = if has_rest {
            format!("at least {required}")
        } else if required == max {
            format!("{required}")
        } else {
            format!("between {required} and {max}")
        };

        Some(ArityError {
            span: node.as_span(),
            message: format!(
                "generic `{name}` expects {expectation} type argument(s), but {found} \
                {} provided",
                if found == 1 { "was" } else { "were" }
            ),
        })
    }
}

/// Bind `args` to `params` by position, applying defaults for omitted trailing
/// parameters and collecting any rest parameter's arguments into a tuple.
/// Defaults may reference earlier parameters, so bindings accumulate left to
/// right. Arity is assumed valid (checked separately by [`TypeEnv::arity_errors`]).
fn bind_params(params: &[Param], args: &[Ast]) -> HashMap<String, Ast> {
    let mut bindings = HashMap::new();
    let mut rest_args = args.iter();

    for param in params {
        if param.rest {
            let items: Vec<Ast> = rest_args.by_ref().cloned().collect();
            bindings.insert(
                param.name.clone(),
                Ast::Tuple(Tuple {
                    items,
                    span: Span::new(0, 0),
                }),
            );
            continue;
        }

        let value = match rest_args.next() {
            Some(arg) => arg.clone(),
            None => match &param.default {
                // A default may use earlier parameters, so substitute what we
                // have bound so far.
                Some(default) => substitute(default, &bindings),
                None => continue,
            },
        };

        bindings.insert(param.name.clone(), value);
    }

    bindings
}

/// The top-level statement nodes of a program (unwrapping the `Statement`
/// wrapper), or the node itself if it isn't a program.
pub(crate) fn top_level_nodes(program: &Ast) -> impl Iterator<Item = &Ast> {
    let statements = match program {
        Ast::Program(program) => program.statements.as_slice(),
        other => std::slice::from_ref(other),
    };

    statements.iter().map(|statement| match statement {
        Ast::Statement(inner) => inner.as_ref(),
        other => other,
    })
}

/// Convert an interface into a definition. `interface Foo extends Bar { … }` is
/// modelled as `Bar & { … }`: resolution expands `Bar` and the existing
/// intersection assignability merges the inherited shape.
fn interface_def(interface: &Interface) -> Def {
    let own = Ast::TypeLiteral(TypeLiteral {
        properties: interface.definition.clone(),
        span: interface.span,
    });

    let body = match &interface.extends {
        Some(parent) => Ast::IntersectionType(IntersectionType {
            types: vec![parent.as_ref().clone(), own],
            span: interface.span,
        }),
        None => own,
    };

    Def {
        params: interface.params.iter().map(Param::from).collect(),
        body,
    }
}

/// Instantiate a definition body against `bindings`, reproducing TypeScript's
/// *distributive conditional types*: when the body is a conditional whose check
/// type is a naked type parameter bound to a union, the conditional distributes
/// over the union members — each member is substituted into the whole body and
/// the results are unioned (`ToArray<A | B>` ≡ `ToArray<A> | ToArray<B>`).
/// Otherwise the body is substituted directly. A wrapped check type (e.g.
/// `[T] extends …`) is not a naked parameter, so it does not distribute.
fn distribute_or_substitute(body: &Ast, bindings: &HashMap<String, Ast>) -> Ast {
    if let Ast::ExtendsExpr(ExtendsExpr { lhs, span, .. }) = body {
        if let Ast::Ident(Ident { name, .. }) = lhs.as_ref() {
            // The naked parameter distributes over the union members it is bound
            // to. `boolean` is the union `true | false`, so it distributes too
            // (`if T <: true …` over `boolean` yields both branches).
            let members: Option<Vec<Ast>> = match bindings.get(name) {
                Some(Ast::UnionType(UnionType { types, .. })) => Some(types.clone()),
                Some(Ast::Primitive(crate::ast::PrimitiveType::Boolean, s)) => {
                    Some(vec![Ast::TrueKeyword(*s), Ast::FalseKeyword(*s)])
                }
                _ => None,
            };
            if let Some(members) = members {
                let variants = members
                    .iter()
                    .map(|member| {
                        let mut member_bindings = bindings.clone();
                        member_bindings.insert(name.clone(), member.clone());
                        substitute(body, &member_bindings)
                    })
                    .collect();
                return Ast::UnionType(UnionType {
                    types: variants,
                    span: *span,
                });
            }
        }
    }

    substitute(body, bindings)
}

/// Substitute bound type arguments for parameter names throughout `body`.
pub(crate) fn substitute(body: &Ast, bindings: &HashMap<String, Ast>) -> Ast {
    let (tree, _) = body.prewalk(bindings.clone(), &|ast, bindings| match ast {
        Ast::Ident(ref id) => {
            let replacement = bindings.get(&id.name).cloned().unwrap_or(ast);
            (replacement, bindings)
        }
        _ => (ast, bindings),
    });

    tree
}

/// Cache key for an instantiation: the definition name plus a structural
/// fingerprint of each argument.
fn instantiation_key(name: &str, args: &[Ast]) -> String {
    let mut key = name.to_string();
    for arg in args {
        key.push('\u{1}');
        key.push_str(&fingerprint(arg));
    }
    key
}

/// A stable, structural fingerprint of a node (its s-expression form), used to
/// key instantiations and coinductive assumptions.
pub fn fingerprint(ast: &Ast) -> String {
    ast.to_sexp()
        .map(|value| value.to_string())
        .unwrap_or_default()
}

/// Context threaded through an assignability check: the (optional) environment
/// for resolving references, and the set of relations currently being proven (so
/// recursive definitions terminate coinductively).
pub struct ResolveCtx<'a> {
    env: Option<&'a TypeEnv>,
    assumptions: RefCell<HashSet<(String, String)>>,
}

impl<'a> ResolveCtx<'a> {
    /// A context with no environment: references are not resolved (the original
    /// purely-structural behavior).
    pub fn empty() -> Self {
        Self {
            env: None,
            assumptions: RefCell::new(HashSet::new()),
        }
    }

    /// A context that resolves references against `env`.
    pub fn new(env: &'a TypeEnv) -> Self {
        Self {
            env: Some(env),
            assumptions: RefCell::new(HashSet::new()),
        }
    }

    /// The environment, if any.
    pub(crate) fn env(&self) -> Option<&TypeEnv> {
        self.env
    }

    /// Record `pair` as assumed-assignable for the duration of a sub-proof.
    /// Returns `false` if it was already assumed (a recursive revisit).
    pub(crate) fn assume(&self, pair: &(String, String)) -> bool {
        self.assumptions.borrow_mut().insert(pair.clone())
    }

    /// Discharge an assumption recorded by [`assume`](Self::assume).
    pub(crate) fn discharge(&self, pair: &(String, String)) {
        self.assumptions.borrow_mut().remove(pair);
    }
}
