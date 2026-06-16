// SPDX-License-Identifier: MIT

use logos::Logos;

// Raw lexemes straight from logos.
#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"[ \t]+")]
enum Lexeme {
    // newline followed by the line's leading spaces; the count drives indent/dedent
    #[regex(r"\n *", |lex| lex.slice().len() - 1)]
    NewlineIndent(usize),

    // Plain integer literal; non-negative scientific notation is allowed.
    // Examples: 1234  12E3
    #[regex("[0-9]+([eE][0-9]+)?")]
    // Integer literal with `_`-separated groups for readability. Groups must be
    // exactly three digits; scientific notation is not allowed here.
    // Matches: 12_000_321    Does not match: 12_11
    #[regex(r"[0-9]{1,3}(_[0-9]{3})+")]
    // Hex: 0x... with arbitrary `_` grouping.
    #[regex(r"0x[a-fA-F0-9]+(_[a-fA-F0-9]+)*")]
    // Binary: 0b... with arbitrary `_` grouping.
    #[regex(r"0b[01]+(_[01]+)*")]
    Int,

    // Floating point literal with optional `_`-separated digit groups in the
    // integer, fractional, and exponent parts. The integer part is optional, the
    // fractional part (with its leading `.`) is required.
    // Examples: .12  123.456  3.14E12  12_000_000.2_333_668E-1_000 (unrealistic).
    // Groups in the exponent are accepted despite making little sense. Logos has no
    // multiline/commented regex form, hence this one ugly line — see the match_float
    // test before touching it.
    #[regex(r"([0-9]{1,3}(_[0-9]{3})+|[0-9]+)?\.([0-9]{1,3}(_[0-9]{3})+|[0-9]+)([eE][+-]?([0-9]{1,3}(_[0-9]{3})+|[0-9]+))?")]
    Float,

    // Identifier: leading `_`s allowed, then a letter, then letters/digits/`_`.
    // Examples: a  Fu  _BAR  _baz_baz_  _fu12_11_bar
    #[regex(r"_*[a-zA-Z][a-zA-Z0-9_]*")]
    Identifier,

    // Compound-assignment operators. Each is a single contiguous token (longest-match
    // beats the bare operator). `div=`/`mod=` likewise beat the identifiers `div`/`mod`,
    // so a spaced `div =` is NOT compound assignment.
    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("^=")]
    CaretEq,
    #[token("div=")]
    DivEq,
    #[token("mod=")]
    ModEq,

    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("^")]
    Caret,
    #[token("=")]
    Equal,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("::")]
    ColonColon,
    #[token(":")]
    Colon,
    #[token(",")]
    Comma,
}

/// The kind of a lexical token the parser consumes. Beyond literals, identifiers, and operators,
/// this includes the `Kw*` keyword operators (`div`/`mod`/`is`) and synthetic layout tokens —
/// `Newline`, `Indent`, `Dedent`, and `Eof` — which carry no source text of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Int,
    Float,
    Identifier,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Equal,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    CaretEq,
    DivEq,
    ModEq,
    LParen,
    RParen,
    Colon,
    ColonColon,
    Comma,
    KwDiv,
    KwMod,
    KwIs,
    Newline,
    Indent,
    Dedent,
    Eof,
}

/// A lexical token: its [`TokenKind`], the `text` it spans (a slice of the source, empty for
/// synthetic layout tokens), and the 1-based `line`/`col` of its start for diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct Token<'src> {
    pub kind: TokenKind,
    pub text: &'src str,
    pub line: u32,
    pub col: u32,
}

/// A lexing failure: a [`message`](Self::message) plus the 1-based `line`/`col` where it occurred.
#[derive(Debug, PartialEq)]
pub struct LexError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

