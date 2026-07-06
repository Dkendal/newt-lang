use std::collections::{HashMap, HashSet};

use crate::{
    ast::{
        dbg_expr::{Decision, TraceEvent},
        type_env::{fingerprint, frame_label, one_line, substitute, ResolveCtx},
        Access, ApplyGeneric, Ast, Builtin, BuiltinKeyword, ExtendsExpr, FunctionType, Ident,
        IntersectionType, MappedType, MappingModifier, ObjectProperty, PrimitiveType, PropertyName,
        Span, Tuple, TypeLiteral, TypeNumber, TypeString, UnionType,
    },
    extends_result::ExtendsResult,
};

/// One piece of a parsed template-literal type: a fixed literal run, or a
/// `${…}` placeholder constrained to a primitive domain.
enum TemplateSeg {
    Lit(String),
    Str,
    Num,
    Bool,
    Big,
}

/// Structural classification of an object-property key, used to relate object
/// type literals that carry index signatures.
enum KeyClass<'a> {
    /// A plain named property, e.g. `a` in `{ a: T }`.
    Named(&'a str),
    /// A `string` index signature, `{ [k in string]: V }`.
    StringIndex,
    /// A `number` index signature, `{ [k in number]: V }`.
    NumberIndex,
    /// A key we cannot reason about structurally: a computed key, a remapped
    /// index, or an index over a non-`string`/`number` iterable.
    Other,
}

impl Ast {
    /// Assignability test mirroring the TypeScript checker's assignable
    /// relation (`internal/checker/relater.go`). Answers "is `self` assignable
    /// to `other`" in the sense of a conditional type `self extends other`.
    ///
    /// This is the purely-structural entry point: named references (idents,
    /// generic applications, …) are treated as free type variables. To resolve
    /// references against a program's definitions, use
    /// [`is_assignable_to_ctx`](Self::is_assignable_to_ctx) with a populated
    /// [`ResolveCtx`].
    pub fn is_assignable_to(&self, other: &Ast) -> ExtendsResult {
        self.is_assignable_to_ctx(other, &ResolveCtx::empty())
    }

    /// Assignability under a resolution context. When `ctx` carries an
    /// environment, named references on either side are resolved to their
    /// definitions and the check recurses; a coinductive assumption set keeps
    /// recursive definitions terminating.
    ///
    /// The four outcomes are interpreted by the runtime (`runtime::builtin::unquote`):
    /// `True` → take the `then` branch; `False` → take the `else` branch;
    /// `Never` → the LHS is the bottom type, collapse to `never`; `Both` → the
    /// relation is indeterminate (the LHS is `any`, or an unresolvable
    /// reference), so the union of both branches is produced.
    pub fn is_assignable_to_ctx(&self, other: &Ast, ctx: &ResolveCtx) -> ExtendsResult {
        type A = Ast;
        type T = ExtendsResult;

        // Read-only `dbg!` observation hook: evaluation demands both operands
        // of a relation, so this is where a watched span becomes "observed".
        // Must not affect any return value or control flow.
        if let Some(sink) = ctx.env().and_then(|env| env.dbg()) {
            sink.observe(self);
            sink.observe(other);
        }

        // Reduce top-level string-literal indexed accesses (`T["k"]`, nested
        // `T["a"]["b"]`) on either operand to the referenced property's type
        // before relating. Only fully-resolved accesses are substituted in;
        // anything the reducer cannot resolve (numeric/`keyof` keys, arrays,
        // unresolved objects, cycles) yields `None` and falls through to the
        // structural match (ultimately the `Access => Both` arm). Gated on an
        // environment, since property lookup resolves named object references.
        if ctx.env().is_some() {
            let lhs = Self::reduce_access_leaf(self, ctx);
            let rhs = Self::reduce_access_leaf(other, ctx);
            if lhs.is_some() || rhs.is_some() {
                let l = lhs.as_ref().unwrap_or(self);
                let r = rhs.as_ref().unwrap_or(other);
                return l.is_assignable_to_ctx(r, ctx);
            }
        }

        // Resolve named references against the environment first. If either side
        // names a definition, expand it and recurse — guarding the relation so a
        // recursive definition encountered while proving itself is taken to hold.
        if let Some(env) = ctx.env() {
            let lhs_resolved = env.resolve_head(self);
            let rhs_resolved = env.resolve_head(other);

            if lhs_resolved.is_some() || rhs_resolved.is_some() {
                let pair = (fingerprint(self), fingerprint(other));
                if !ctx.assume(&pair) {
                    return T::True;
                }
                // `dbg!` stacktrace frames: the resolved body is evaluated by
                // the recursion below, so each resolved side's application
                // stays on the stack for exactly that extent. Read-only.
                let sink = ctx.env().and_then(|env| env.dbg());
                let _lhs_frame = sink
                    .filter(|_| lhs_resolved.is_some())
                    .map(|s| s.frame(frame_label(self), self.as_span()));
                let _rhs_frame = sink
                    .filter(|_| rhs_resolved.is_some())
                    .map(|s| s.frame(frame_label(other), other.as_span()));
                let lhs = lhs_resolved.as_ref().unwrap_or(self);
                let rhs = rhs_resolved.as_ref().unwrap_or(other);
                let result = lhs.is_assignable_to_ctx(rhs, ctx);
                ctx.discharge(&pair);
                return result;
            }
        }

        // Conditional types (`check extends T ? then : else`) on either side are
        // reduced to the selected branch before relating. Distribution over a
        // naked union check type is handled upstream (at instantiation, in
        // `TypeEnv`), so by this point the check type is a single type and a
        // plain reduction — plus `infer` extraction — is correct.
        if let Ast::ExtendsExpr(e) = self {
            let reduced = Self::reduce_conditional(e, ctx);
            return reduced.is_assignable_to_ctx(other, ctx);
        }
        if let Ast::ExtendsExpr(e) = other {
            let reduced = Self::reduce_conditional(e, ctx);
            return self.is_assignable_to_ctx(&reduced, ctx);
        }

        // Literal/primitive source assignable to its own bare primitive target
        // (e.g. `1 <: number`, `"x" <: string`, `{} <: object`). Object wrappers
        // are excluded: a wrapper (`String`, `Number`, …) is an object value and
        // is NOT assignable to its bare primitive, so it must not collapse here.
        if let Ast::Primitive(other, _) = other {
            if !self.is_object_wrapper() {
                if let Some(value) = self.get_primitive_type() {
                    if value == *other {
                        return T::True;
                    }
                }
            }
        }

        match (self, other) {
            (x, _) if !x.is_typescript_feature() => {
                unreachable!()
            }

            (A::NeverKeyword(_), _) => T::Never,

            (lhs, rhs) if lhs == rhs => T::True,

            (_, rhs) if rhs.is_top_type() => T::True,

            (A::AnyKeyword(_), _) => T::Both,

            // `unknown` is assignable only to a top type (handled above) — except
            // a union/intersection target may *contain* a top type. Defer those to
            // the set-membership folds so `unknown <: number | unknown` holds and
            // `unknown <: string | number` still fails.
            (A::UnknownKeyword(_), A::UnionType(UnionType { types, .. })) => {
                Self::assignable_to_union(self, types, ctx)
            }

            (A::UnknownKeyword(_), A::IntersectionType(IntersectionType { types, .. })) => {
                Self::assignable_to_intersection(self, types, ctx)
            }

            (A::UnknownKeyword(_), _) => T::False,

            // `keyof X` on either side. When `X` is (or resolves to) an object
            // literal, evaluate `keyof X` to the union of its plain string-named
            // keys and relate via the normal union logic. Both arms precede the
            // set-operation arms below: a source `keyof` must expand before the
            // target-union arm would otherwise distribute over it, and a target
            // `keyof` must expand before the structural arms. A non-enumerable
            // argument (arrays, primitives, unresolved refs, index signatures,
            // mapped types) leaves the relation indeterminate.
            (
                A::Builtin(Builtin {
                    name: BuiltinKeyword::Keyof,
                    argument,
                    ..
                }),
                rhs,
            ) => match Self::eval_keyof(argument, ctx) {
                Some(keys) => keys.is_assignable_to_ctx(rhs, ctx),
                None => T::Both,
            },

            (
                lhs,
                A::Builtin(Builtin {
                    name: BuiltinKeyword::Keyof,
                    argument,
                    ..
                }),
            ) => match Self::eval_keyof(argument, ctx) {
                Some(keys) => lhs.is_assignable_to_ctx(&keys, ctx),
                None => T::Both,
            },

            // Mapped types `{ [K in T]: V }`. When the key set `T` is a
            // statically-known set of literal keys (a union/singleton of string
            // literals, `never`, or `keyof O` for an object literal `O`), expand
            // the mapped type to the equivalent object literal and relate
            // structurally. An unknown key set (a `string`/`number` primitive
            // iterable, a template remap, an unresolved reference) leaves the
            // relation indeterminate. The target arm must precede the structural
            // `TypeLiteral`/`Tuple`/… source arms below, which would otherwise
            // mishandle a mapped-type target.
            (A::MappedType(mt), rhs) => match Self::expand_mapped_type(mt, ctx) {
                Some(tl) => Ast::TypeLiteral(tl).is_assignable_to_ctx(rhs, ctx),
                None => T::Both,
            },

            (lhs, A::MappedType(mt)) => match Self::expand_mapped_type(mt, ctx) {
                Some(tl) => lhs.is_assignable_to_ctx(&Ast::TypeLiteral(tl), ctx),
                None => T::Both,
            },

            // Set operations. Source-union: every member must be assignable.
            // Source-intersection: some member (or the merged shape) must be.
            // Target-union: assignable to some member. Target-intersection:
            // assignable to every member. Source arms come first so that
            // `(A | B) <: (C | D)` distributes correctly, and — crucially — so a
            // source intersection that reduces to `never` (e.g. `string & number`)
            // collapses to `Never` even against a `never` target, instead of being
            // caught by the `(_, never) => False` arm below.
            (A::UnionType(UnionType { types, .. }), rhs) => {
                Self::union_source_assignable(types, rhs, ctx)
            }

            (A::IntersectionType(IntersectionType { types, .. }), rhs) => {
                Self::intersection_source_assignable(types, rhs, ctx)
            }

            (_, A::NeverKeyword(_)) => T::False,

            (lhs, A::UnionType(UnionType { types, .. })) => {
                Self::assignable_to_union(lhs, types, ctx)
            }

            (lhs, A::IntersectionType(IntersectionType { types, .. })) => {
                Self::assignable_to_intersection(lhs, types, ctx)
            }

            // `readonly` arrays/tuples. The relation is one-directional: a
            // mutable array/tuple is assignable to a `readonly` one, but not
            // the reverse. A readonly source is thus unassignable to a mutable
            // array/tuple target; against any other target it behaves like the
            // underlying array/tuple. These arms must precede the source-side
            // `Array`/`Tuple` arms so a `readonly` operand is not mishandled.
            (A::Readonly(src), rhs) => match rhs {
                A::Readonly(tgt) => src.is_assignable_to_ctx(tgt, ctx),
                A::Array(_) | A::Tuple(_) => T::False,
                _ => src.is_assignable_to_ctx(rhs, ctx),
            },

            (lhs, A::Readonly(tgt)) => lhs.is_assignable_to_ctx(tgt, ctx),

            // Indexed-access / generic-application / qualified references cannot
            // be resolved without a type environment (bindings are substituted
            // away upstream), so a reference reaching this engine is a free type
            // variable: indeterminate.
            (A::Access(Access { .. }), _) => T::Both,

            (A::ApplyGeneric(_), _) => T::Both,

            (A::Array(element), rhs) => Self::array_assignable_to(element, rhs, ctx),

            (A::Path(_), _) => T::Both,

            // A bare, unresolvable type reference (e.g. `Foo`) is a free type
            // variable — indeterminate, like the deferred conditional TypeScript
            // would produce. Object-wrapper idents (`String`, `Object`, …) have a
            // primitive type and are handled structurally further down, so they
            // are excluded here. This must precede the `Object`-wrapper target
            // arm below, otherwise `Foo <: Object` would wrongly resolve to True.
            // The global `Function` interface is an object value that carries
            // call signatures: it is assignable to the any-object targets
            // (`object`, `{}`, `Object`) but NOT to any specific function type
            // (its signature is `(...args: any[]) => any`, which is not
            // assignable to an arbitrary concrete signature). Must precede the
            // free-variable `Ident` arm below.
            (lhs, rhs) if lhs.is_function_interface() => {
                if Self::accepts_any_object(rhs) {
                    T::True
                } else {
                    T::False
                }
            }

            (lhs @ A::Ident(_), _) if lhs.get_primitive_type().is_none() => T::Both,

            // Every non-nullish value is assignable to the empty object type `{}`
            // and to the global `Object` interface (both denote "any non-nullish
            // value"). This admits bare primitives (`string <: {}`, `1 <: {}`) and
            // `object` itself. The `object` *primitive* is intentionally NOT
            // included here — it rejects primitives (`string <: object` is false)
            // and is handled structurally below.
            (lhs, rhs)
                if lhs.is_non_nullish()
                    && (rhs.is_object_object_wrapper() || rhs.is_empty_object()) =>
            {
                T::True
            }

            (A::TypeLiteral(lhs), rhs) => Self::type_literal_assignable_to(lhs, rhs, ctx),

            // `undefined` is assignable to `void` under --strict (the `void`
            // domain includes `undefined`). The relation is one-directional:
            // `void <: undefined` is false, and `null` is assignable to
            // neither — those fall through to the identity check below.
            (A::Primitive(PrimitiveType::Undefined, _), A::Primitive(PrimitiveType::Void, _)) => {
                T::True
            }

            (A::Primitive(lhs, _), A::Primitive(rhs, _)) => Into::into(lhs == rhs),

            (A::TemplateString(_) | A::TypeString(_), A::Primitive(PrimitiveType::String, _)) => {
                T::True
            }

            // A concrete string literal against a template-literal *pattern*
            // (`"abc" <: \`a${string}\``): match the literal text against the
            // pattern's fixed runs and `${…}` placeholders. An unrecognised
            // placeholder domain leaves the relation indeterminate.
            (A::TypeString(s), A::TemplateString(t)) => match Self::parse_template(&t.ty) {
                Some(segs) => Into::into(Self::template_matches(&segs, &s.ty)),
                None => T::Both,
            },

            (
                A::Primitive(PrimitiveType::String, _) | A::TemplateString(_) | A::TypeString(_),
                rhs,
            ) if rhs.is_string_object_wrapper() => T::True,

            (A::TypeNumber(_), A::Primitive(PrimitiveType::Number, _)) => T::True,

            // Two numeric literal types are the same type iff they denote the same
            // numeric *value*, regardless of surface spelling (`1.0`/`1`,
            // `1.50`/`1.5`, `1_000`/`1000`, `0`/`-0`). The identity arm above
            // handles textually-equal cases; this compares parsed values.
            (A::TypeNumber(a), A::TypeNumber(b)) => {
                match (Self::number_value(&a.ty), Self::number_value(&b.ty)) {
                    (Some(x), Some(y)) => Into::into(x == y),
                    _ => Into::into(a.ty == b.ty),
                }
            }

            // Object wrappers as TARGETS (`Number`, `Boolean`, `BigInt`,
            // `Symbol` — `Object` is the any-object target handled above). A
            // value is assignable to the wrapper iff its primitive type matches
            // the wrapper's primitive: this admits the bare primitive
            // (`number <: Number`), the matching literals (`1 <: Number`,
            // `true <: Boolean`), and the same wrapper (`Number <: Number`),
            // while rejecting cross-primitive and non-primitive sources.
            (lhs, rhs) if rhs.is_object_wrapper() && !rhs.is_object_object_wrapper() => {
                // `get_primitive_type` maps literals and wrapper idents to their
                // primitive but not a bare `Primitive`, so match that directly.
                let lhs_prim = match lhs {
                    A::Primitive(p, _) => Some(p.clone()),
                    _ => lhs.get_primitive_type(),
                };
                match (lhs_prim, rhs.get_primitive_type()) {
                    (Some(l), Some(r)) if l == r => T::True,
                    _ => T::False,
                }
            }

            // Object wrappers as SOURCES are object values: assignable to the
            // any-object targets (`{}`, `object`, `Object` — `Object` is handled
            // by the non-nullish arm above), but NOT to their bare primitive.
            (lhs, rhs) if lhs.is_object_wrapper() => Into::into(Self::accepts_any_object(rhs)),

            (A::Tuple(tuple), rhs) => Self::tuple_assignable_to(tuple, rhs, ctx),

            (A::FunctionType(lhs), rhs) => Self::function_assignable_to(lhs, rhs, ctx),

            (_, _) => T::False,
        }
    }

