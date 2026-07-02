use newtype::extends_result::ExtendsResult;

#[macro_use]
mod common;

mod assignability_tests {
    use super::*;
    use rstest::rstest;

    static TRUE: ExtendsResult = ExtendsResult::True;
    static FALSE: ExtendsResult = ExtendsResult::False;
    static BOTH: ExtendsResult = ExtendsResult::Both;
    static NEVER: ExtendsResult = ExtendsResult::Never;

    #[rstest]
    // never
    #[case("never", "any", NEVER)]
    #[case("never", "unknown", NEVER)]
    #[case("never", "{}", NEVER)]
    #[case("never", "[]", NEVER)]
    #[case("never", "string", NEVER)]
    #[case("never", "number", NEVER)]
    #[case("never", "object", NEVER)]
    #[case("never", "boolean", NEVER)]
    #[case("never", "Object", NEVER)]
    #[case("never", "1", NEVER)]
    #[case("never", "'string'", NEVER)]
    #[case("never", "true", NEVER)]
    #[case("never", "false", NEVER)]
    #[case("never", "null", NEVER)]
    #[case("never", "undefined", NEVER)]
    #[case("never", "never", NEVER)]
    // any
    #[case("any", "any", TRUE)]
    #[case("any", "unknown", TRUE)]
    #[case("any", "{}", BOTH)]
    #[case("any", "[]", BOTH)]
    #[case("any", "string", BOTH)]
    #[case("any", "number", BOTH)]
    #[case("any", "boolean", BOTH)]
    #[case("any", "object", BOTH)]
    #[case("any", "Object", BOTH)]
    #[case("any", "1", BOTH)]
    #[case("any", "'string'", BOTH)]
    #[case("any", "true", BOTH)]
    #[case("any", "false", BOTH)]
    #[case("any", "null", BOTH)]
    #[case("any", "undefined", BOTH)]
    #[case("any", "never", BOTH)]
    // unknown
    #[case("unknown", "any", TRUE)]
    #[case("unknown", "unknown", TRUE)]
    #[case("unknown", "{}", FALSE)]
    #[case("unknown", "[]", FALSE)]
    #[case("unknown", "string", FALSE)]
    #[case("unknown", "number", FALSE)]
    #[case("unknown", "boolean", FALSE)]
    #[case("unknown", "object", FALSE)]
    #[case("unknown", "Object", FALSE)]
    #[case("unknown", "1", FALSE)]
    #[case("unknown", "'string'", FALSE)]
    #[case("unknown", "true", FALSE)]
    #[case("unknown", "false", FALSE)]
    #[case("unknown", "null", FALSE)]
    #[case("unknown", "undefined", FALSE)]
    #[case("unknown", "never", FALSE)]
    // {}
    #[case("{}", "any", TRUE)]
    #[case("{}", "unknown", TRUE)]
    #[case("{}", "{}", TRUE)]
    #[case("{}", "{ x: string }", FALSE)]
    #[case("{}", "[]", FALSE)]
    #[case("{}", "[{}]", FALSE)]
    #[case("{}", "{}[]", FALSE)]
    #[case("{}", "string", FALSE)]
    #[case("{}", "number", FALSE)]
    #[case("{}", "boolean", FALSE)]
    #[case("{}", "bitint", FALSE)]
    #[case("{}", "symbol", FALSE)]
    #[case("{}", "object", TRUE)]
    #[case("{}", "Object", TRUE)]
    #[case("{}", "Function", FALSE)]
    #[case("{}", "String", FALSE)]
    #[case("{}", "1", FALSE)]
    #[case("{}", "'string'", FALSE)]
    #[case("{}", "`string${var}`", FALSE)]
    #[case("{}", "true", FALSE)]
    #[case("{}", "false", FALSE)]
    #[case("{}", "null", FALSE)]
    #[case("{}", "undefined", FALSE)]
    #[case("{}", "never", FALSE)]
    // string
    #[case("string", "any", TRUE)]
    #[case("string", "unknown", TRUE)]
    #[case("string", "{}", TRUE)] // every non-nullish value is assignable to `{}`
    #[case("string", "[]", FALSE)]
    #[case("string", "string", TRUE)]
    #[case("string", "number", FALSE)]
    #[case("string", "boolean", FALSE)]
    #[case("string", "Object", TRUE)]
    #[case("string", "Function", FALSE)]
    #[case("string", "String", TRUE)]
    #[case("string", "1", FALSE)]
    #[case("string", "'string'", FALSE)]
    #[case("string", "true", FALSE)]
    #[case("string", "false", FALSE)]
    #[case("string", "null", FALSE)]
    #[case("string", "undefined", FALSE)]
    #[case("string", "never", FALSE)]
    // String
    #[case("String", "any", TRUE)]
    #[case("String", "unknown", TRUE)]
    #[case("String", "{}", TRUE)] // a wrapper is an object value
    #[case("String", "[]", FALSE)]
    #[case("String", "string", FALSE)] // wrapper is NOT assignable to its bare primitive
    #[case("String", "number", FALSE)]
    #[case("String", "boolean", FALSE)]
    #[case("String", "Object", TRUE)]
    #[case("String", "Function", FALSE)]
    #[case("String", "String", TRUE)]
    #[case("String", "1", FALSE)]
    #[case("String", "'string'", FALSE)]
    #[case("String", "true", FALSE)]
    #[case("String", "false", FALSE)]
    #[case("String", "null", FALSE)]
    #[case("String", "undefined", FALSE)]
    #[case("String", "never", FALSE)]
    // primitive/literal <: its wrapper = TRUE
    #[case("number", "Number", TRUE)]
    #[case("boolean", "Boolean", TRUE)]
    #[case("1", "Number", TRUE)]
    #[case("'x'", "String", TRUE)]
    #[case("true", "Boolean", TRUE)]
    #[case("false", "Boolean", TRUE)]
    // wrapper <: bare primitive = FALSE (unsound to allow)
    #[case("Number", "number", FALSE)]
    #[case("Boolean", "boolean", FALSE)]
    // wrapper <: same wrapper = TRUE
    #[case("Number", "Number", TRUE)]
    #[case("Boolean", "Boolean", TRUE)]
    // wrapper <: any-object target = TRUE (a wrapper is an object value)
    #[case("Number", "{}", TRUE)]
    #[case("Number", "object", TRUE)]
    #[case("Number", "Object", TRUE)]
    #[case("Boolean", "{}", TRUE)]
    #[case("Boolean", "object", TRUE)]
    #[case("Boolean", "Object", TRUE)]
    // cross-primitive wrappers = FALSE
    #[case("string", "Number", FALSE)]
    #[case("number", "String", FALSE)]
    #[case("boolean", "String", FALSE)]
    #[case("String", "Number", FALSE)]
    #[case("Number", "String", FALSE)]
    #[case("Number", "string", FALSE)]
    // nullish/void (--strict). `undefined` is assignable to `void`, but the
    // relation is one-directional and `null` is assignable to neither.
    #[case("undefined", "undefined", TRUE)]
    #[case("undefined", "void", TRUE)]
    #[case("undefined", "null", FALSE)]
    #[case("undefined", "unknown", TRUE)]
    #[case("undefined", "{}", FALSE)]
    #[case("undefined", "object", FALSE)]
    #[case("void", "void", TRUE)]
    #[case("void", "undefined", FALSE)]
    #[case("void", "null", FALSE)]
    #[case("void", "unknown", TRUE)]
    #[case("void", "{}", FALSE)]
    #[case("void", "object", FALSE)]
    #[case("null", "null", TRUE)]
    #[case("null", "undefined", FALSE)]
    #[case("null", "void", FALSE)]
    #[case("null", "unknown", TRUE)]
    #[case("null", "{}", FALSE)]
    #[case("null", "object", FALSE)]
    #[trace]
    fn is_assignable_to(#[case] a: &str, #[case] b: &str, #[case] expected: ExtendsResult) {
        assert_eq!(ast!(a).is_assignable_to(&ast!(b)), expected);
    }

