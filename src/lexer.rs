use logos::Logos;

// Raw lexemes straight from logos. The numeric/identifier regexes carry `_`-separated
// 3-digit grouping for decimals and arbitrary grouping for hex/binary.
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
}

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
    LParen,
    RParen,
    KwDiv,
    KwMod,
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token<'src> {
    pub kind: TokenKind,
    pub text: &'src str,
}

#[derive(Debug, PartialEq)]
pub struct LexError {
    pub text: String,
}

// Lex the whole source into a `Vec` of tokens up front, rather than handing the
// parser a lazy iterator. The parser is a Pratt parser that needs multi-token
// look-ahead (e.g. `Identifier` followed by `=` to tell an assignment from a bare
// expression), and logos' `Lexer` is forward-only with no peeking. Materialising
// also lets this pass do its one-to-many rewrites — a single newline can emit
// `Newline` plus a run of `Dedent`s, and EOF synthesises trailing `Dedent`s/`Eof`
// that have no source token.
pub fn lex(src: &str) -> Result<Vec<Token<'_>>, LexError> {
    let mut out = Vec::new();
    let mut indents = vec![0usize];
    let mut lx = Lexeme::lexer(src);

    while let Some(res) = lx.next() {
        let slice = lx.slice();
        let lexeme = res.map_err(|_| LexError {
            text: slice.to_string(),
        })?;

        let kind = match lexeme {
            Lexeme::NewlineIndent(n) => {
                out.push(tok(TokenKind::Newline));
                emit_indent(&mut out, &mut indents, n);
                continue;
            }
            Lexeme::Int => TokenKind::Int,
            Lexeme::Float => TokenKind::Float,
            Lexeme::Identifier => match slice {
                "div" => TokenKind::KwDiv,
                "mod" => TokenKind::KwMod,
                _ => TokenKind::Identifier,
            },
            Lexeme::Plus => TokenKind::Plus,
            Lexeme::Minus => TokenKind::Minus,
            Lexeme::Star => TokenKind::Star,
            Lexeme::Slash => TokenKind::Slash,
            Lexeme::Caret => TokenKind::Caret,
            Lexeme::Equal => TokenKind::Equal,
            Lexeme::LParen => TokenKind::LParen,
            Lexeme::RParen => TokenKind::RParen,
        };
        out.push(Token { kind, text: slice });
    }

    while indents.len() > 1 {
        indents.pop();
        out.push(tok(TokenKind::Dedent));
    }
    out.push(tok(TokenKind::Eof));
    Ok(out)
}

fn tok<'src>(kind: TokenKind) -> Token<'src> {
    Token { kind, text: "" }
}

fn emit_indent<'src>(out: &mut Vec<Token<'src>>, indents: &mut Vec<usize>, n: usize) {
    let top = *indents.last().unwrap();
    if n > top {
        indents.push(n);
        out.push(tok(TokenKind::Indent));
    } else {
        while n < *indents.last().unwrap() {
            indents.pop();
            out.push(tok(TokenKind::Dedent));
        }
    }
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
            kinds("10 div 4 mod divmod"),
            vec![Int, KwDiv, Int, KwMod, Identifier, Eof]
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
    fn keeps_text_slice() {
        let toks = lex("g = 12_000_321").unwrap();
        assert_eq!(toks[0].text, "g");
        assert_eq!(toks[2].text, "12_000_321");
    }
}