    /// `[A, B, …]` (array element type `element`) assignable to `rhs`.
    fn array_assignable_to(element: &Ast, rhs: &Ast, ctx: &ResolveCtx) -> ExtendsResult {
        match rhs {
            // Covariant element relation: `A[] <: B[]` iff `A <: B`.
            Ast::Array(rhs_element) => {
                ExtendsResult::True.and(element.is_assignable_to_ctx(rhs_element, ctx))
            }
            // A variable-length array is not assignable to a fixed-arity tuple.
            Ast::Tuple(_) => ExtendsResult::False,
            rhs if Self::accepts_any_object(rhs) => ExtendsResult::True,
            // An array `T[]` has the apparent members `length: number` and a
            // numeric index signature `[k in number]: T`, so it can satisfy an
            // object type that asks for those (`number[] <: { length: number }`).
            Ast::TypeLiteral(_) => {
                let shape = Self::array_object_shape(element, element.as_span());
                Ast::TypeLiteral(shape).is_assignable_to_ctx(rhs, ctx)
            }
            _ => ExtendsResult::False,
        }
    }

    /// The apparent object shape of an array `element[]`: a required
    /// `length: number` plus a numeric index signature carrying the element type.
    fn array_object_shape(element: &Ast, span: Span) -> TypeLiteral {
        TypeLiteral {
            properties: vec![
                ObjectProperty {
                    readonly: false,
                    optional: false,
                    key: PropertyName::LiteralPropertyName("length".to_string()),
                    value: Ast::Primitive(PrimitiveType::Number, span),
                    span,
                },
                ObjectProperty {
                    readonly: false,
                    optional: false,
                    key: PropertyName::Index(crate::ast::PropertyKeyIndex {
                        key: "k".to_string(),
                        iterable: Ast::Primitive(PrimitiveType::Number, span),
                        remapped_as: None,
                        span,
                    }),
                    value: element.clone(),
                    span,
                },
            ],
            span,
        }
    }

