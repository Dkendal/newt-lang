use crate::{ast::Ast, parser::parse_newtype_program};

macro_rules! ast {
    ($input:expr) => {{
        use crate::parser;

        parser::parse_source(parser::Rule::expr, $input).unwrap()
    }};
}

pub(crate) use ast;

macro_rules! sexpr {
    ($rule:expr, $input:expr) => {{
        use crate::parser;

        let ast = parser::parse_source($rule, $input).unwrap();

        serde_lexpr::to_value(ast)
    }};

    ($input:expr) => {
        sexpr!(crate::parser::Rule::expr, $input)
    };
}

pub(crate) use sexpr;

macro_rules! parse {
    ($rule:expr, $source:expr) => {{
        // Full-input consumption is inherent: the parser's entry points end
        // with `end()`, so trailing garbage is a parse error.
        crate::parser::parse_source($rule, $source).unwrap_or_else(|errors| {
            let rendered = errors
                .iter()
                .map(|e| e.render("<test>", $source))
                .collect::<Vec<_>>()
                .join("\n");
            panic!("{rendered}")
        })
    }};
    ($source:expr) => {
        parse!(crate::parser::Rule::program, $source)
    };
}

pub(crate) use parse;

macro_rules! assert_typescript {
    ($rule:expr, $expected:expr, $source:expr) => {
        let source = dedent!($source).trim();
        let expected = dedent!($expected).trim();

        let pairs = parse!($rule, source);

        let simplified = pairs.simplify();

        let actual = simplified.render_pretty_ts(80);

        pretty_assertions::assert_eq!(expected, actual.trim());
    };

    ($expected:expr, $source:expr) => {
        assert_typescript!(crate::parser::Rule::program, $expected, $source);
    };
}

pub(crate) use assert_typescript;

macro_rules! assert_parse_failure {
    ($source:expr) => {
        let node = crate::parser::parse_source(crate::parser::Rule::program, $source);
        println!("{:#?}", node);
        assert!(node.is_err());
    };
}