    /// Structural assignability for the LHS variants that were previously
    /// `todo!()`: arrays, tuples, non-empty object literals, the opaque/
    /// unresolvable references (`Ident`, `Path`, `Access`, `ApplyGeneric`,
    /// `keyof`), and set operations (unions & intersections). Expected values
    /// mirror the TypeScript checker's assignable relation
    /// (`internal/checker/relater.go`) and the `assignmentCompat*` baselines,
    /// subject to this engine's existing conventions (e.g. primitives are not
    /// assignable to `{}`, but object/array/tuple values are).
    #[rstest]
    // --- Array LHS ---
    #[case("number[]", "number[]", TRUE)] // reflexive
    #[case("1[]", "number[]", TRUE)] // covariant element: 1 <: number
    #[case("number[]", "string[]", FALSE)]
    #[case("string[]", "number[]", FALSE)]
    #[case("never[]", "string[]", TRUE)] // never element is assignable
    #[case("string[]", "never[]", FALSE)]
    #[case("number[]", "object", TRUE)] // arrays are objects
    #[case("number[]", "{}", TRUE)]
    #[case("number[]", "Object", TRUE)]
    #[case("number[]", "[number]", FALSE)] // array is not a fixed-arity tuple
    #[case("number[]", "string", FALSE)]
    #[case("number[]", "number", FALSE)]
    #[case("number[][]", "number[][]", TRUE)] // nested arrays
    #[case("number[][]", "string[][]", FALSE)]
    // --- Tuple LHS ---
    #[case("[number, string]", "[number, string]", TRUE)] // reflexive
    #[case("[1, 'a']", "[number, string]", TRUE)] // element-wise widening
    #[case("[number]", "[string]", FALSE)]
    #[case("[number, string]", "[number]", FALSE)] // too long
    #[case("[number]", "[number, string]", FALSE)] // too short
    #[case("[number, string]", "[string, number]", FALSE)] // order matters
    #[case("[number, number]", "number[]", TRUE)] // tuple to array
    #[case("[1, 2]", "number[]", TRUE)]
    #[case("[number, string]", "number[]", FALSE)] // string not <: number
    #[case("[]", "number[]", TRUE)] // empty tuple to any array
    #[case("[]", "[]", TRUE)]
    #[case("[number]", "object", TRUE)]
    #[case("[number]", "{}", TRUE)]
    #[case("[number]", "Object", TRUE)]
    #[case("[number]", "string", FALSE)]
    // --- Non-empty TypeLiteral LHS ---
    #[case("{ x: 1 }", "{ x: number }", TRUE)]
    #[case("{ x: 1 }", "{ x: string }", FALSE)]
    #[case("{ x: 1 }", "{}", TRUE)]
    #[case("{ x: 1 }", "object", TRUE)]
    #[case("{ x: 1 }", "Object", TRUE)]
    #[case("{ x: 1 }", "{ x: 1, y: 2 }", FALSE)] // missing required property
    #[case("{ x: 1, y: 2 }", "{ x: number }", TRUE)] // width subtyping: extras allowed
    #[case("{ a: number }", "{ b: number }", FALSE)] // name mismatch
    #[case("{ x: number }", "{ x: 1 }", FALSE)] // base not assignable to literal
    #[case("{ x: { y: 1 } }", "{ x: { y: number } }", TRUE)] // depth
    #[case("{ x: { y: 1 } }", "{ x: { y: string } }", FALSE)]
    #[case("{ x: 1 }", "string", FALSE)]
    #[case("{ x: 1 }", "[number]", FALSE)]
    // empty source is assignable to an all-optional target
    #[case("{}", "{ x: number }", FALSE)]
    // still false: required prop missing
    // optional target/source via TypeScript postfix `?` syntax (`a?: T`)
    #[case("{ a: 1 }", "{ a?: number }", TRUE)] // required source → optional target
    #[case("{ a?: 1 }", "{ a: 1 }", FALSE)] // optional source → required target
    #[case("{}", "{ a?: 1 }", TRUE)] // empty source → all-optional target
    #[case("{ a?: 1 }", "{ a?: 1 }", TRUE)] // optional → optional ok
    #[case("{ readonly a?: 1 }", "{ a?: number }", TRUE)]
    // readonly + postfix optional
    // --- Index signatures (verified against tsgo --strict) ---
    // A source index signature does NOT supply named properties.
    #[case("{ [k in string]: number }", "{ a: number }", FALSE)]
    #[case("{ [k in number]: number }", "{ a: number }", FALSE)]
    // Named props must satisfy a target string index signature.
    #[case("{ a: number }", "{ [k in string]: number }", TRUE)]
    #[case("{ a: number, b: string }", "{ [k in string]: number }", FALSE)]
    #[case("{ a: 1 }", "{ [k in string]: number }", TRUE)] // covariant value
    #[case("{ a: number }", "{ [k in string]: 1 }", FALSE)]
    // Index-to-index value relation.
    #[case("{ [k in string]: number }", "{ [k in string]: number }", TRUE)]
    #[case("{ [k in string]: string }", "{ [k in string]: number }", FALSE)]
    #[case("{ [k in number]: number }", "{ [k in string]: number }", TRUE)] // number idx <: string idx
    #[case("{ [k in string]: number }", "{ [k in number]: number }", TRUE)]
    // string idx <: number idx
    // A number index constrains only numeric-named props; string keys are free.
    #[case("{ a: number, b: number }", "{ [k in number]: number }", TRUE)]
    // Source index + named: the named prop satisfies a named target.
    #[case("{ [k in string]: number, a: number }", "{ a: number }", TRUE)]
    #[case("{ [k in string]: number, a: number }", "{ b: number }", FALSE)]
    // An optional / any-object target is satisfied by an index signature.
    #[case("{ [k in string]: number }", "{ a?: number }", TRUE)]
    #[case("{ [k in string]: number }", "{}", TRUE)]
    #[case("{}", "{ [k in string]: number }", TRUE)]
    // A non-string/number index iterable stays indeterminate.
    #[case("{ [k in K]: number }", "{ a: number }", BOTH)]
    // --- Opaque / unresolvable references → indeterminate (Both) ---
    #[case("Foo", "Foo", TRUE)] // reflexive resolves first
    #[case("Foo", "any", TRUE)]
    #[case("Foo", "unknown", TRUE)]
    #[case("Foo", "never", FALSE)]
    #[case("Foo", "string", BOTH)]
    #[case("Foo", "object", BOTH)]
    #[case("Foo", "Object", BOTH)] // wrapper target does not preempt the free variable
    #[case("Foo", "{ x: 1 }", BOTH)]
    #[case("A::B", "A::B", TRUE)]
    #[case("A::B", "string", BOTH)]
    #[case("A[B]", "string", BOTH)] // indexed access
    #[case("A(B)", "string", BOTH)] // generic application
    #[case("keyof(A)", "string", BOTH)]
    // --- Union LHS / RHS ---
    #[case("1 | 2", "number", TRUE)] // every member <: number
    #[case("1 | 2", "1", FALSE)] // 2 is not <: 1
    #[case("'a' | 'b'", "string", TRUE)]
    #[case("1", "1 | 2", TRUE)] // assignable to some member
    #[case("3", "1 | 2", FALSE)]
    #[case("string", "string | number", TRUE)]
    #[case("string", "number | boolean", FALSE)]
    #[case("string | number", "number | string", TRUE)]
    #[case("1 | 2", "number | string", TRUE)]
    #[case("boolean", "true | false", TRUE)] // boolean is true | false
    // --- Intersection LHS / RHS ---
    #[case("{ one: number } & { two: string }", "{ one: number }", TRUE)] // some member
    #[case("{ one: number } & { two: string }", "{ two: string }", TRUE)]
    #[case(
        "{ one: number } & { two: string }",
        "{ one: number, two: string }",
        TRUE
    )] // merged shape
    #[case("{ one: number } & { two: string }", "{ three: boolean }", FALSE)]
    #[case("{ a: 1 } & { b: 2 }", "{ a: number, b: number } | string", TRUE)] // merged vs union
    #[case("string", "string & Object", TRUE)] // assignable to every member
    #[case("string", "string & number", FALSE)]
    #[case("never & string", "number", NEVER)]
    // never & T == never
    // Contradictory intersections of disjoint primitives are uninhabited and
    // reduce to `never` (the bottom type). TS treats `string & number`, etc.,
    // as `never`, so the LHS collapses regardless of the target.
    #[case("string & number", "never", NEVER)]
    #[case("string & number", "string", NEVER)]
    #[case("string & boolean", "string", NEVER)]
    #[case("1 & 2", "number", NEVER)] // two distinct disjoint literals
    #[case("true & false", "boolean", NEVER)]
    // ...but a literal and its widened primitive share a common subtype (the
    // literal), so the intersection is inhabited and must NOT reduce to never.
    #[case("1 & number", "number", TRUE)] // 1 & number == 1
    #[case("1 & number", "never", FALSE)]
    #[case("string & 'a'", "string", TRUE)] // string & "a" == "a"
    #[case("string & 'a'", "never", FALSE)]
    // Object members never participate in primitive contradiction.
    #[case("{ a: 1 } & { b: 2 }", "never", FALSE)]
    #[case("string & {}", "never", FALSE)] // string & {} == string
    // --- Function type LHS ---
    #[case("() => any", "() => any", TRUE)] // reflexive
    #[case("() => never", "() => any", TRUE)] // covariant return: never <: any
    #[case("() => string", "() => unknown", TRUE)] // covariant return widening
    #[case("() => unknown", "() => string", FALSE)] // return is not contravariant
    #[case("(unknown) => void", "(string) => void", TRUE)] // params contravariant
    #[case("(string) => void", "(unknown) => void", FALSE)]
    #[case("() => void", "(string) => void", TRUE)] // source may omit trailing params
    #[case("(string) => void", "() => void", FALSE)] // but not require more than target
    #[case("(string) => any", "(any) => any", TRUE)] // `any` param is compatible
    #[case("(string) => any", "(unknown) => any", FALSE)] // `unknown` param is not
    #[case("() => any", "Function", TRUE)] // functions are assignable to Function
    #[case("() => void", "{}", TRUE)] // functions are objects
    #[case("5", "Function", FALSE)]
    // non-functions are not
    // --- readonly arrays / tuples (one-directional: mutable <: readonly) ---
    #[case("string[]", "readonly string[]", TRUE)] // mutable -> readonly
    #[case("readonly string[]", "string[]", FALSE)] // readonly -> mutable
    #[case("readonly string[]", "readonly string[]", TRUE)] // reflexive
    #[case("readonly 1[]", "readonly number[]", TRUE)] // covariant element
    #[case("readonly string[]", "readonly number[]", FALSE)]
    #[case("[string, number]", "readonly [string, number]", TRUE)]
    #[case("readonly [string, number]", "[string, number]", FALSE)]
    #[case("readonly string[]", "object", TRUE)] // readonly arrays are objects
    #[case("readonly string[]", "unknown", TRUE)]
    // --- keyof of an object literal (evaluated to the union of its keys) ---
    #[case("keyof { a: string, b: number }", "'a' | 'b'", TRUE)] // source keyof
    #[case("'a'", "keyof { a: 1, b: 2 }", TRUE)] // target keyof
    #[case("keyof { a: 1 }", "'a'", TRUE)] // single-key keyof
    #[case("keyof { a: 1 }", "'b'", FALSE)] // wrong key
    #[case("keyof { a: string } | 'z'", "'a' | 'z'", TRUE)] // keyof inside a union
    #[case("'a' | 'b'", "keyof { a: 1, b: 2 }", TRUE)] // union into target keyof
    #[case("keyof { a: 1, b: 2 }", "'a'", FALSE)] // key union not assignable to one member
    #[case("keyof string[]", "string", BOTH)]
    // non-object arg stays indeterminate
    // --- Mapped types expanded to object literals (verified vs tsgo --strict) ---
    #[case(
        "map K in \"a\" | \"b\" do number end",
        "{ a: number, b: number }",
        TRUE
    )]
    #[case(
        "{ a: number, b: number }",
        "map K in \"a\" | \"b\" do number end",
        TRUE
    )]
    #[case("map K in \"a\" | \"b\" do number end", "{ a: number }", TRUE)] // width
    #[case("map K in \"a\" do string end", "{ a: number }", FALSE)] // value mismatch
    #[case("{}", "map K in never do number end", TRUE)] // empty key set -> {}
    #[case("map K in never do number end", "{}", TRUE)]
    #[case("map K in \"a\" do 1 end", "{ a: number }", TRUE)] // covariant value
    // An unknown key set leaves the relation indeterminate (sound over-approx).
    #[case("map K in string do number end", "{ a: number }", BOTH)] // primitive iterable
    #[case("map K in Foo do number end", "{ a: number }", BOTH)] // unresolved ref iterable
    #[trace]
    fn is_assignable_to_extended(
        #[case] a: &str,
        #[case] b: &str,
        #[case] expected: ExtendsResult,
    ) {
        assert_eq!(ast!(a).is_assignable_to(&ast!(b)), expected);
    }
}

mod simplify_tests {
    use newtype::parser::{self, Rule::expr};

    #[test]
    fn simplify_path_access() {
        let actual = parser::parse_source(parser::Rule::expr, "A::B::C::D")
            .unwrap()
            .simplify();
        insta::assert_snapshot!(actual.to_sexp().unwrap());
    }

    #[test]
    fn simplify_basic() {
        let actual = parse!(expr, "if a <: b then c else d end").simplify();
        insta::assert_snapshot!(actual.to_sexp().unwrap());
    }
}