    /// `(p…) => R` assignable to `rhs`. A function value is also an object, so
    /// it is assignable to the global `Function` interface and the any-object
    /// targets. Against another function type the relation mirrors the
    /// TypeScript checker's `compareSignatures`
    /// (`internal/checker/relater.go`): the source may omit trailing
    /// parameters but must not require more than the target supplies,
    /// parameters relate contravariantly (strict function types), and the
    /// return type relates covariantly — except a target return of `any` or
    /// `void` accepts any source return.
    fn function_assignable_to(lhs: &FunctionType, rhs: &Ast, ctx: &ResolveCtx) -> ExtendsResult {
        let Ast::FunctionType(target) = rhs else {
            return if rhs.is_function_interface() || Self::accepts_any_object(rhs) {
                ExtendsResult::True
            } else {
                ExtendsResult::False
            };
        };

        // Normalize each signature into its fixed (leading, non-rest) parameter
        // types plus an optional rest element type — a `...a: T[]` rest absorbs
        // any number of trailing arguments of type `T`.
        let (source_fixed, source_rest) = Self::split_params(&lhs.params);
        let (target_fixed, target_rest) = Self::split_params(&target.params);

        // Arity. The source must not require more parameters than the target can
        // supply: a target rest parameter supplies unboundedly, otherwise only
        // its fixed parameters. Fewer source parameters are fine (the source
        // simply ignores the extra arguments the target passes).
        let source_required = source_fixed.len();
        let target_supply = if target_rest.is_some() {
            usize::MAX
        } else {
            target_fixed.len()
        };
        if source_required > target_supply {
            return ExtendsResult::False;
        }

        // Parameters are contravariant: at each position the target supplies, the
        // target parameter type must be assignable to the source parameter type.
        // An `any` in either position is bidirectionally compatible. A position
        // the source does not cover (no fixed parameter and no rest) imposes no
        // constraint — the source ignores that argument.
        let mut acc = ExtendsResult::True;
        let positions = source_fixed.len().max(target_fixed.len());
        for i in 0..positions {
            let source_ty = source_fixed.get(i).copied().or(source_rest);
            let target_ty = target_fixed.get(i).copied().or(target_rest);
            let (Some(source_ty), Some(target_ty)) = (source_ty, target_ty) else {
                continue;
            };
            let related = if matches!(source_ty, Ast::AnyKeyword(_))
                || matches!(target_ty, Ast::AnyKeyword(_))
            {
                ExtendsResult::True
            } else {
                target_ty.is_assignable_to_ctx(source_ty, ctx)
            };
            acc = acc.and(related);
        }

        // Rest-element contravariance. Two rest parameters (`...a: S[]` vs
        // `...b: T[]`) relate the rest *elements* contravariantly, just like a
        // fixed position. The per-position loop above only covers indices up to
        // the larger fixed-parameter count, so when both signatures are
        // pure-rest (no fixed params) the elements are otherwise never compared.
        if let (Some(source_ty), Some(target_ty)) = (source_rest, target_rest) {
            let related = if matches!(source_ty, Ast::AnyKeyword(_))
                || matches!(target_ty, Ast::AnyKeyword(_))
            {
                ExtendsResult::True
            } else {
                target_ty.is_assignable_to_ctx(source_ty, ctx)
            };
            acc = acc.and(related);
        }

        // Return type. A target return of `any` or `void` is satisfied by any
        // source return; otherwise the relation is covariant. A source return of
        // `any` is assignable to every target return except `never` (`any` is not
        // assignable to the bottom type).
        let target_return = target.return_type.as_ref();
        let target_return_absorbs = matches!(
            target_return,
            Ast::AnyKeyword(_) | Ast::Primitive(PrimitiveType::Void, _)
        );
        let return_relation = if target_return_absorbs {
            ExtendsResult::True
        } else if matches!(lhs.return_type.as_ref(), Ast::AnyKeyword(_)) {
            Into::into(!matches!(target_return, Ast::NeverKeyword(_)))
        } else {
            lhs.return_type.is_assignable_to_ctx(target_return, ctx)
        };

        acc.and(return_relation)
    }

    /// Split a parameter list into its leading fixed (non-rest) parameter types
    /// and the element type of a trailing rest parameter, if any. A rest
    /// parameter `...a: T[]` contributes element type `T`; a non-array rest type
    /// contributes itself.
    fn split_params(params: &[crate::ast::Parameter]) -> (Vec<&Ast>, Option<&Ast>) {
        let mut fixed = Vec::new();
        let mut rest = None;
        for param in params {
            // A `this` parameter is a typing annotation, not a real argument
            // position — TypeScript erases it from the signature's arity.
            if param.name == "this" {
                continue;
            }
            if param.ellipsis {
                rest = Some(match &param.kind {
                    Ast::Array(element) => element.as_ref(),
                    other => other,
                });
                break;
            }
            fixed.push(&param.kind);
        }
        (fixed, rest)
    }

    /// `[A, B, …]` tuple assignable to `rhs`.
    fn tuple_assignable_to(tuple: &Tuple, rhs: &Ast, ctx: &ResolveCtx) -> ExtendsResult {
        match rhs {
            // Element-wise, same arity (no optional/rest modelling in tuples).
            Ast::Tuple(other) => {
                if tuple.items.len() != other.items.len() {
                    return ExtendsResult::False;
                }
                tuple
                    .items
                    .iter()
                    .zip(other.items.iter())
                    .fold(ExtendsResult::True, |acc, (a, b)| {
                        acc.and(a.is_assignable_to_ctx(b, ctx))
                    })
            }
            // `[A, B] <: T[]` iff every element is assignable to `T`. An empty
            // tuple is vacuously assignable to any array.
            Ast::Array(rhs_element) => tuple.items.iter().fold(ExtendsResult::True, |acc, item| {
                acc.and(item.is_assignable_to_ctx(rhs_element, ctx))
            }),
            rhs if Self::accepts_any_object(rhs) => ExtendsResult::True,
            // A tuple `[A, B]` has the apparent members `length: <literal arity>`
            // and numeric-named element properties (`0: A`, `1: B`), so it can
            // satisfy an object type that asks for those.
            Ast::TypeLiteral(_) => {
                let shape = Self::tuple_object_shape(tuple);
                Ast::TypeLiteral(shape).is_assignable_to_ctx(rhs, ctx)
            }
            _ => ExtendsResult::False,
        }
    }

    /// The apparent object shape of a tuple `[A, B, …]`: a required `length`
    /// whose type is the literal arity, plus a numeric-named property per element.
    fn tuple_object_shape(tuple: &Tuple) -> TypeLiteral {
        let span = tuple.span;
        let mut properties = vec![ObjectProperty {
            readonly: false,
            optional: false,
            key: PropertyName::LiteralPropertyName("length".to_string()),
            value: Ast::TypeNumber(TypeNumber {
                ty: tuple.items.len().to_string(),
                span,
            }),
            span,
        }];
        for (i, item) in tuple.items.iter().enumerate() {
            properties.push(ObjectProperty {
                readonly: false,
                optional: false,
                key: PropertyName::LiteralPropertyName(i.to_string()),
                value: item.clone(),
                span,
            });
        }
        TypeLiteral { properties, span }
    }

    /// Object type literal assignable to `rhs`. Structural width and depth
    /// subtyping: every required property of the target must be present on the
    /// source with an assignable value; extra source properties are allowed (no
    /// fresh-literal excess-property check at this layer). Handles the empty
    /// source `{}` too — it is assignable to any all-optional target.
    fn type_literal_assignable_to(lhs: &TypeLiteral, rhs: &Ast, ctx: &ResolveCtx) -> ExtendsResult {
        match rhs {
            rhs if Self::accepts_any_object(rhs) => ExtendsResult::True,
            Ast::TypeLiteral(target) => {
                // Any key we can't reason about structurally — a computed key, a
                // remapped index, or an index whose iterable is neither the
                // `string` nor the `number` primitive (a union/keyof/mapped
                // iterable) — leaves the whole relation indeterminate.
                let any_other = lhs
                    .iter()
                    .chain(target.iter())
                    .any(|p| matches!(Self::classify_key(&p.key), KeyClass::Other));
                if any_other {
                    return ExtendsResult::Both;
                }

                // Weak-type rule. A "weak" target is a non-empty object whose
                // members are ALL optional plain properties (no required member
                // and no index/call/construct signature). TypeScript rejects a
                // source that has at least one property of its own yet shares no
                // property name with such a target ("has no properties in
                // common"). An empty source `{}` is exempt, and a source carrying
                // an index signature is treated as compatible.
                let target_named: Vec<&str> = target
                    .iter()
                    .filter_map(|p| match Self::classify_key(&p.key) {
                        KeyClass::Named(name) => Some(name),
                        _ => None,
                    })
                    .collect();
                let target_weak = !target_named.is_empty()
                    && target.iter().all(|p| {
                        matches!(Self::classify_key(&p.key), KeyClass::Named(_)) && p.optional
                    });
                if target_weak {
                    let mut source_has_named = false;
                    let mut source_has_index = false;
                    let mut shares = false;
                    for p in lhs.iter() {
                        match Self::classify_key(&p.key) {
                            KeyClass::Named(name) => {
                                source_has_named = true;
                                if target_named.contains(&name) {
                                    shares = true;
                                }
                            }
                            KeyClass::StringIndex | KeyClass::NumberIndex => {
                                source_has_index = true
                            }
                            KeyClass::Other => {}
                        }
                    }
                    if source_has_named && !source_has_index && !shares {
                        return ExtendsResult::False;
                    }
                }

                // Index the source by kind: named properties (by name), and the
                // optional string/number index-signature value types. A source
                // index signature does NOT supply named properties.
                let mut source_named: Vec<(&str, &ObjectProperty)> = vec![];
                let mut source_string_index: Option<&Ast> = None;
                let mut source_number_index: Option<&Ast> = None;
                for p in lhs.iter() {
                    match Self::classify_key(&p.key) {
                        KeyClass::Named(name) => source_named.push((name, p)),
                        KeyClass::StringIndex => source_string_index = Some(&p.value),
                        KeyClass::NumberIndex => source_number_index = Some(&p.value),
                        KeyClass::Other => unreachable!("filtered out above"),
                    }
                }

                let mut acc = ExtendsResult::True;
                for target_prop in target.iter() {
                    let relation = match Self::classify_key(&target_prop.key) {
                        KeyClass::Named(name) => {
                            match source_named.iter().find(|(n, _)| *n == name) {
                                Some((_, source_prop)) => {
                                    Self::property_relation(source_prop, target_prop, ctx)
                                }
                                // A source index signature does NOT satisfy a
                                // required named target.
                                None if target_prop.optional => ExtendsResult::True,
                                None => ExtendsResult::False,
                            }
                        }
                        // A target string index must accept every source member
                        // (every named value, plus either index value type).
                        KeyClass::StringIndex => {
                            let vt = &target_prop.value;
                            let mut r = ExtendsResult::True;
                            for (_, source_prop) in &source_named {
                                r = r.and(source_prop.value.is_assignable_to_ctx(vt, ctx));
                            }
                            if let Some(sv) = source_string_index {
                                r = r.and(sv.is_assignable_to_ctx(vt, ctx));
                            }
                            if let Some(sv) = source_number_index {
                                r = r.and(sv.is_assignable_to_ctx(vt, ctx));
                            }
                            r
                        }
                        // A target number index constrains only numeric-named
                        // source members, plus either index value type.
                        KeyClass::NumberIndex => {
                            let vt = &target_prop.value;
                            let mut r = ExtendsResult::True;
                            for (name, source_prop) in &source_named {
                                if Self::is_numeric_name(name) {
                                    r = r.and(source_prop.value.is_assignable_to_ctx(vt, ctx));
                                }
                            }
                            if let Some(sv) = source_number_index {
                                r = r.and(sv.is_assignable_to_ctx(vt, ctx));
                            }
                            if let Some(sv) = source_string_index {
                                r = r.and(sv.is_assignable_to_ctx(vt, ctx));
                            }
                            r
                        }
                        KeyClass::Other => unreachable!("filtered out above"),
                    };
                    acc = acc.and(relation);
                }
                acc
            }
            _ => ExtendsResult::False,
        }
    }

