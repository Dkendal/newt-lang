#[macro_export]
macro_rules! ast {
    ($input:expr) => {{
        use newtype::parser;

        parser::parse_source(parser::Rule::expr, $input).unwrap()
    }};
}

#[macro_export]
macro_rules! parse {
    ($rule:expr, $source:expr) => {{
        // Full-input consumption is inherent: the parser's entry points end
        // with `end()`, so trailing garbage is a parse error.
        newtype::parser::parse_source($rule, $source).unwrap_or_else(|errors| {
            let rendered = errors
                .iter()
                .map(|e| e.render("<test>", $source))
                .collect::<Vec<_>>()
                .join("\n");
            panic!("{rendered}")
        })
    }};
    ($source:expr) => {
        parse!(newtype::parser::Rule::program, $source)
    };
}

#[macro_export]
macro_rules! assert_expr_eq {
    ($a:expr, $b:expr) => {{
        use newtype::parser::Rule::expr;
        pretty_assertions::assert_eq!(
            parse!(expr, $a).simplify().to_sexp().unwrap(),
            parse!(expr, $b).simplify().to_sexp().unwrap(),
        )
    }};
}

#[macro_export]
macro_rules! assert_typescript {
    ($rule:expr, $expected:expr, $source:expr) => {
        let source = ::textwrap_macros::dedent!($source).trim();
        let expected = ::textwrap_macros::dedent!($expected).trim();

        let pairs = parse!($rule, source);

        let simplified = pairs.simplify();

        let actual = <_ as newtype::typescript::Pretty>::render_pretty_ts(&simplified, 80);

        pretty_assertions::assert_eq!(expected, actual.trim());
    };

    ($expected:expr, $source:expr) => {
        assert_typescript!(newtype::parser::Rule::program, $expected, $source);
    };
}
