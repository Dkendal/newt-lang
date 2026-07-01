use newtype::ast::Ast;
use newtype::parser::{parse_source, Rule};

#[macro_use]
mod common;

#[test]
fn parse_expr_sexp_apply() {
    let actual = parse_source(Rule::expr, "Equals(T, any)").unwrap();
    insta::assert_snapshot!(actual.to_sexp().unwrap());
}

#[test]
fn parse_expr_sexp_apply_with_path() {
    let actual = parse_source(Rule::expr, "A::Equals(T, any)").unwrap();
    insta::assert_snapshot!(actual.to_sexp().unwrap());
}

#[test]
fn parses_to_ident() {
    let actual = parse_source(Rule::expr, "x").unwrap();
    assert!(matches!(&actual, Ast::Ident(id) if id.name == "x"));
    let span = actual.as_span();
    assert_eq!((span.start, span.end), (0, 1));
}

#[test]
fn fails_with_else() {
    // `else` is a reserved word, not an identifier.
    let errors = parse_source(Rule::expr, "else").unwrap_err();
    assert!(!errors.is_empty());
    assert_eq!(errors[0].span.start, 0);
}

fn parse_extends(input: &str) -> Ast {
    parse_source(Rule::extends_expr, input).unwrap()
}

#[test]
fn extends_expr_parser_extends() {
    insta::assert_snapshot!(parse_extends("A <: B").to_sexp().unwrap());
}

#[test]
fn extends_expr_parser_extends_parens() {
    insta::assert_snapshot!(parse_extends("(A <: B)").to_sexp().unwrap());
}

#[test]
fn extends_expr_parser_extends_multiple_parens() {
    insta::assert_snapshot!(parse_extends("((A <: B))").to_sexp().unwrap());
}

#[test]
fn extends_expr_parser_not_with_parens_extends() {
    insta::assert_snapshot!(parse_extends("not (A <: B)").to_sexp().unwrap());
}

#[test]
fn extends_expr_parser_and() {
    insta::assert_snapshot!(parse_extends("A <: B and C <: D").to_sexp().unwrap());
}

#[test]
fn extends_expr_parser_not_and_left() {
    insta::assert_snapshot!(parse_extends("not (A <: B) and C <: D").to_sexp().unwrap());
}

#[test]
fn extends_expr_parser_not_and_right() {
    insta::assert_snapshot!(parse_extends("A <: B and (not (C <: D))")
        .to_sexp()
        .unwrap());
}

#[test]
fn extends_expr_parser_not_and_both() {
    insta::assert_snapshot!(parse_extends("not (A <: B) and (not (C <: D))")
        .to_sexp()
        .unwrap());
}

mod unittest_statement {
    use super::*;

    #[test]
    fn parses_assert_statements() {
        let actual = parse_source(
            Rule::program,
            r#"
            unittest "assignability" do
                assert string <: unknown
                assert not (number <: string)
            end
            "#,
        )
        .unwrap();

        insta::assert_snapshot!(actual.to_sexp().unwrap());
    }
}

mod unquote {
    const R: Rule = Rule::expr;
    use super::*;

    #[test]
    fn parsing() {
        let actual = parse_source(R, "unquote!(1)").unwrap();
        insta::assert_snapshot!(actual.to_sexp().unwrap());
    }

    #[ignore]
    #[test]
    fn evaluates_expression() {
        assert_typescript!(
            R,
            "1",
            r#"
            unquote!(
                if 1 <: number then
                    1
                else
                    0
                end
            )
            "#
        );
    }
}