/// Lexes `src` into a flat `Vec` of [`Token`]s, turning indentation into `Indent`/`Dedent`
/// tokens and joining lines inside parentheses. Stops at and returns the first [`LexError`] —
/// an unexpected character or inconsistent indentation.
// Lex the whole source into a `Vec` of tokens up front, rather than handing the
// parser a lazy iterator. The parser is a Pratt parser that needs multi-token
// look-ahead (e.g. `Identifier` followed by `=` to tell an assignment from a bare
// expression), and logos' `Lexer` is forward-only with no peeking. Materialising
// also lets this pass do its one-to-many rewrites — a single newline can emit
// `Newline` plus a run of `Dedent`s, and EOF synthesises trailing `Dedent`s/`Eof`
// that have no source token.
pub fn lex(src: &str) -> Result<Vec<Token<'_>>, LexError> {
    let line_starts = line_starts(src);
    let locate = |offset: usize| -> (u32, u32) {
        let line = line_starts.partition_point(|&s| s <= offset) - 1;
        (line as u32 + 1, (offset - line_starts[line]) as u32 + 1)
    };

    let mut out = Vec::new();
    let mut indents = vec![0];
    let mut paren_depth: usize = 0;
    let mut lx = Lexeme::lexer(src);

    while let Some(res) = lx.next() {
        let token_text = lx.slice();
        let (line, col) = locate(lx.span().start);
        let lexeme = res.map_err(|_| LexError {
            message: format!("Unexpected character `{token_text}`."),
            line,
            col,
        })?;

        let kind = match lexeme {
            Lexeme::NewlineIndent(n) => {
                // Inside `( … )` a newline is implicit line-joining: emit nothing, so the
                // indent stack only ever reacts to real block indentation.
                if paren_depth == 0 {
                    out.push(synthetic(TokenKind::Newline, line, col));
                    emit_indent(&mut out, &mut indents, n, line, col)?;
                }
                continue;
            }
            Lexeme::Int => TokenKind::Int,
            Lexeme::Float => TokenKind::Float,
            Lexeme::Identifier => match token_text {
                "div" => TokenKind::KwDiv,
                "mod" => TokenKind::KwMod,
                "is" => TokenKind::KwIs,
                _ => TokenKind::Identifier,
            },
            Lexeme::PlusEq => TokenKind::PlusEq,
            Lexeme::MinusEq => TokenKind::MinusEq,
            Lexeme::StarEq => TokenKind::StarEq,
            Lexeme::SlashEq => TokenKind::SlashEq,
            Lexeme::CaretEq => TokenKind::CaretEq,
            Lexeme::DivEq => TokenKind::DivEq,
            Lexeme::ModEq => TokenKind::ModEq,
            Lexeme::Plus => TokenKind::Plus,
            Lexeme::Minus => TokenKind::Minus,
            Lexeme::Star => TokenKind::Star,
            Lexeme::Slash => TokenKind::Slash,
            Lexeme::Caret => TokenKind::Caret,
            Lexeme::Equal => TokenKind::Equal,
            Lexeme::LParen => {
                paren_depth += 1;
                TokenKind::LParen
            }
            Lexeme::RParen => {
                paren_depth = paren_depth.saturating_sub(1);
                TokenKind::RParen
            }
            Lexeme::ColonColon => TokenKind::ColonColon,
            Lexeme::Colon => TokenKind::Colon,
            Lexeme::Comma => TokenKind::Comma,
        };
        out.push(Token {
            kind,
            text: token_text,
            line,
            col,
        });
    }

    let (line, col) = locate(src.len());
    while indents.len() > 1 {
        indents.pop();
        out.push(synthetic(TokenKind::Dedent, line, col));
    }
    out.push(synthetic(TokenKind::Eof, line, col));
    Ok(out)
}

fn line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

// Build a token the lexer manufactures itself rather than matching from source:
// `Newline`, `Indent`, `Dedent`, `Eof`. These have no backing source slice, so `text`
// is empty — the empty `text` is what distinguishes them from lexed tokens.
fn synthetic(kind: TokenKind, line: u32, col: u32) -> Token<'static> {
    Token {
        kind,
        text: "",
        line,
        col,
    }
}

