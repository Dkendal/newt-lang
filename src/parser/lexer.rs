//! The newtype lexer: source text → `(Token, SimpleSpan)` stream.
//!
//! Faithful to the old pest grammar's atomic rules:
//!
//! * **Reserved words** lex as [`Token::Kw`]; contextual keywords (`do`,
//!   `match`, `cond`, `where`, `defaults`, `from`, `and`, `or`) lex as plain
//!   [`Token::Ident`]s and are matched by text in the parser, so `type and do 1
//!   end` stays legal.
//! * `[]` with **no gap** is a single [`Token::BracketPair`] (the array postfix
//!   and the empty tuple); `[ ]` with a gap lexes as two brackets.
//! * A macro identifier includes its trailing `!` (`dbg!`) — but `A != B` and
//!   `A !== B` lex as ident + operator because the char after `!` is `=`.
//! * Numbers keep their **raw source text**, including an optional `-` that may
//!   be separated from the digits by whitespace (`- 5`), and `_` separators.
//! * Strings have **no escape sequences**: a backslash is an ordinary character
//!   and the literal ends at the first matching quote (it may span newlines).
//!   The raw text, including the quotes, is kept. Template strings are one
//!   opaque backticked token.
//! * `//` line comments and non-nesting `/* */` block comments are skipped, as
//!   is whitespace (space, tab, newline; `\r` is also accepted — a conscious
//!   normalization of the old grammar, which rejected it).

use chumsky::prelude::*;
use std::fmt;

pub type Span = SimpleSpan<usize>;

/// A reserved word. Everything here is unusable as an identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kw {
    As,
    Assert,
    Class,
    Const,
    Export,
    Extends,
    False,
    For,
    Map,
    Function,
    If,
    Import,
    In,
    Infer,
    Interface,
    Keyof,
    Let,
    Optional,
    Readonly,
    Never,
    Not,
    Then,
    True,
    Type,
    Unittest,
    Unique,
    Else,
    End,
    // Primitive type keywords.
    String,
    Boolean,
    Number,
    Object,
    Bigint,
    Symbol,
    Void,
    Null,
    Undefined,
    // Top types.
    Any,
    Unknown,
}