    /// Classify an object-property key for the structural index-signature
    /// relation. A plain key is `Named`; a `[k in string]` / `[k in number]`
    /// index signature is `StringIndex` / `NumberIndex`; everything else (a
    /// computed key, a remapped index, or an index over a non-`string`/`number`
    /// iterable such as a union, `keyof`, or mapped iterable) is `Other`.
    fn classify_key(key: &PropertyName) -> KeyClass<'_> {
        match key {
            PropertyName::LiteralPropertyName(name) => KeyClass::Named(name),
            PropertyName::Index(index) if index.remapped_as.is_none() => match &index.iterable {
                Ast::Primitive(PrimitiveType::String, _) => KeyClass::StringIndex,
                Ast::Primitive(PrimitiveType::Number, _) => KeyClass::NumberIndex,
                _ => KeyClass::Other,
            },
            _ => KeyClass::Other,
        }
    }

    /// Whether a plain property name is a numeric key (constrained by a `number`
    /// index signature). TypeScript treats a property whose name is a valid
    /// number literal as a numeric key.
    fn is_numeric_name(name: &str) -> bool {
        name.parse::<f64>().is_ok()
    }

    /// Parse a numeric-literal type's surface text to its value, stripping the
    /// `_` digit separators TypeScript allows. `None` if it isn't a plain decimal
    /// literal (the caller then falls back to textual identity).
    fn number_value(text: &str) -> Option<f64> {
        text.replace('_', "").parse::<f64>().ok()
    }

    /// Parse a template-literal type's raw text (with surrounding backticks) into
    /// fixed runs and `${…}` placeholders. Returns `None` if a placeholder names
    /// a domain we don't match here (a literal, union, or reference), so the
    /// caller stays indeterminate rather than guessing.
    fn parse_template(raw: &str) -> Option<Vec<TemplateSeg>> {
        let inner = raw.strip_prefix('`')?.strip_suffix('`')?;
        let mut segs = vec![];
        let mut literal = String::new();
        let mut rest = inner;
        while let Some(start) = rest.find("${") {
            literal.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let end = after.find('}')?;
            let placeholder = after[..end].trim();
            if !literal.is_empty() {
                segs.push(TemplateSeg::Lit(std::mem::take(&mut literal)));
            }
            segs.push(match placeholder {
                "string" | "any" => TemplateSeg::Str,
                "number" => TemplateSeg::Num,
                "boolean" => TemplateSeg::Bool,
                "bigint" => TemplateSeg::Big,
                _ => return None,
            });
            rest = &after[end + 1..];
        }
        literal.push_str(rest);
        if !literal.is_empty() {
            segs.push(TemplateSeg::Lit(literal));
        }
        Some(segs)
    }

    /// Whether `text` matches a parsed template pattern, with backtracking over
    /// the open-ended placeholders.
    fn template_matches(segs: &[TemplateSeg], text: &str) -> bool {
        match segs.split_first() {
            None => text.is_empty(),
            Some((TemplateSeg::Lit(lit), tail)) => match text.strip_prefix(lit.as_str()) {
                Some(remainder) => Self::template_matches(tail, remainder),
                None => false,
            },
            Some((placeholder, tail)) => {
                // Try every prefix of `text` that is valid for this placeholder's
                // domain, then match the remainder against the rest of the
                // pattern. `string`/`any` admit the empty prefix too.
                let valid = |prefix: &str| match placeholder {
                    TemplateSeg::Str => true,
                    TemplateSeg::Num => Self::number_value(prefix).is_some(),
                    TemplateSeg::Bool => prefix == "true" || prefix == "false",
                    TemplateSeg::Big => {
                        !prefix.is_empty()
                            && prefix
                                .strip_prefix('-')
                                .unwrap_or(prefix)
                                .bytes()
                                .all(|b| b.is_ascii_digit())
                    }
                    TemplateSeg::Lit(_) => unreachable!(),
                };
                let start = if matches!(placeholder, TemplateSeg::Str) {
                    0
                } else {
                    1
                };
                for end in start..=text.len() {
                    if !text.is_char_boundary(end) {
                        continue;
                    }
                    if valid(&text[..end]) && Self::template_matches(tail, &text[end..]) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Relation between a present source property and the target property it
    /// matches by name.
    fn property_relation(
        source: &ObjectProperty,
        target: &ObjectProperty,
        ctx: &ResolveCtx,
    ) -> ExtendsResult {
        // An optional source property may be absent, so it is not assignable to a
        // required target property.
        if source.optional && !target.optional {
            return ExtendsResult::False;
        }

        // An optional target property `x?: T` has effective type `T | undefined`
        // under --strict without `exactOptionalPropertyTypes`, so the source
        // value need only be assignable to `T | undefined`. With
        // `exactOptionalPropertyTypes` the widening is disabled and the source
        // must be assignable to `T` itself.
        if target.optional && !ctx.exact_optional_property_types() {
            let undefined = Ast::Primitive(PrimitiveType::Undefined, target.value.as_span());
            let widened = Ast::UnionType(UnionType {
                types: vec![target.value.clone(), undefined],
                span: target.value.as_span(),
            });
            return source.value.is_assignable_to_ctx(&widened, ctx);
        }

        source.value.is_assignable_to_ctx(&target.value, ctx)
    }

    /// Source union: assignable iff *every* member is assignable to `rhs`.
    fn union_source_assignable(members: &[Ast], rhs: &Ast, ctx: &ResolveCtx) -> ExtendsResult {
        members.iter().fold(ExtendsResult::True, |acc, member| {
            acc.and(member.is_assignable_to_ctx(rhs, ctx))
        })
    }

    /// Target union: assignable iff `lhs` is assignable to *some* member.
    fn assignable_to_union(lhs: &Ast, members: &[Ast], ctx: &ResolveCtx) -> ExtendsResult {
        // `boolean` is the union `true | false`; it is assignable to a target
        // union only if both of its members are.
        if matches!(lhs, Ast::Primitive(PrimitiveType::Boolean, _)) {
            let span = lhs.as_span();
            let t = Ast::TrueKeyword(span);
            let f = Ast::FalseKeyword(span);
            return Self::assignable_to_union(&t, members, ctx)
                .and(Self::assignable_to_union(&f, members, ctx));
        }

        members.iter().fold(ExtendsResult::False, |acc, member| {
            acc.or(lhs.is_assignable_to_ctx(member, ctx))
        })
    }

    /// Target intersection: assignable iff `lhs` is assignable to *every* member.
    fn assignable_to_intersection(lhs: &Ast, members: &[Ast], ctx: &ResolveCtx) -> ExtendsResult {
        members.iter().fold(ExtendsResult::True, |acc, member| {
            acc.and(lhs.is_assignable_to_ctx(member, ctx))
        })
    }

    /// Source intersection: assignable iff *some* member is assignable, or —
    /// for an object target — the merged shape of all object members is.
    fn intersection_source_assignable(
        members: &[Ast],
        rhs: &Ast,
        ctx: &ResolveCtx,
    ) -> ExtendsResult {
        // Treat a left-associated `A & B & C` as the flat set `{A, B, C}` so the
        // disjointness, merge, and membership logic sees every member.
        let flat = Self::flatten_intersection_members(members);
        let members = &flat[..];

        // Distribute an intersection containing a union: `(A | B) & C` ≡
        // `(A & C) | (B & C)`. Relate every product-combination (a union-free
        // intersection) to `rhs`; the whole relation holds iff each surviving
        // combination does — a contradictory combination reduces to `never` and
        // is vacuously satisfied, which is exactly how the empty members of the
        // distributed union drop out (`(1|2|3) & (2|3|4)` ≡ `2 | 3`).
        if members.iter().any(|m| matches!(m, Ast::UnionType(_))) {
            return Self::distribute_intersection(members)
                .iter()
                .fold(ExtendsResult::True, |acc, combo| {
                    acc.and(Self::intersection_source_assignable(combo, rhs, ctx))
                });
        }

        // `never & T` reduces to `never`, which collapses the conditional.
        if members.iter().any(|m| matches!(m, Ast::NeverKeyword(_))) {
            return ExtendsResult::Never;
        }

        // A genuinely contradictory intersection is uninhabited, so it reduces
        // to `never` (the bottom type), mirroring TS. Two members are
        // contradictory when both are drawn from disjoint *unit* primitive/literal
        // kinds (`string`/`number`/`boolean`/`bigint`/`symbol` and their literals)
        // yet share no common subtype — neither is assignable to the other. This
        // catches `string & number`, `string & boolean`, `1 & 2`, `true & false`,
        // while leaving `1 & number` (→ `1`) and `string & "a"` (→ `"a"`) alone,
        // since there one member is assignable to the other. Object members
        // (`{a:1} & {b:2}`, `string & {}`) never participate here — disjoint object
        // shapes intersect to a non-`never` type and are handled by the merge below.
        for (i, a) in members.iter().enumerate() {
            if Self::disjoint_primitive_family(a).is_none() {
                continue;
            }
            for b in &members[i + 1..] {
                if Self::disjoint_primitive_family(b).is_none() {
                    continue;
                }
                let a_to_b = a.is_assignable_to_ctx(b, ctx);
                let b_to_a = b.is_assignable_to_ctx(a, ctx);
                if a_to_b != ExtendsResult::True && b_to_a != ExtendsResult::True {
                    return ExtendsResult::Never;
                }
            }
        }

        // `null`/`undefined`/`void` are disjoint from every non-top type they are
        // not mutually assignable to (under --strict), so an intersection pairing
        // a nullish member with such a member is uninhabited and reduces to
        // `never` (`null & string`, `undefined & {a:1}`, `null & undefined`). Top
        // types (`unknown`/`any`) are skipped — they are identity/collapse for
        // intersection — and `void & undefined` survives because `undefined <:
        // void`.
        for (i, a) in members.iter().enumerate() {
            for b in &members[i + 1..] {
                if !(a.is_nullish() || b.is_nullish()) {
                    continue;
                }
                if a.is_top_type() || b.is_top_type() {
                    continue;
                }
                let a_to_b = a.is_assignable_to_ctx(b, ctx);
                let b_to_a = b.is_assignable_to_ctx(a, ctx);
                if a_to_b != ExtendsResult::True && b_to_a != ExtendsResult::True {
                    return ExtendsResult::Never;
                }
            }
        }

        let some = members.iter().fold(ExtendsResult::False, |acc, member| {
            acc.or(member.is_assignable_to_ctx(rhs, ctx))
        });

        if some == ExtendsResult::True {
            return some;
        }

        // The intersection carries the union of all its object members'
        // properties, so merge them and try the combined shape against the
        // target (e.g. `{a:1} & {b:2} <: {a:number, b:number}`, and likewise
        // against a union target containing such a shape). The merged members
        // may themselves be references (e.g. an interface's `extends` parent),
        // so resolve each before merging.
        let resolved: Vec<Ast> = members
            .iter()
            .map(|member| Self::resolve_for_merge(member, ctx))
            .collect();

        if let Some(merged) = Self::merge_object_members(&resolved) {
            return some.or(Ast::TypeLiteral(merged).is_assignable_to_ctx(rhs, ctx));
        }

        some
    }

    /// Resolve a node to its definition body if it is a known reference, for the
    /// purpose of merging object members (interface inheritance). Non-references
    /// are returned unchanged.
    fn resolve_for_merge(member: &Ast, ctx: &ResolveCtx) -> Ast {
        ctx.env()
            .and_then(|env| env.resolve_head(member))
            .unwrap_or_else(|| member.clone())
    }

    /// Collect the properties of every object-literal member into one shape. A
    /// plain key carried by more than one member has its value types
    /// *intersected* (`{a: T} & {a: U}` → `{a: T & U}`), mirroring how TypeScript
    /// merges an intersection of object types; non-plain keys (index signatures)
    /// are carried through as-is.
    fn merge_object_members(members: &[Ast]) -> Option<TypeLiteral> {
        let mut properties: Vec<ObjectProperty> = vec![];
        let mut span = None;

        for member in members {
            if let Ast::TypeLiteral(tl) = member {
                if span.is_none() {
                    span = Some(tl.span);
                }
                for prop in tl.properties.iter() {
                    if let PropertyName::LiteralPropertyName(name) = &prop.key {
                        if let Some(existing) = properties.iter_mut().find(
                            |p| matches!(&p.key, PropertyName::LiteralPropertyName(n) if n == name),
                        ) {
                            existing.value = Ast::IntersectionType(IntersectionType {
                                types: vec![existing.value.clone(), prop.value.clone()],
                                span: prop.span,
                            });
                            // A merged key is optional only if optional in every
                            // member, and readonly if readonly in any.
                            existing.optional = existing.optional && prop.optional;
                            existing.readonly = existing.readonly || prop.readonly;
                            continue;
                        }
                    }
                    properties.push(prop.clone());
                }
            }
        }

        span.map(|span| TypeLiteral { properties, span })
    }

    /// Cartesian product of an intersection's members, expanding each union
    /// member into its alternatives: `[(A|B), C]` → `[[A, C], [B, C]]`. Each
    /// returned combination is a union-free member list.
    fn distribute_intersection(members: &[Ast]) -> Vec<Vec<Ast>> {
        let mut result: Vec<Vec<Ast>> = vec![vec![]];
        for member in members {
            let options: Vec<Ast> = match member {
                Ast::UnionType(UnionType { types, .. }) => types.clone(),
                other => vec![other.clone()],
            };
            let mut next = Vec::with_capacity(result.len() * options.len());
            for combo in &result {
                for option in &options {
                    let mut extended = combo.clone();
                    extended.push(option.clone());
                    next.push(extended);
                }
            }
            result = next;
        }
        result
    }

    /// Flatten nested intersection members into a single level so a
    /// left-associated `A & B & C` is treated as the set `{A, B, C}` for the
    /// disjointness, merge, and membership checks below.
    fn flatten_intersection_members(members: &[Ast]) -> Vec<Ast> {
        let mut out = vec![];
        for member in members {
            match member {
                Ast::IntersectionType(IntersectionType { types, .. }) => {
                    out.extend(Self::flatten_intersection_members(types));
                }
                _ => out.push(member.clone()),
            }
        }
        out
    }

    /// Targets that accept any object value: the empty object `{}`, the
    /// `object` primitive, and the `Object` interface.
    fn accepts_any_object(rhs: &Ast) -> bool {
        rhs.is_empty_object()
            || matches!(rhs, Ast::Primitive(PrimitiveType::Object, _))
            || rhs.is_object_interface()
    }

    /// The "unit" primitive family of an intersection member, used to detect a
    /// contradictory (uninhabited) intersection. Returns the family for a bare
    /// primitive keyword or a literal of `string`/`number`/`boolean`/`bigint`/
    /// `symbol`. Object-ish kinds (object literals, the `object` primitive, the
    /// wrapper objects like `String`/`Number`) and the nullish primitives
    /// (`void`/`undefined`/`null`) are excluded — their intersections are not
    /// contradictory in this disjoint-primitive sense.
    fn disjoint_primitive_family(ast: &Ast) -> Option<PrimitiveType> {
        // Object-wrapper idents (`String`, `Number`, …) are object values, not the
        // bare primitive, so they do not participate in primitive contradiction.
        if ast.is_object_wrapper() {
            return None;
        }

        // `get_primitive_type` maps literals/wrappers to their primitive but not a
        // bare `Primitive`, so match that directly.
        let prim = match ast {
            Ast::Primitive(p, _) => p.clone(),
            _ => ast.get_primitive_type()?,
        };

        match prim {
            PrimitiveType::String
            | PrimitiveType::Number
            | PrimitiveType::Boolean
            | PrimitiveType::BigInt
            | PrimitiveType::Symbol => Some(prim),
            PrimitiveType::Object
            | PrimitiveType::Void
            | PrimitiveType::Undefined
            | PrimitiveType::Null => None,
        }
    }

    /// Evaluate `keyof arg` to the set of its plain string-named keys, as
    /// `TypeString` literal nodes. Returns `None` (indeterminate) unless `arg`
    /// is — or resolves via the environment to — an object literal whose keys
    /// are *all* plain names (`PropertyName::LiteralPropertyName`). Index, computed, or
    /// remapped keys, and non-object arguments, are not enumerable here and
    /// yield `None`. Exposed as a reusable building block (mapped types).
    fn keyof_string_keys(arg: &Ast, ctx: &ResolveCtx) -> Option<Vec<Ast>> {
        let resolved = ctx.env().and_then(|env| env.resolve_head(arg));
        let target = resolved.as_ref().unwrap_or(arg);

        let Ast::TypeLiteral(literal) = target else {
            return None;
        };

        let mut keys = Vec::with_capacity(literal.properties.len());
        for prop in literal.iter() {
            match &prop.key {
                PropertyName::LiteralPropertyName(name) => keys.push(Ast::TypeString(TypeString {
                    ty: name.clone(),
                    span: literal.span,
                })),
                // A non-plain key can't be enumerated structurally.
                _ => return None,
            }
        }
        Some(keys)
    }

    /// Evaluate `keyof arg` to a single relatable type. For an object literal
    /// this is the union of its string-named keys (a bare key when there is one,
    /// `never` for the empty object). Additional reducible forms:
    ///
    /// * `keyof (A | B)` = `keyof A & keyof B`: for object literals, the
    ///   INTERSECTION of the members' key sets (the shared plain keys).
    /// * `keyof (A & B)` = `keyof A | keyof B`: the UNION of the members' key
    ///   sets — but ONLY when the members' key sets are pairwise DISJOINT. A key
    ///   name shared between members may collapse the intersection to `never` in
    ///   tsgo (conflicting value types make the property `never`, e.g.
    ///   `{a: 1} & {a: 's'}`), turning `keyof` into `string | number | symbol`;
    ///   the engine does not model that collapse, so a shared key name yields
    ///   `None` (indeterminate) instead of a wrong-definite key union.
    /// * `keyof unknown` = `never`; `keyof never` = `string | number | symbol`.
    ///
    /// `None` when the argument is not one of these enumerable forms (a
    /// primitive, array, tuple, unresolved reference, index signature, …),
    /// leaving the relation indeterminate.
    fn eval_keyof(arg: &Ast, ctx: &ResolveCtx) -> Option<Ast> {
        // Read-only `dbg!` observation hook: the `keyof` argument as
        // received, before resolution.
        if let Some(sink) = ctx.env().and_then(|env| env.dbg()) {
            sink.observe(arg);
        }

        // The synthesized result gets its own span, deliberately NOT aliasing
        // `arg`'s: reusing the argument's span (as the rest of this function
        // used to) makes the *computed* key type indistinguishable, to the
        // `dbg!` watch-span index, from a fresh observation of the watched
        // argument itself — a `dbg!(User)` under a `keyof` would then double-
        // fire (once for `User`, once for the derived key union carrying the
        // same coordinates). `Span` is excluded from `Ast`'s `PartialEq`
        // (see its `impl PartialEq for Span`), so this is invisible to every
        // other consumer; only the `dbg!` sink's exact-span lookup can tell
        // the difference.
        let span = Span::new(0, 0);
        let resolved = ctx.env().and_then(|env| env.resolve_head(arg));
        let target = resolved.as_ref().unwrap_or(arg);

        match target {
            // `keyof unknown` is the empty key set.
            Ast::UnknownKeyword(_) => Some(Ast::NeverKeyword(span)),

            // `keyof never` is every possible key domain.
            Ast::NeverKeyword(_) => Some(Ast::UnionType(UnionType {
                types: vec![
                    Ast::Primitive(PrimitiveType::String, span),
                    Ast::Primitive(PrimitiveType::Number, span),
                    Ast::Primitive(PrimitiveType::Symbol, span),
                ],
                span,
            })),

            // `keyof (A | B)` = `keyof A & keyof B`: the intersection of the
            // members' key sets. All-or-nothing: every member must reduce.
            Ast::UnionType(UnionType { types, .. }) => {
                let mut members = types.iter();
                let mut acc = Self::keyof_key_names(members.next()?, ctx)?;
                for member in members {
                    let names = Self::keyof_key_names(member, ctx)?;
                    acc.retain(|n| names.contains(n));
                }
                Some(Self::names_to_key_union(acc, span))
            }

            // `keyof (A & B)` = `keyof A | keyof B`: the union of the members'
            // key sets. All-or-nothing: every member must reduce, and the key
            // sets must be pairwise disjoint — a shared key name may collapse
            // the intersection to `never` in tsgo (conflicting value types),
            // which is not modelled here, so it stays indeterminate.
            Ast::IntersectionType(IntersectionType { types, .. }) => {
                let flat = Self::flatten_intersection_members(types);
                let mut acc: Vec<String> = vec![];
                for member in &flat {
                    for name in Self::keyof_key_names(member, ctx)? {
                        if acc.contains(&name) {
                            return None;
                        }
                        acc.push(name);
                    }
                }
                Some(Self::names_to_key_union(acc, span))
            }

            _ => {
                let names = Self::keyof_key_names(target, ctx)?;
                Some(Self::names_to_key_union(names, span))
            }
        }
    }

    /// The plain string key names of `arg` (an object literal or a reference to
    /// one), in declaration order. `None` when `arg` is not an enumerable object
    /// literal (mirrors `keyof_string_keys`).
    fn keyof_key_names(arg: &Ast, ctx: &ResolveCtx) -> Option<Vec<String>> {
        let keys = Self::keyof_string_keys(arg, ctx)?;
        Some(
            keys.into_iter()
                .map(|k| match k {
                    Ast::TypeString(TypeString { ty, .. }) => ty,
                    _ => unreachable!("keyof_string_keys yields TypeString nodes"),
                })
                .collect(),
        )
    }

    /// Build the relatable key type from a set of plain key names: `never` for
    /// the empty set, a bare `TypeString` for a singleton, else a union.
    fn names_to_key_union(names: Vec<String>, span: Span) -> Ast {
        let mut keys: Vec<Ast> = names
            .into_iter()
            .map(|ty| Ast::TypeString(TypeString { ty, span }))
            .collect();
        match keys.len() {
            0 => Ast::NeverKeyword(span),
            1 => keys.pop().unwrap(),
            _ => Ast::UnionType(UnionType { types: keys, span }),
        }
    }

    /// Expand a mapped type `{ [K in T]: V }` to the equivalent object literal,
    /// when its key set `T` is a statically-known set of literal string keys.
    /// Returns `None` (so the caller stays indeterminate) when the key set isn't
    /// enumerable, or when the mapped type carries an `as` key-remap clause
    /// (deferred). For each key, the index variable `K` is substituted in the
    /// body `V`, and a homomorphic indexed-access body `O[K]` is reduced one step
    /// when it resolves; an irreducible body is left in place (relating as `Both`
    /// later). The mapped type's `+optional`/`+readonly` modifiers carry onto the
    /// generated properties.
    fn expand_mapped_type(mt: &MappedType, ctx: &ResolveCtx) -> Option<TypeLiteral> {
        // An `as` remap clause rewrites keys; defer that for now.
        if mt.remapped_as.is_some() {
            return None;
        }

        let keys = Self::mapped_key_set(&mt.iterable, ctx, &mut HashSet::new())?;

        let add_optional = matches!(mt.optional_mod, Some(MappingModifier::Add));
        let add_readonly = matches!(mt.readonly_mod, Some(MappingModifier::Add));

        // For a homomorphic mapped type `[k in keyof O]`, the source object's
        // per-property `readonly`/`optional` modifiers carry onto the generated
        // properties; the mapped type's own `+readonly`/`+optional` only *add* to
        // them. (Modifier *removal* `-readonly`/`-?` is not modelled yet.)
        let source_mods = Self::homomorphic_source_mods(&mt.iterable, ctx);

        let mut properties = Vec::with_capacity(keys.len());
        for key in keys {
            // The key must be a plain string literal to name a property.
            let Ast::TypeString(TypeString { ty: name, .. }) = &key else {
                return None;
            };

            let (src_optional, src_readonly) =
                source_mods.get(name).copied().unwrap_or((false, false));

            // Substitute the index variable with this key in the body, then try a
            // one-step reduction of a homomorphic `O[K]` indexed access.
            let mut bindings = HashMap::new();
            bindings.insert(mt.index.clone(), key.clone());
            let body = substitute(&mt.body, &bindings, None);
            let value = Self::reduce_indexed_access(&body, ctx);

            properties.push(ObjectProperty {
                readonly: add_readonly || src_readonly,
                optional: add_optional || src_optional,
                key: PropertyName::LiteralPropertyName(name.clone()),
                value,
                span: mt.span,
            });
        }

        Some(TypeLiteral {
            properties,
            span: mt.span,
        })
    }

    /// For a homomorphic mapped iterable `keyof O` where `O` resolves to an
    /// object literal, return each plain key's `(optional, readonly)` modifiers so
    /// they can be preserved on the mapped result. Empty for any non-homomorphic
    /// iterable (a literal key union, a primitive, …).
    fn homomorphic_source_mods(iterable: &Ast, ctx: &ResolveCtx) -> HashMap<String, (bool, bool)> {
        let mut out = HashMap::new();
        if let Ast::Builtin(Builtin {
            name: BuiltinKeyword::Keyof,
            argument,
            ..
        }) = iterable
        {
            let resolved = ctx.env().and_then(|env| env.resolve_head(argument));
            let target = resolved.as_ref().unwrap_or(argument);
            if let Ast::TypeLiteral(tl) = target {
                for p in tl.iter() {
                    if let PropertyName::LiteralPropertyName(name) = &p.key {
                        out.insert(name.clone(), (p.optional, p.readonly));
                    }
                }
            }
        }
        out
    }

    /// Resolve the key set of a mapped type's iterable to a vector of literal key
    /// nodes, or `None` when it isn't a statically-known set of string-literal
    /// keys. A `never` iterable yields the empty set (`{}`); a single string
    /// literal a singleton; a union of string literals those keys; `keyof O` the
    /// keys of an object literal `O`; a reference is resolved one step and
    /// recursed (guarded by `seen` so a cyclic alias terminates).
    fn mapped_key_set(
        iterable: &Ast,
        ctx: &ResolveCtx,
        seen: &mut HashSet<String>,
    ) -> Option<Vec<Ast>> {
        match iterable {
            Ast::NeverKeyword(_) => Some(vec![]),
            Ast::TypeString(_) => Some(vec![iterable.clone()]),
            Ast::UnionType(UnionType { types, .. }) => {
                let mut keys = Vec::with_capacity(types.len());
                for ty in types {
                    match ty {
                        Ast::TypeString(_) => keys.push(ty.clone()),
                        _ => return None,
                    }
                }
                Some(keys)
            }
            Ast::Builtin(Builtin {
                name: BuiltinKeyword::Keyof,
                argument,
                ..
            }) => Self::keyof_string_keys(argument, ctx),
            other => {
                if !seen.insert(fingerprint(other)) {
                    return None;
                }
                let resolved = ctx.env().and_then(|env| env.resolve_head(other))?;
                Self::mapped_key_set(&resolved, ctx, seen)
            }
        }
    }

    /// Reduce a conditional type `check extends extends_ty ? then : else` to the
    /// branch it selects. When `extends_ty` carries `infer` positions, the check
    /// type is structurally matched against the pattern to bind each `infer`
    /// variable; on a successful match the bindings are substituted into the
    /// `then` branch (duplicate covariant `infer` occurrences union), otherwise
    /// the `else` branch is taken. Without `infer`, the relation `check <:
    /// extends_ty` selects the branch (`True` → then, `False` → else, `Never` →
    /// `never`, indeterminate → the union of both branches).
    fn reduce_conditional(e: &ExtendsExpr, ctx: &ResolveCtx) -> Ast {
        let ExtendsExpr {
            lhs,
            rhs,
            then_branch,
            else_branch,
            span,
        } = e;

        // Read-only `dbg!` observation hook: the check type as received,
        // before any resolution. Must not affect the branch selected.
        if let Some(sink) = ctx.env().and_then(|env| env.dbg()) {
            sink.observe(lhs);
        }

        // Resolve a named check type one step (e.g. an alias) before matching.
        let resolved = ctx.env().and_then(|env| env.resolve_head(lhs));
        // `dbg!` stacktrace frame for a named check type: alive while the
        // resolved check is related below. Read-only.
        let sink = ctx.env().and_then(|env| env.dbg());
        let _check_frame = sink
            .filter(|_| resolved.is_some())
            .map(|s| s.frame(frame_label(lhs), lhs.as_span()));
        let check = resolved.unwrap_or_else(|| (**lhs).clone());

        if Self::contains_infer(rhs) {
            let mut bindings: HashMap<String, Vec<Ast>> = HashMap::new();
            let matched = Self::match_infer(&check, rhs, ctx, &mut bindings);
            Self::trace_conditional(
                ctx,
                *span,
                &check,
                rhs,
                if matched {
                    Decision::Then
                } else {
                    Decision::Else
                },
            );
            if matched {
                let resolved: HashMap<String, Ast> = bindings
                    .into_iter()
                    .map(|(name, types)| (name, Self::union_of(types, *span)))
                    .collect();
                return substitute(then_branch, &resolved, None);
            }
            return (**else_branch).clone();
        }

        let result = check.is_assignable_to_ctx(rhs, ctx);
        Self::trace_conditional(ctx, *span, &check, rhs, Self::decision_of(result));
        match result {
            ExtendsResult::True => (**then_branch).clone(),
            ExtendsResult::False => (**else_branch).clone(),
            ExtendsResult::Never => Ast::NeverKeyword(*span),
            ExtendsResult::Both => Ast::UnionType(UnionType {
                types: vec![(**then_branch).clone(), (**else_branch).clone()],
                span: *span,
            }),
        }
    }

    /// `--trace-eval`: one event per conditional decision, anchored at the
    /// conditional's span and carrying which branch was taken. Read-only —
    /// only appends to the sink's log when trace mode is on and the sink
    /// isn't paused (see `DbgSink::trace`); never affects the branch
    /// selected above.
    fn trace_conditional(
        ctx: &ResolveCtx,
        span: Span,
        check: &Ast,
        extends: &Ast,
        decision: Decision,
    ) {
        let Some(sink) = ctx.env().and_then(|env| env.dbg()) else {
            return;
        };
        if !sink.trace_enabled() {
            return;
        }
        sink.trace(TraceEvent {
            span,
            message: format!("{} <: {}", one_line(check), one_line(extends)),
            decision: Some(decision),
        });
    }

    /// The `--trace-eval` decision for a definite/indeterminate relation
    /// result, matching the branch [`Self::reduce_conditional`] selects.
    fn decision_of(result: ExtendsResult) -> Decision {
        match result {
            ExtendsResult::True => Decision::Then,
            ExtendsResult::False => Decision::Else,
            ExtendsResult::Never => Decision::Never,
            ExtendsResult::Both => Decision::Both,
        }
    }

    /// Whether `ast` contains an `infer` node anywhere in its subtree.
    fn contains_infer(ast: &Ast) -> bool {
        let any = |items: &[Ast]| items.iter().any(Self::contains_infer);
        match ast {
            Ast::Infer(_) => true,
            Ast::Array(inner) | Ast::Readonly(inner) => Self::contains_infer(inner),
            Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) => {
                Self::contains_infer(receiver) || any(args)
            }
            Ast::FunctionType(f) => {
                f.params.iter().any(|p| Self::contains_infer(&p.kind))
                    || Self::contains_infer(&f.return_type)
            }
            Ast::Tuple(t) => any(&t.items),
            Ast::TypeLiteral(l) => l.iter().any(|p| Self::contains_infer(&p.value)),
            Ast::UnionType(u) => any(&u.types),
            Ast::IntersectionType(i) => any(&i.types),
            Ast::Access(a) => Self::contains_infer(&a.lhs) || Self::contains_infer(&a.rhs),
            Ast::Builtin(b) => Self::contains_infer(&b.argument),
            _ => false,
        }
    }

    /// Combine inferred occurrences of a single `infer` variable into one type,
    /// deduplicating structurally and unioning multiple covariant occurrences.
    ///
    /// LIMITATION: TypeScript only unions duplicate `infer`s in *covariant*
    /// positions; in *contravariant* (parameter) positions it intersects them
    /// (so `(a: infer U, b: infer U) => any` against `(string, number) => any`
    /// yields `string & number`). We always union. This is currently latent —
    /// no caller reaches a contravariant duplicate, and the parser does not yet
    /// accept the multi-`infer` function syntax that would express it.
    fn union_of(types: Vec<Ast>, span: Span) -> Ast {
        let mut seen = HashSet::new();
        let mut unique = Vec::with_capacity(types.len());
        for ty in types {
            if seen.insert(fingerprint(&ty)) {
                unique.push(ty);
            }
        }
        match unique.len() {
            0 => Ast::NeverKeyword(span),
            1 => unique.into_iter().next().unwrap(),
            _ => Ast::UnionType(UnionType {
                types: unique,
                span,
            }),
        }
    }

    /// Structurally match a concrete `value` against an `infer`-bearing `pattern`,
    /// binding each `infer` variable to the corresponding part of `value`.
    /// Returns whether the shapes are compatible. `Array<T>`/`ReadonlyArray<T>`
    /// patterns match `T[]` values; generic applications match by receiver and
    /// arity; functions match parameter-wise and on the return type; tuples and
    /// object literals match structurally. A pattern subtree with no `infer`
    /// imposes the ordinary assignability constraint (`value <: pattern`).
    fn match_infer(
        value: &Ast,
        pattern: &Ast,
        ctx: &ResolveCtx,
        bindings: &mut HashMap<String, Vec<Ast>>,
    ) -> bool {
        match pattern {
            Ast::Infer(inner) => {
                if let Ast::Ident(Ident { name, .. }) = &**inner {
                    bindings
                        .entry(name.clone())
                        .or_default()
                        .push(value.clone());
                    true
                } else {
                    false
                }
            }

            Ast::Array(p_el) => match value {
                Ast::Array(v_el) => Self::match_infer(v_el, p_el, ctx, bindings),
                _ => false,
            },

            Ast::ApplyGeneric(ApplyGeneric { receiver, args, .. }) => {
                // `Array<X>` / `ReadonlyArray<X>` is the generic spelling of `X[]`.
                if let Ast::Ident(Ident { name, .. }) = &**receiver {
                    if (name == "Array" || name == "ReadonlyArray") && args.len() == 1 {
                        if let Ast::Array(v_el) = value {
                            return Self::match_infer(v_el, &args[0], ctx, bindings);
                        }
                    }
                }
                // Otherwise match against another application of the same generic.
                if let Ast::ApplyGeneric(ApplyGeneric {
                    receiver: v_recv,
                    args: v_args,
                    ..
                }) = value
                {
                    if fingerprint(receiver) == fingerprint(v_recv) && args.len() == v_args.len() {
                        return args
                            .iter()
                            .zip(v_args.iter())
                            .all(|(p, v)| Self::match_infer(v, p, ctx, bindings));
                    }
                }
                false
            }

            Ast::FunctionType(pattern_fn) => match value {
                Ast::FunctionType(value_fn) => {
                    if pattern_fn.params.len() != value_fn.params.len() {
                        return false;
                    }
                    for (pp, vp) in pattern_fn.params.iter().zip(value_fn.params.iter()) {
                        if !Self::match_infer(&vp.kind, &pp.kind, ctx, bindings) {
                            return false;
                        }
                    }
                    Self::match_infer(
                        &value_fn.return_type,
                        &pattern_fn.return_type,
                        ctx,
                        bindings,
                    )
                }
                _ => false,
            },

            Ast::Tuple(pattern_tuple) => match value {
                Ast::Tuple(value_tuple) => {
                    if pattern_tuple.items.len() != value_tuple.items.len() {
                        return false;
                    }
                    pattern_tuple
                        .items
                        .iter()
                        .zip(value_tuple.items.iter())
                        .all(|(p, v)| Self::match_infer(v, p, ctx, bindings))
                }
                _ => false,
            },

            Ast::TypeLiteral(pattern_lit) => match value {
                Ast::TypeLiteral(value_lit) => {
                    for pp in pattern_lit.iter() {
                        let PropertyName::LiteralPropertyName(key) = &pp.key else {
                            return false;
                        };
                        let matched = value_lit
                            .iter()
                            .find(|vp| matches!(&vp.key, PropertyName::LiteralPropertyName(vk) if vk == key));
                        match matched {
                            Some(vp) => {
                                if !Self::match_infer(&vp.value, &pp.value, ctx, bindings) {
                                    return false;
                                }
                            }
                            None => return false,
                        }
                    }
                    true
                }
                _ => false,
            },

            // A pattern subtree with no `infer` constrains the value structurally.
            _ => value.is_assignable_to_ctx(pattern, ctx) != ExtendsResult::False,
        }
    }

    /// One-step reduction of a homomorphic indexed-access body `O["k"]`: when `O`
    /// resolves to an object literal and `"k"` is one of its plain string keys,
    /// replace the access with that property's value. Any other body (a
    /// non-access, an unresolved object, a non-literal key, a missing key) is
    /// returned unchanged, to be related structurally (often `Both`) later.
    fn reduce_indexed_access(body: &Ast, ctx: &ResolveCtx) -> Ast {
        let Ast::Access(Access { lhs, rhs, .. }) = body else {
            return body.clone();
        };

        let Ast::TypeString(TypeString { ty: key_name, .. }) = rhs.as_ref() else {
            return body.clone();
        };

        let resolved = ctx
            .env()
            .and_then(|env| env.resolve_head(lhs))
            .unwrap_or_else(|| (**lhs).clone());
        let Ast::TypeLiteral(literal) = &resolved else {
            return body.clone();
        };

        for prop in literal.iter() {
            if let PropertyName::LiteralPropertyName(name) = &prop.key {
                if name == key_name {
                    return prop.value.clone();
                }
            }
        }

        body.clone()
    }

    /// Reduce a string-literal indexed access at a relation leaf to the named
    /// property's type. The object side is reduced innermost-first so nested
    /// `T["a"]["b"]` resolves, the (env-resolved) object must be an object
    /// literal / interface shape, and the key must be a plain string literal
    /// naming one of its properties. An optional property `x?: V` widens to
    /// `V | undefined`, mirroring tsc under `--strict` (no
    /// `exactOptionalPropertyTypes`).
    ///
    /// Returns `Some(ty)` only when the access fully resolves to a non-access
    /// type. Non-accesses, numeric/`keyof`/union keys, unresolved objects,
    /// missing keys, and self-/mutually-referential access cycles return `None`,
    /// leaving the operand for structural handling (typically `Both`). The
    /// `None`-on-unresolved-access contract guarantees the caller cannot loop:
    /// a reduced operand is always a non-access, so re-entry reduces to `None`.
    fn reduce_access_leaf(node: &Ast, ctx: &ResolveCtx) -> Option<Ast> {
        /// One reduction step of `O[K]`, resolving the object side
        /// innermost-first (bounded by `depth` against pathological nesting).
        fn one_step(node: &Ast, ctx: &ResolveCtx, depth: usize) -> Option<Ast> {
            if depth > 32 {
                return None;
            }
            let Ast::Access(Access { lhs, rhs, span, .. }) = node else {
                return None;
            };

            let obj = one_step(lhs, ctx, depth + 1).unwrap_or_else(|| (**lhs).clone());
            Ast::index_type(&obj, rhs, ctx, *span, depth + 1)
        }

        // Drive to a fixpoint: a resolved property value may itself be an access
        // chain. Stop on a non-access result, on lack of progress, or at the cap
        // (a cycle), returning `None` in the unresolved cases.
        let mut current = one_step(node, ctx, 0)?;
        let mut steps = 0;
        while matches!(current, Ast::Access(_)) {
            steps += 1;
            if steps > 32 {
                return None;
            }
            match one_step(&current, ctx, 0) {
                Some(next) if next != current => current = next,
                _ => return None,
            }
        }
        Some(current)
    }

    /// Reduce `self` toward a normal form **for display** (the `dbg!` macro):
    /// resolve named references against the environment and reduce
    /// conditionals, indexed access, and `keyof`, bottom-up, repeating to a
    /// fixpoint. The iteration cap bounds recursive aliases (`type Rec as
    /// { next: Rec }`), which would otherwise expand forever — a capped or
    /// unresolvable expression is returned as far as it got. Never fails.
    ///
    /// This is presentation-only: assignability continues to reduce lazily and
    /// is unaffected.
    pub fn normalize(&self, ctx: &ResolveCtx) -> Ast {
        const CAP: usize = 8;

        let mut current = self.clone();
        for _ in 0..CAP {
            let next = Self::normalize_step(&current, ctx);
            // Spans are ignored by `PartialEq`, so this is structural equality.
            if next == current {
                break;
            }
            current = next;
        }
        current
    }

    /// One bottom-up reduction pass: children first, then resolve/reduce the
    /// rebuilt node.
    fn normalize_step(node: &Ast, ctx: &ResolveCtx) -> Ast {
        // `ApplyGeneric`'s receiver names a generic definition, not an
        // independent expression: `Ast::map` would otherwise recurse into it
        // and `resolve_head` would try to instantiate it with zero arguments
        // (e.g. `Get` alone), corrupting the receiver before the application
        // as a whole gets a chance to resolve. Only the arguments are
        // reduced bottom-up; the receiver is left for `resolve_head` on the
        // rebuilt application node below.
        let node = if let Ast::ApplyGeneric(app) = node {
            let mut app = app.clone();
            app.args = app
                .args
                .iter()
                .map(|arg| Self::normalize_step(arg, ctx))
                .collect();
            Ast::ApplyGeneric(app)
        } else {
            node.map(|child| Self::normalize_step(child, ctx))
        };

        if let Some(resolved) = ctx.env().and_then(|env| env.resolve_head(&node)) {
            return resolved;
        }

        match &node {
            Ast::ExtendsExpr(conditional) => Self::reduce_conditional(conditional, ctx),
            Ast::Access(_) => Self::reduce_access_leaf(&node, ctx).unwrap_or_else(|| node.clone()),
            Ast::Builtin(Builtin {
                name: BuiltinKeyword::Keyof,
                argument,
                ..
            }) => Self::eval_keyof(argument, ctx).unwrap_or_else(|| node.clone()),
            _ => node,
        }
    }

    /// Reduce a single indexed access `obj[key]` to the referenced member type,
    /// or `None` when it cannot be resolved definitely (leaving the operand
    /// indeterminate). `obj` is resolved to its definition head before dispatch;
    /// `key` is likewise resolved so a named literal-union alias distributes.
    ///
    /// Handled object sides: object literals (string / numeric-name keys, with
    /// optional-property `| undefined` widening), tuples (numeric-literal keys,
    /// `"length"` → literal arity, `number` → element union), arrays incl.
    /// `readonly` (numeric keys and `number` → element, `"length"` → `number`),
    /// unions (distribute — all-or-nothing), and intersections (property types
    /// from whichever object-literal members carry the key, intersected). A
    /// union KEY distributes over the object (all-or-nothing). Anything else —
    /// notably a `keyof`-driven key or an unresolvable object — yields `None`.
    fn index_type(obj: &Ast, key: &Ast, ctx: &ResolveCtx, span: Span, depth: usize) -> Option<Ast> {
        if depth > 32 {
            return None;
        }

        // Read-only `dbg!` observation hook: the object side and the key as
        // received, before either is resolved.
        if let Some(sink) = ctx.env().and_then(|env| env.dbg()) {
            sink.observe(obj);
            sink.observe(key);
        }

        // Resolve a named key one step so a literal-union alias distributes.
        let key_resolved = ctx.env().and_then(|env| env.resolve_head(key));
        let key = key_resolved.as_ref().unwrap_or(key);

        // A `keyof` KEY (`T[keyof T]`, `T[keyof U]`) reduces to its literal-key
        // union first, then re-enters — distributing through the union-key path
        // below. All-or-nothing: if the keyof does not reduce, the whole access
        // stays unreduced.
        if let Ast::Builtin(Builtin {
            name: BuiltinKeyword::Keyof,
            argument,
            ..
        }) = key
        {
            let reduced = Self::eval_keyof(argument, ctx)?;
            return Self::index_type(obj, &reduced, ctx, span, depth + 1);
        }

        // A union KEY distributes over the object: `T["a" | "b"]` →
        // `T["a"] | T["b"]`. All-or-nothing: every branch must itself reduce.
        if let Ast::UnionType(UnionType { types, .. }) = key {
            let mut parts = Vec::with_capacity(types.len());
            for k in types {
                parts.push(Self::index_type(obj, k, ctx, span, depth + 1)?);
            }
            return Some(Self::union_of(parts, span));
        }

        let resolved = ctx
            .env()
            .and_then(|env| env.resolve_head(obj))
            .unwrap_or_else(|| obj.clone());

        match &resolved {
            Ast::TypeLiteral(tl) => Self::index_object_literal(tl, key, span),
            Ast::Tuple(tuple) => Self::index_tuple(tuple, key, span),
            Ast::Array(element) => Self::index_array(element, key, span),
            // A `readonly` array/tuple indexes identically for reads.
            Ast::Readonly(inner) => Self::index_type(inner, key, ctx, span, depth + 1),
            // A union object side distributes: `(X | Y)["k"]` →
            // `X["k"] | Y["k"]`, all-or-nothing (a member lacking the key is a
            // type error in TS, so the whole access stays unreduced).
            Ast::UnionType(UnionType { types, .. }) => {
                let mut parts = Vec::with_capacity(types.len());
                for member in types {
                    parts.push(Self::index_type(member, key, ctx, span, depth + 1)?);
                }
                Some(Self::union_of(parts, span))
            }
            // An intersection object side carries the union of its members'
            // properties: gather the property type from every object-literal
            // member that carries the key and intersect them. If no member
            // carries it, the access is unreduced; if any member is not a
            // resolvable object literal, bail (we cannot be sure).
            Ast::IntersectionType(IntersectionType { types, .. }) => {
                let flat = Self::flatten_intersection_members(types);
                let mut parts = vec![];
                for member in &flat {
                    let member = ctx
                        .env()
                        .and_then(|env| env.resolve_head(member))
                        .unwrap_or_else(|| member.clone());
                    let Ast::TypeLiteral(tl) = &member else {
                        return None;
                    };
                    if let Some(value) = Self::index_object_literal(tl, key, span) {
                        parts.push(value);
                    }
                }
                match parts.len() {
                    0 => None,
                    1 => Some(parts.into_iter().next().unwrap()),
                    _ => Some(Ast::IntersectionType(IntersectionType {
                        types: parts,
                        span,
                    })),
                }
            }
            _ => None,
        }
    }

    /// Look up `key` on an object literal `tl`, returning the property's type
    /// (widened to `V | undefined` for an optional property under --strict
    /// without `exactOptionalPropertyTypes`). `key` must be a string or
    /// numeric literal naming a plain property; a missing or non-literal key
    /// yields `None`.
    fn index_object_literal(tl: &TypeLiteral, key: &Ast, span: Span) -> Option<Ast> {
        let key_name = match key {
            Ast::TypeString(TypeString { ty, .. }) => ty.clone(),
            Ast::TypeNumber(TypeNumber { ty, .. }) => ty.clone(),
            _ => return None,
        };
        for prop in tl.iter() {
            if let PropertyName::LiteralPropertyName(name) = &prop.key {
                if *name == key_name {
                    if prop.optional {
                        return Some(Ast::UnionType(UnionType {
                            types: vec![
                                prop.value.clone(),
                                Ast::Primitive(PrimitiveType::Undefined, span),
                            ],
                            span,
                        }));
                    }
                    return Some(prop.value.clone());
                }
            }
        }
        None
    }

    /// Look up `key` on a tuple `[A, B, …]`: a numeric-literal key selects the
    /// element at that in-bounds index, `"length"` yields the literal arity, and
    /// the `number` primitive yields the union of all element types (`never` for
    /// the empty tuple). Anything else yields `None`.
    fn index_tuple(tuple: &Tuple, key: &Ast, span: Span) -> Option<Ast> {
        match key {
            Ast::TypeNumber(TypeNumber { ty, .. }) => {
                let index = Self::number_value(ty)?;
                if index.fract() != 0.0 || index < 0.0 {
                    return None;
                }
                tuple.items.get(index as usize).cloned()
            }
            Ast::TypeString(TypeString { ty, .. }) if ty == "length" => {
                Some(Ast::TypeNumber(TypeNumber {
                    ty: tuple.items.len().to_string(),
                    span,
                }))
            }
            Ast::Primitive(PrimitiveType::Number, _) => {
                Some(Self::union_of(tuple.items.clone(), span))
            }
            _ => None,
        }
    }

    /// Look up `key` on an array `E[]`: a numeric-literal key or the `number`
    /// primitive yields the element type `E`, and `"length"` yields `number`.
    /// Anything else yields `None`.
    fn index_array(element: &Ast, key: &Ast, span: Span) -> Option<Ast> {
        match key {
            Ast::TypeNumber(_) | Ast::Primitive(PrimitiveType::Number, _) => Some(element.clone()),
            Ast::TypeString(TypeString { ty, .. }) if ty == "length" => {
                Some(Ast::Primitive(PrimitiveType::Number, span))
            }
            _ => None,
        }
    }
}