fn emit_indent(
    out: &mut Vec<Token<'_>>,
    indents: &mut Vec<usize>,
    n: usize,
    line: u32,
    col: u32,
) -> Result<(), LexError> {
    let top = *indents.last().unwrap();
    if n > top {
        indents.push(n);
        out.push(synthetic(TokenKind::Indent, line, col));
    } else if n < top {
        while n < *indents.last().unwrap() {
            indents.pop();
            out.push(synthetic(TokenKind::Dedent, line, col));
        }
        if *indents.last().unwrap() != n {
            return Err(LexError {
                message: "Inconsistent indentation.".into(),
                line,
                col,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn numeric_groups_and_radixes() {
        use TokenKind::*;
        assert_eq!(
            kinds("12_000_321 0xFF_a 0b1_010 16E10 3.14 1_000.000_000e-1_000 .5"),
            vec![Int, Int, Int, Int, Float, Float, Float, Eof]
        );
    }

    #[test]
    fn identifier_patterns() {
        let toks = lex("a Fu _BAR _baz_baz_ _fu12_11_bar").unwrap();
        let idents: Vec<_> = toks
            .iter()
            .filter(|t| t.kind == TokenKind::Identifier)
            .map(|t| t.text)
            .collect();
        assert_eq!(idents, ["a", "Fu", "_BAR", "_baz_baz_", "_fu12_11_bar"]);
    }

    #[test]
    fn keyword_classification() {
        use TokenKind::*;
        assert_eq!(
            kinds("10 div 4 mod is divmod"),
            vec![Int, KwDiv, Int, KwMod, KwIs, Identifier, Eof]
        );
    }

    #[test]
    fn punctuation_and_compound_assign() {
        use TokenKind::*;
        assert_eq!(
            kinds(":: : , += -= *= /= ^= div= mod="),
            vec![
                ColonColon, Colon, Comma, PlusEq, MinusEq, StarEq, SlashEq, CaretEq, DivEq, ModEq,
                Eof
            ]
        );
    }

    #[test]
    fn compound_div_requires_contiguity() {
        use TokenKind::*;
        // `div=` is one token; `div =` (spaced) is the keyword then `=`.
        assert_eq!(kinds("a div= b"), vec![Identifier, DivEq, Identifier, Eof]);
        assert_eq!(
            kinds("a div = b"),
            vec![Identifier, KwDiv, Equal, Identifier, Eof]
        );
    }

    #[test]
    fn newline_and_eof() {
        use TokenKind::*;
        let src = indoc! {"
            a = 1
            b = 2
        "};
        assert_eq!(
            kinds(src),
            vec![Identifier, Equal, Int, Newline, Identifier, Equal, Int, Newline, Eof]
        );
    }

    #[test]
    fn indent_dedent_from_leading_spaces() {
        use TokenKind::*;
        let src = indoc! {"
            a = 1
                b = 2
            c = 3
        "};
        assert_eq!(
            kinds(src),
            vec![
                Identifier, Equal, Int, Newline, Indent, // a = 1, then indented line
                Identifier, Equal, Int, Newline, Dedent, // b = 2, then back out
                Identifier, Equal, Int, Newline, // c = 3
                Eof
            ]
        );
    }

    #[test]
    fn inconsistent_dedent_is_error() {
        let src = indoc! {"
            a
                b
              c
        "};
        assert!(lex(src).is_err());
    }

    #[test]
    fn newlines_inside_parens_are_joined() {
        use TokenKind::*;
        // Multi-line parameter list: the newline between params emits no structural
        // tokens, so the indent stack only reacts to the body indent.
        let src = indoc! {"
            f(a: float,
              b: float): float is
                x
        "};
        assert_eq!(
            kinds(src),
            vec![
                Identifier, LParen, Identifier, Colon, Identifier, Comma, // f(a: float,
                Identifier, Colon, Identifier, RParen, Colon, Identifier, KwIs, // b: float): float is
                Newline, Indent, Identifier, Newline, Dedent, // body `x`
                Eof
            ]
        );
    }

    #[test]
    fn tracks_line_and_column() {
        let src = indoc! {"
            a = 1
              bb = 22
        "};
        let toks = lex(src).unwrap();
        let bb = toks.iter().find(|t| t.text == "bb").unwrap();
        assert_eq!((bb.line, bb.col), (2, 3));
        assert_eq!((toks[0].line, toks[0].col), (1, 1));
    }

    #[test]
    fn keeps_text_slice() {
        let toks = lex("g = 12_000_321").unwrap();
        assert_eq!(toks[0].text, "g");
        assert_eq!(toks[2].text, "12_000_321");
    }
}