impl Kw {
    /// The keyword for an identifier-shaped word, if it is reserved.
    pub fn from_str(word: &str) -> Option<Kw> {
        use Kw::*;
        Some(match word {
            "as" => As,
            "assert" => Assert,
            "class" => Class,
            "const" => Const,
            "export" => Export,
            "extends" => Extends,
            "false" => False,
            "for" => For,
            "map" => Map,
            "function" => Function,
            "if" => If,
            "import" => Import,
            "in" => In,
            "infer" => Infer,
            "interface" => Interface,
            "keyof" => Keyof,
            "let" => Let,
            "optional" => Optional,
            "readonly" => Readonly,
            "never" => Never,
            "not" => Not,
            "then" => Then,
            "true" => True,
            "type" => Type,
            "unittest" => Unittest,
            "unique" => Unique,
            "else" => Else,
            "end" => End,
            "string" => String,
            "boolean" => Boolean,
            "number" => Number,
            "object" => Object,
            "bigint" => Bigint,
            "symbol" => Symbol,
            "void" => Void,
            "null" => Null,
            "undefined" => Undefined,
            "any" => Any,
            "unknown" => Unknown,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        use Kw::*;
        match self {
            As => "as",
            Assert => "assert",
            Class => "class",
            Const => "const",
            Export => "export",
            Extends => "extends",
            False => "false",
            For => "for",
            Map => "map",
            Function => "function",
            If => "if",
            Import => "import",
            In => "in",
            Infer => "infer",
            Interface => "interface",
            Keyof => "keyof",
            Let => "let",
            Optional => "optional",
            Readonly => "readonly",
            Never => "never",
            Not => "not",
            Then => "then",
            True => "true",
            Type => "type",
            Unittest => "unittest",
            Unique => "unique",
            Else => "else",
            End => "end",
            String => "string",
            Boolean => "boolean",
            Number => "number",
            Object => "object",
            Bigint => "bigint",
            Symbol => "symbol",
            Void => "void",
            Null => "null",
            Undefined => "undefined",
            Any => "any",
            Unknown => "unknown",
        }
    }
}

impl fmt::Display for Kw {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Kw(Kw),
    Ident(String),
    /// An identifier immediately followed by `!`, e.g. `dbg!`. The `!` is part
    /// of the token text.
    MacroIdent(String),
    /// A number literal, kept as raw source text (optional `-` — possibly with
    /// a whitespace gap —, `_` separators, optional fraction).
    Number(String),
    /// A string literal, raw text **including** the surrounding quotes.
    Str(String),
    /// A template string, raw text **including** the surrounding backticks.
    TemplateStr(String),

    /// `[]` with no gap: array postfix / empty tuple.
    BracketPair,
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    /// `|`
    Union,
    /// `&`
    Intersection,
    /// `|>`
    PipeOp,
    /// `::`
    Colon2,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `...`
    Ellipsis,
    /// `?`
    Question,
    /// `-optional` (mapped-type optionality removal)
    MinusOptional,
    /// `-readonly` (mapped-type readonly removal)
    MinusReadonly,
    /// `,`
    Comma,
    /// `*`
    Star,
    /// `->`
    Arrow,
    /// `=>`
    FatArrow,
    /// `=`
    Eq,
    /// `==`
    EqEq,
    /// `!=`
    Neq,
    /// `!==`
    NeqStrict,
    /// `<:`
    Extends,
    /// `</:`
    NotExtends,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Kw(k) => write!(f, "{k}"),
            Token::Ident(s)
            | Token::MacroIdent(s)
            | Token::Number(s)
            | Token::Str(s)
            | Token::TemplateStr(s) => f.write_str(s),
            Token::BracketPair => f.write_str("[]"),
            Token::LParen => f.write_str("("),
            Token::RParen => f.write_str(")"),
            Token::LBracket => f.write_str("["),
            Token::RBracket => f.write_str("]"),
            Token::LBrace => f.write_str("{"),
            Token::RBrace => f.write_str("}"),
            Token::Union => f.write_str("|"),
            Token::Intersection => f.write_str("&"),
            Token::PipeOp => f.write_str("|>"),
            Token::Colon2 => f.write_str("::"),
            Token::Colon => f.write_str(":"),
            Token::Dot => f.write_str("."),
            Token::Ellipsis => f.write_str("..."),
            Token::Question => f.write_str("?"),
            Token::MinusOptional => f.write_str("-optional"),
            Token::MinusReadonly => f.write_str("-readonly"),
            Token::Comma => f.write_str(","),
            Token::Star => f.write_str("*"),
            Token::Arrow => f.write_str("->"),
            Token::FatArrow => f.write_str("=>"),
            Token::Eq => f.write_str("="),
            Token::EqEq => f.write_str("=="),
            Token::Neq => f.write_str("!="),
            Token::NeqStrict => f.write_str("!=="),
            Token::Extends => f.write_str("<:"),
            Token::NotExtends => f.write_str("</:"),
        }
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '$' || c == '_'
}

fn is_ident_start(c: char) -> bool {
    is_ident_char(c) && !c.is_ascii_digit()
}

/// Lex `src` into a spanned token stream. Spans are byte offsets into `src`.
pub fn lex(src: &str) -> (Option<Vec<(Token, Span)>>, Vec<Rich<'_, char, Span>>) {
    lexer().parse(src).into_output_errors()
}

fn lexer<'src>(
) -> impl Parser<'src, &'src str, Vec<(Token, Span)>, extra::Err<Rich<'src, char, Span>>> {
    // An identifier-shaped word: [A-Za-z$_][A-Za-z0-9$_]*. Reserved words
    // become `Kw` by maximal munch (`iffy`, `types`, `stringify` stay idents).
    let word = any()
        .filter(|c: &char| is_ident_start(*c))
        .then(any().filter(|c: &char| is_ident_char(*c)).repeated())
        .to_slice();

    // Macro ident: a *non-reserved* word immediately followed by `!`, where the
    // char after the `!` is not `=` (so `A != B` / `A !== B` lex as operators).
    let macro_ident = word
        .filter(|w: &&str| Kw::from_str(w).is_none())
        .then(just('!'))
        .then_ignore(just('=').not().rewind())
        .to_slice()
        .map(|s: &str| Token::MacroIdent(s.to_string()));

    let ident_or_kw = word.map(|w: &str| match Kw::from_str(w) {
        Some(kw) => Token::Kw(kw),
        None => Token::Ident(w.to_string()),
    });

    // number = (`-` whitespace*)? digits (`.` fraction?)?, kept as raw text.
    // The integer part may not start with `_`; the fraction may be empty (`1.`
    // is valid) but may not start with `_` (matching the pest grammar, where
    // `1._2` lexes as `1` `.` `_2`).
    let digits = any()
        .filter(|c: &char| c.is_ascii_digit())
        .then(
            any()
                .filter(|c: &char| c.is_ascii_digit() || *c == '_')
                .repeated(),
        )
        .ignored();
    let fraction = just('.')
        .then_ignore(just('_').not().rewind())
        .then(
            any()
                .filter(|c: &char| c.is_ascii_digit() || *c == '_')
                .repeated(),
        )
        .ignored();
    // A trailing `n` marks a bigint literal (`1n`); the raw text keeps it.
    let number = just('-')
        .then(one_of(" \t\n\r").repeated())
        .or_not()
        .then(digits)
        .then(fraction.or_not())
        .then(just('n').or_not())
        .to_slice()
        .map(|s: &str| Token::Number(s.to_string()));

    // Strings: no escapes; end at the first matching quote; may span newlines.
    let dq_string = none_of('"')
        .repeated()
        .delimited_by(just('"'), just('"'))
        .to_slice()
        .map(|s: &str| Token::Str(s.to_string()));
    let sq_string = none_of('\'')
        .repeated()
        .delimited_by(just('\''), just('\''))
        .to_slice()
        .map(|s: &str| Token::Str(s.to_string()));
    let template = none_of('`')
        .repeated()
        .delimited_by(just('`'), just('`'))
        .to_slice()
        .map(|s: &str| Token::TemplateStr(s.to_string()));

    // Operators and punctuation, longest first where one is a prefix of
    // another (`!==` before `!=`, `==`/`=>` before `=`, `</:` before `<:`,
    // `|>` before `|`, `::` before `:`, `...` before `.`, `->`, `[]` before `[`).
    let op = choice((
        just("!==").to(Token::NeqStrict),
        just("!=").to(Token::Neq),
        just("==").to(Token::EqEq),
        just("=>").to(Token::FatArrow),
        just("=").to(Token::Eq),
        just("</:").to(Token::NotExtends),
        just("<:").to(Token::Extends),
        just("|>").to(Token::PipeOp),
        just("|").to(Token::Union),
        just("&").to(Token::Intersection),
        just("::").to(Token::Colon2),
        just(":").to(Token::Colon),
        just("->").to(Token::Arrow),
        just("...").to(Token::Ellipsis),
        just(".").to(Token::Dot),
        just("-readonly")
            .to(Token::MinusReadonly)
            .or(just("-optional").to(Token::MinusOptional)),
        just("?").to(Token::Question),
        just(",").to(Token::Comma),
        just("*").to(Token::Star),
        just("[]").to(Token::BracketPair),
        just("[").to(Token::LBracket),
        just("]").to(Token::RBracket),
        just("(").to(Token::LParen),
        just(")").to(Token::RParen),
        just("{").to(Token::LBrace),
        just("}").to(Token::RBrace),
    ));

    // `->` must be tried before `number` would commit to a bare `-`; `number`
    // itself backtracks off `->` because no digit follows, so ordering `number`
    // first is safe — but a bare `-` (no digits, no `>`) is a lex error.
    let token = choice((
        number,
        macro_ident,
        ident_or_kw,
        dq_string,
        sq_string,
        template,
        op,
    ));

    let line_comment = just("//").then(none_of("\r\n").repeated()).ignored();
    let block_comment = just("/*")
        .then(any().and_is(just("*/").not()).repeated())
        .then(just("*/"))
        .ignored();
    let trivia = choice((one_of(" \t\n\r").ignored(), line_comment, block_comment)).repeated();

    trivia
        .clone()
        .ignore_then(
            token
                .map_with(|tok, e| (tok, e.span()))
                .then_ignore(trivia)
                .repeated()
                .collect(),
        )
        .then_ignore(end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Token> {
        let (tokens, errs) = lex(src);
        assert!(errs.is_empty(), "lex errors for {src:?}: {errs:?}");
        tokens.unwrap().into_iter().map(|(t, _)| t).collect()
    }

    #[test]
    fn bracket_pair_vs_brackets() {
        assert_eq!(
            toks("A[]"),
            vec![Token::Ident("A".into()), Token::BracketPair]
        );
        assert_eq!(
            toks("A[ ]"),
            vec![Token::Ident("A".into()), Token::LBracket, Token::RBracket]
        );
    }

    #[test]
    fn macro_ident_vs_neq() {
        assert_eq!(toks("dbg!"), vec![Token::MacroIdent("dbg!".into())]);
        assert_eq!(
            toks("A != B"),
            vec![
                Token::Ident("A".into()),
                Token::Neq,
                Token::Ident("B".into())
            ]
        );
        assert_eq!(
            toks("A !== B"),
            vec![
                Token::Ident("A".into()),
                Token::NeqStrict,
                Token::Ident("B".into())
            ]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(toks("-1"), vec![Token::Number("-1".into())]);
        assert_eq!(toks("- 5"), vec![Token::Number("- 5".into())]);
        assert_eq!(toks("1_000.5"), vec![Token::Number("1_000.5".into())]);
        assert_eq!(toks("1."), vec![Token::Number("1.".into())]);
        // The fraction may not start with `_`: `1._2` is `1` `.` `_2`.
        assert_eq!(
            toks("1._2"),
            vec![
                Token::Number("1".into()),
                Token::Dot,
                Token::Ident("_2".into())
            ]
        );
    }

    #[test]
    fn soft_keywords_are_idents() {
        for w in [
            "do", "match", "cond", "where", "defaults", "from", "and", "or",
        ] {
            assert_eq!(toks(w), vec![Token::Ident(w.into())], "{w}");
        }
        assert_eq!(toks("let"), vec![Token::Kw(Kw::Let)]);
        assert_eq!(toks("letx"), vec![Token::Ident("letx".into())]);
    }

    #[test]
    fn string_backslash_is_ordinary() {
        // The string ends at the first `"`; the backslash does not escape it.
        let ts = toks(r#""a\" x"#);
        assert_eq!(ts[0], Token::Str(r#""a\""#.into()));
    }

    #[test]
    fn comments_are_skipped() {
        assert_eq!(
            toks("a // line\n /* block\n */ b"),
            vec![Token::Ident("a".into()), Token::Ident("b".into())]
        );
    }

    #[test]
    fn arrow_is_not_a_number() {
        assert_eq!(toks("->"), vec![Token::Arrow]);
    }

    #[test]
    fn bare_minus_is_an_error() {
        let (_, errs) = lex("a - b");
        assert!(!errs.is_empty());
    }
}
