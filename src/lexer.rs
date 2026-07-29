// SPDX-License-Identifier: MIT

use logos::Logos;

// Raw lexemes straight from logos.
#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"[ \t]+")]
// A comment runs from `#` to just before the newline, so the line boundary survives for the
// indentation logic; the comment text itself is discarded.
#[logos(skip("#[^\n]*", allow_greedy = true))]
enum Lexeme {
    // A newline followed by the line's leading spaces; the count drives indent/dedent. `\r\n` is a
    // line terminator too, so a file written on Windows lexes identically — a lone `\r` matches
    // nothing and stays an error rather than being guessed at.
    #[regex(r"\r?\n *", |lex| {
        let slice = lex.slice();
        slice.len() - slice.rfind('\n').expect("a newline lexeme contains its newline") - 1
    })]
    NewlineIndent(usize),

    // A tab in a line's leading whitespace. pinp's layout is space-based, and the general
    // whitespace skip would otherwise swallow this and leave the line looking unindented — so it is
    // matched here (longest-match beats `NewlineIndent`) purely to be reported as an error.
    #[regex(r"\r?\n *\t")]
    TabIndent,

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

    // A string literal between matching quotes: any run not containing its own delimiter. Single
    // and double quotes are interchangeable, and the body **may span physical lines** — a newline
    // between the delimiters is content, so it never reaches the indent/dedent logic. The accepted
    // cost is that an unterminated literal matches nothing until end of input; it still surfaces as
    // a lex error at the opening quote.
    #[regex(r"'[^']*'")]
    #[regex("\"[^\"]*\"")]
    Str,

    // An f-string (interpolation) literal: the `f` prefix immediately before a string body, which
    // may span lines just the same. Longest-match keeps a bare `f` not followed by a quote an
    // `Identifier`.
    #[regex(r"f'[^']*'")]
    #[regex("f\"[^\"]*\"")]
    FStr,

    // Identifier: optional leading `_`s, then a letter, then letters/digits/`_`.
    // Examples: a  Fu  _BAR  _baz_baz_  _fu12_11_bar
    #[regex(r"_*[a-zA-Z][a-zA-Z0-9_]*")]
    Identifier,
    // Bare `_` — the don't-care binder. Separate from `Identifier` so the parser can
    // accept it in binder positions and reject it elsewhere without special-casing strings.
    // Logos longest-match ensures `_foo` → `Identifier`, bare `_` → `Underscore`.
    #[token("_")]
    Underscore,

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

    // Comparison operators. The two-character forms are longest-match winners over the bare
    // `=`/`<`/`>` (and `!=` is the only use of `!`).
    #[token("==")]
    EqEq,
    #[token("!=")]
    Ne,
    #[token("<=")]
    Le,
    #[token(">=")]
    Ge,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,

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
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("::")]
    ColonColon,
    #[token(":")]
    Colon,
    #[token(",")]
    Comma,
    #[token(";")]
    Semicolon,

    // Range operators. `..` is the inclusive form; `..<`/`..>` exclude the stop bound (longest-match
    // beats `..`). `..` competes with the `Float` regex only where a digit follows the dot, so `.5`
    // stays a float while `..5` is `DotDot` then `Int`.
    #[token("..")]
    DotDot,
    #[token("..<")]
    DotDotLt,
    #[token("..>")]
    DotDotGt,

    // Single dot for member access (`.len`, etc.). Longest-match ensures `..` and float `.5`
    // are not disturbed: logos picks `..` over `.`, and the float regex requires a digit after `.`.
    #[token(".")]
    Dot,
}

/// The kind of a lexical token the parser consumes. Beyond literals, identifiers, and operators,
/// this includes the `Kw*` keyword operators (`div`/`mod`/`is`) and synthetic layout tokens —
/// `Newline`, `Indent`, `Dedent`, and `Eof` — which carry no source text of their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Int,
    Float,
    /// A string literal, delimiters included in `text` (the parser strips them).
    Str,
    /// An f-string literal (`f'…'`), prefix and delimiters included in `text`.
    FStr,
    Identifier,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,
    Equal,
    EqEq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    CaretEq,
    DivEq,
    ModEq,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Colon,
    ColonColon,
    Comma,
    Semicolon,
    Dot,
    DotDot,
    DotDotLt,
    DotDotGt,
    KwDiv,
    KwMod,
    KwIs,
    KwTrue,
    KwFalse,
    KwAnd,
    KwOr,
    KwXor,
    KwNot,
    KwIf,
    KwElif,
    KwElse,
    KwWhile,
    KwUntil,
    KwLoop,
    KwFor,
    KwIn,
    /// The bare `_` character: the don't-care binder in `for` loops.
    Underscore,
    Newline,
    Indent,
    Dedent,
    Eof,
}

/// A half-open byte range into the source.
///
/// Byte offsets rather than line/column: the parser copies them straight off a token with no
/// arithmetic, and [`LineIndex`] resolves them for display only when something is actually
/// reported. A span is what lets a diagnostic point at source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    /// The span of something whose position was never recorded. Analyses report it as "no
    /// position" rather than pointing at the start of the file.
    ///
    /// It is indistinguishable from a genuine empty span at offset 0, which no real token has.
    pub const UNKNOWN: Span = Span { start: 0, end: 0 };

    pub fn new(start: u32, end: u32) -> Span {
        Span { start, end }
    }

    pub fn is_unknown(&self) -> bool {
        self.start == 0 && self.end == 0
    }

    /// The source text this span covers.
    pub fn text<'src>(&self, src: &'src str) -> &'src str {
        &src[self.start as usize..self.end as usize]
    }
}

/// Line-start offsets for one source, so a byte offset resolves to a 1-based line and column.
///
/// Built once and queried many times: the lexer uses it to place errors, and the analysis layer to
/// render diagnostics.
pub struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(src: &str) -> LineIndex {
        let mut line_starts = vec![0];
        for (index, byte) in src.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(index as u32 + 1);
            }
        }
        LineIndex { line_starts }
    }

    /// The 1-based line and column containing `offset`. A column counts bytes, not characters.
    pub fn locate(&self, offset: u32) -> (u32, u32) {
        let line = self.line_starts.partition_point(|&start| start <= offset) - 1;
        (line as u32 + 1, offset - self.line_starts[line] + 1)
    }
}

/// A lexical token: its [`TokenKind`], the `text` it spans (a slice of the source, empty for
/// synthetic layout tokens), the byte offset it starts at, and the 1-based `line`/`col` of that
/// start for diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct Token<'src> {
    pub kind: TokenKind,
    pub text: &'src str,
    /// Byte offset of the token's first character. Synthetic layout tokens carry the offset of the
    /// position they were manufactured at.
    pub start: u32,
    pub line: u32,
    pub col: u32,
}

impl Token<'_> {
    /// The token's source range.
    pub fn span(&self) -> Span {
        Span::new(self.start, self.start + self.text.len() as u32)
    }
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
    let line_index = LineIndex::new(src);
    let locate = |offset: usize| -> (u32, u32) { line_index.locate(offset as u32) };

    let mut out = Vec::new();
    let mut indents = vec![0];
    let mut paren_depth: usize = 0;
    // A newline is not turned into tokens the moment it is seen; it waits here until a line with
    // real content follows. Blank and comment-only lines keep overwriting this record, so they
    // collapse away and only the newline before a content line survives — with that line's indent.
    let mut pending_newline: Option<(usize, u32, u32, u32)> = None;
    // Stays false until the first real token, so a content-free run at the start of the file —
    // blank or comment lines with no statement yet to terminate — leaves no leading separator.
    let mut emitted_content = false;
    let mut lexer = Lexeme::lexer(src);

    while let Some(lexeme_result) = lexer.next() {
        let token_text = lexer.slice();
        let (line, col) = locate(lexer.span().start);
        let lexeme = lexeme_result.map_err(|_| LexError {
            message: format!("Unexpected character `{token_text}`."),
            line,
            col,
        })?;

        let kind = match lexeme {
            Lexeme::NewlineIndent(indent) => {
                // Inside `( … )` a newline is implicit line-joining: record nothing, so the
                // indent stack only ever reacts to real block indentation.
                if paren_depth == 0 {
                    pending_newline = Some((indent, lexer.span().start as u32, line, col));
                }
                continue;
            }
            Lexeme::TabIndent => {
                // The lexeme spans the newline and any spaces before the tab; report the tab itself.
                let (tab_line, tab_col) = locate(lexer.span().end - 1);
                return Err(LexError {
                    message: "Tabs are not allowed in indentation.".into(),
                    line: tab_line,
                    col: tab_col,
                });
            }
            Lexeme::Int => TokenKind::Int,
            Lexeme::Float => TokenKind::Float,
            Lexeme::Str => TokenKind::Str,
            Lexeme::FStr => TokenKind::FStr,
            Lexeme::Identifier => match token_text {
                "div" => TokenKind::KwDiv,
                "mod" => TokenKind::KwMod,
                "is" => TokenKind::KwIs,
                "true" => TokenKind::KwTrue,
                "false" => TokenKind::KwFalse,
                "and" => TokenKind::KwAnd,
                "or" => TokenKind::KwOr,
                "xor" => TokenKind::KwXor,
                "not" => TokenKind::KwNot,
                "if" => TokenKind::KwIf,
                "elif" => TokenKind::KwElif,
                "else" => TokenKind::KwElse,
                "while" => TokenKind::KwWhile,
                "until" => TokenKind::KwUntil,
                "loop" => TokenKind::KwLoop,
                "for" => TokenKind::KwFor,
                "in" => TokenKind::KwIn,
                _ => TokenKind::Identifier,
            },
            Lexeme::Underscore => TokenKind::Underscore,
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
            Lexeme::EqEq => TokenKind::EqEq,
            Lexeme::Ne => TokenKind::Ne,
            Lexeme::Lt => TokenKind::Lt,
            Lexeme::Gt => TokenKind::Gt,
            Lexeme::Le => TokenKind::Le,
            Lexeme::Ge => TokenKind::Ge,
            Lexeme::LParen => {
                paren_depth += 1;
                TokenKind::LParen
            }
            Lexeme::RParen => {
                paren_depth = paren_depth.saturating_sub(1);
                TokenKind::RParen
            }
            Lexeme::LBracket => {
                paren_depth += 1;
                TokenKind::LBracket
            }
            Lexeme::RBracket => {
                paren_depth = paren_depth.saturating_sub(1);
                TokenKind::RBracket
            }
            Lexeme::ColonColon => TokenKind::ColonColon,
            Lexeme::Colon => TokenKind::Colon,
            Lexeme::Comma => TokenKind::Comma,
            Lexeme::Semicolon => TokenKind::Semicolon,
            Lexeme::Dot => TokenKind::Dot,
            Lexeme::DotDot => TokenKind::DotDot,
            Lexeme::DotDotLt => TokenKind::DotDotLt,
            Lexeme::DotDotGt => TokenKind::DotDotGt,
        };
        // A content token has arrived: realise the recorded newline as the boundary before it,
        // unless nothing has been emitted yet — a leading separator has nothing to separate.
        if emitted_content {
            flush_pending_newline(&mut out, &mut indents, &mut pending_newline)?;
        } else {
            pending_newline = None;
        }
        out.push(Token {
            kind,
            text: token_text,
            start: lexer.span().start as u32,
            line,
            col,
        });
        emitted_content = true;
    }

    // A newline still pending at end of input terminates the final statement, as a trailing
    // newline always did. (A file of only blank/comment lines emitted nothing, so there is none.)
    if emitted_content {
        flush_pending_newline(&mut out, &mut indents, &mut pending_newline)?;
    }
    let (line, col) = locate(src.len());
    let offset = src.len() as u32;
    while indents.len() > 1 {
        indents.pop();
        out.push(synthetic(TokenKind::Dedent, offset, line, col));
    }
    out.push(synthetic(TokenKind::Eof, offset, line, col));
    Ok(out)
}

// Build a token the lexer manufactures itself rather than matching from source:
// `Newline`, `Indent`, `Dedent`, `Eof`. These have no backing source slice, so `text`
// is empty — the empty `text` is what distinguishes them from lexed tokens.
fn synthetic(kind: TokenKind, start: u32, line: u32, col: u32) -> Token<'static> {
    Token {
        kind,
        text: "",
        start,
        line,
        col,
    }
}

// Emit the `Newline` and any indent change recorded for a line boundary, once a content-bearing
// line has been confirmed to follow. Does nothing if no newline is pending.
fn flush_pending_newline(
    out: &mut Vec<Token<'_>>,
    indents: &mut Vec<usize>,
    pending: &mut Option<(usize, u32, u32, u32)>,
) -> Result<(), LexError> {
    if let Some((indent, offset, line, col)) = pending.take() {
        out.push(synthetic(TokenKind::Newline, offset, line, col));
        emit_indent(out, indents, indent, offset, line, col)?;
    }
    Ok(())
}

fn emit_indent(
    out: &mut Vec<Token<'_>>,
    indents: &mut Vec<usize>,
    indent: usize,
    offset: u32,
    line: u32,
    col: u32,
) -> Result<(), LexError> {
    let top = *indents.last().unwrap();
    if indent > top {
        indents.push(indent);
        out.push(synthetic(TokenKind::Indent, offset, line, col));
    } else if indent < top {
        while indent < *indents.last().unwrap() {
            indents.pop();
            out.push(synthetic(TokenKind::Dedent, offset, line, col));
        }
        if *indents.last().unwrap() != indent {
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
        lex(src)
            .unwrap()
            .into_iter()
            .map(|token| token.kind)
            .collect()
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
            .filter(|token| token.kind == TokenKind::Identifier)
            .map(|token| token.text)
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
    fn comparison_and_logical_tokens() {
        use TokenKind::*;
        assert_eq!(
            kinds("== != < > <= >="),
            vec![EqEq, Ne, Lt, Gt, Le, Ge, Eof]
        );
        // Logical/value words become keywords; `bool` stays an ordinary identifier (resolved
        // only in type-annotation position, like `int`/`float`/`void`).
        assert_eq!(
            kinds("true false and or xor not bool"),
            vec![KwTrue, KwFalse, KwAnd, KwOr, KwXor, KwNot, Identifier, Eof]
        );
    }

    #[test]
    fn control_flow_keywords() {
        use TokenKind::*;
        assert_eq!(
            kinds("if elif else while until loop"),
            vec![KwIf, KwElif, KwElse, KwWhile, KwUntil, KwLoop, Eof]
        );
    }

    #[test]
    fn range_operators() {
        use TokenKind::*;
        // Longest-match: `..<`/`..>` beat `..`; `for`/`in` classify as keywords.
        assert_eq!(
            kinds("1..10 1..<10 5..>1 for idx in"),
            vec![
                Int, DotDot, Int, Int, DotDotLt, Int, Int, DotDotGt, Int, KwFor, Identifier, KwIn,
                Eof
            ]
        );
    }

    #[test]
    fn bracket_and_dot_tokens() {
        use TokenKind::*;
        // `[` and `]` are their own tokens.
        assert_eq!(
            kinds("[1, 2]"),
            vec![LBracket, Int, Comma, Int, RBracket, Eof]
        );
        // `.len` is Dot + Identifier; `.5` stays a float; `..` is still DotDot.
        assert_eq!(kinds("arr.len"), vec![Identifier, Dot, Identifier, Eof]);
        assert_eq!(kinds(".5"), vec![Float, Eof]);
        assert_eq!(kinds("1..5"), vec![Int, DotDot, Int, Eof]);
        // Brackets join lines just like parentheses.
        let src = indoc! {"
            [1,
             2]
        "};
        assert_eq!(
            kinds(src),
            vec![LBracket, Int, Comma, Int, RBracket, Newline, Eof]
        );
    }

    #[test]
    fn dot_dot_does_not_disturb_floats() {
        use TokenKind::*;
        // A dot followed by a digit is still a float; two dots are the range operator.
        assert_eq!(kinds(".5"), vec![Float, Eof]);
        assert_eq!(kinds("1..5"), vec![Int, DotDot, Int, Eof]);
        // A stepped range: `1..10:2` carries the step after a `Colon`.
        assert_eq!(kinds("1..10:2"), vec![Int, DotDot, Int, Colon, Int, Eof]);
    }

    #[test]
    fn multichar_comparisons_beat_bare_operators() {
        use TokenKind::*;
        // Longest-match: `==` over `= =`, `<=`/`>=` over `<`/`>` then `=`.
        assert_eq!(kinds("a == b"), vec![Identifier, EqEq, Identifier, Eof]);
        assert_eq!(kinds("a <= b"), vec![Identifier, Le, Identifier, Eof]);
        assert_eq!(kinds("a = b"), vec![Identifier, Equal, Identifier, Eof]);
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
            vec![
                Identifier, Equal, Int, Newline, Identifier, Equal, Int, Newline, Eof
            ]
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
                Identifier, Colon, Identifier, RParen, Colon, Identifier,
                KwIs, // b: float): float is
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
        let bb = toks.iter().find(|token| token.text == "bb").unwrap();
        assert_eq!((bb.line, bb.col), (2, 3));
        assert_eq!((toks[0].line, toks[0].col), (1, 1));
    }

    #[test]
    fn keeps_text_slice() {
        let toks = lex("g = 12_000_321").unwrap();
        assert_eq!(toks[0].text, "g");
        assert_eq!(toks[2].text, "12_000_321");
    }

    #[test]
    fn trailing_comment_is_ignored() {
        use TokenKind::*;
        // A comment after code leaves the code untouched and the line's newline intact.
        assert_eq!(
            kinds("a = 1  # the answer\n"),
            vec![Identifier, Equal, Int, Newline, Eof]
        );
    }

    #[test]
    fn comment_only_line_emits_nothing() {
        use TokenKind::*;
        let src = indoc! {"
            # leading note
            a = 1
            # trailing note
        "};
        assert_eq!(kinds(src), vec![Identifier, Equal, Int, Newline, Eof]);
    }

    #[test]
    fn blank_lines_do_not_break_a_block() {
        use TokenKind::*;
        // A blank line at the left margin inside an indented block must not be read as the
        // block ending; the indent stack only reacts to lines that carry content.
        let src = indoc! {"
            a = 1
                b = 2

                c = 3
            d = 4
        "};
        assert_eq!(
            kinds(src),
            vec![
                Identifier, Equal, Int, Newline, Indent, // a = 1, then indent
                Identifier, Equal, Int, Newline, // b = 2 (blank line below skipped)
                Identifier, Equal, Int, Newline, Dedent, // c = 3, then back out
                Identifier, Equal, Int, Newline, // d = 4
                Eof
            ]
        );
    }

    #[test]
    fn semicolon_is_a_token() {
        use TokenKind::*;
        // `;` is the 2D matrix row separator; it appears inside `[…]` brackets so no
        // newline logic fires. Here we test it as a bare token and inside brackets.
        assert_eq!(kinds(";"), vec![Semicolon, Eof]);
        assert_eq!(
            kinds("[1, 2; 3, 4]"),
            vec![
                LBracket, Int, Comma, Int, Semicolon, Int, Comma, Int, RBracket, Eof
            ]
        );
        // Existing tokens are undisturbed around `;`.
        assert_eq!(kinds("a; b"), vec![Identifier, Semicolon, Identifier, Eof]);
    }

    // ── string literals ─────────────────────────────────────────────────────

    #[test]
    fn string_both_quote_styles() {
        use TokenKind::*;
        // Single and double quotes are interchangeable; only the captured slice differs.
        assert_eq!(kinds("'hello'"), vec![Str, Eof]);
        assert_eq!(kinds("\"hello\""), vec![Str, Eof]);
        assert_eq!(lex("'hello'").unwrap()[0].text, "'hello'");
        assert_eq!(lex("\"hello\"").unwrap()[0].text, "\"hello\"");
    }

    #[test]
    fn empty_string_literal() {
        use TokenKind::*;
        // `''`/`""` is a single empty-string token — the two delimiters with no content between —
        // not two separate quote characters.
        assert_eq!(kinds("''"), vec![Str, Eof]);
        assert_eq!(lex("''").unwrap()[0].text, "''");
        assert_eq!(kinds("\"\""), vec![Str, Eof]);
        assert_eq!(lex("\"\"").unwrap()[0].text, "\"\"");
    }

    #[test]
    fn string_content_is_opaque() {
        use TokenKind::*;
        // Spaces, operators, digits, keywords, and `#` inside a literal are all just content:
        // no whitespace skipping, no comment, no sub-tokenisation.
        assert_eq!(kinds("'if 1 + 2  # x'"), vec![Str, Eof]);
        assert_eq!(lex("'if 1 + 2  # x'").unwrap()[0].text, "'if 1 + 2  # x'");
    }

    #[test]
    fn opposite_quote_allowed_inside() {
        // The non-delimiter quote is ordinary content — no escaping, no mixed delimiters.
        assert_eq!(lex("\"can't\"").unwrap()[0].text, "\"can't\"");
        assert_eq!(lex("'say \"hi\"'").unwrap()[0].text, "'say \"hi\"'");
    }

    #[test]
    fn fstring_both_quote_styles() {
        use TokenKind::*;
        assert_eq!(kinds("f'hi'"), vec![FStr, Eof]);
        assert_eq!(kinds("f\"hi\""), vec![FStr, Eof]);
        assert_eq!(lex("f'hi'").unwrap()[0].text, "f'hi'");
    }

    #[test]
    fn bare_f_stays_identifier() {
        use TokenKind::*;
        // Longest-match yields FStr only when a quote immediately follows `f`.
        assert_eq!(kinds("f"), vec![Identifier, Eof]);
        assert_eq!(kinds("f + 1"), vec![Identifier, Plus, Int, Eof]);
        assert_eq!(kinds("foo"), vec![Identifier, Eof]);
        // A space splits it into an identifier and a plain string.
        assert_eq!(kinds("f 'hi'"), vec![Identifier, Str, Eof]);
    }

    #[test]
    fn string_adjacent_to_tokens() {
        use TokenKind::*;
        assert_eq!(kinds("a + 'b'"), vec![Identifier, Plus, Str, Eof]);
        assert_eq!(kinds("'a' 'b'"), vec![Str, Str, Eof]);
        // `.len` on a literal is unaffected: Str, Dot, Identifier.
        assert_eq!(kinds("'hi'.len"), vec![Str, Dot, Identifier, Eof]);
    }

    #[test]
    fn unterminated_string_is_error_at_opening_quote() {
        // No closing quote before end of input: the opening quote is the failure site.
        let err = lex("'abc").unwrap_err();
        assert_eq!((err.line, err.col), (1, 1));
        assert!(lex("\"abc").is_err());
        assert!(lex("f'abc").is_err());
    }

    // ── spans and line resolution ───────────────────────────────────────────

    #[test]
    fn a_token_records_where_it_starts() {
        let src = "abc = 42";
        let tokens = lex(src).unwrap();
        assert_eq!(tokens[0].span().text(src), "abc");
        assert_eq!(tokens[1].span().text(src), "=");
        assert_eq!(tokens[2].span().text(src), "42");
    }

    #[test]
    fn a_span_covers_a_whole_multiline_literal() {
        // The span is a byte range, so a token spanning lines needs no special handling.
        let src = "x = 'one\ntwo'";
        let literal = lex(src)
            .unwrap()
            .into_iter()
            .find(|token| token.kind == TokenKind::Str)
            .unwrap();
        assert_eq!(literal.span().text(src), "'one\ntwo'");
    }

    #[test]
    fn line_index_resolves_offsets() {
        let src = "ab\ncde\n\nf";
        let index = LineIndex::new(src);
        assert_eq!(index.locate(0), (1, 1), "first byte");
        assert_eq!(index.locate(1), (1, 2));
        assert_eq!(index.locate(3), (2, 1), "first byte of a line");
        assert_eq!(index.locate(5), (2, 3));
        assert_eq!(index.locate(7), (3, 1), "an empty line");
        assert_eq!(index.locate(8), (4, 1));
        assert_eq!(
            index.locate(src.len() as u32),
            (4, 2),
            "one past the end, where EOF is reported"
        );
    }

    #[test]
    fn line_index_agrees_with_reported_error_positions() {
        // The lexer places its errors through the same index, so the two can never drift.
        let src = "f() is\n\tx = 1\n";
        let error = lex(src).unwrap_err();
        let index = LineIndex::new(src);
        let tab_offset = src.find('\t').unwrap() as u32;
        assert_eq!((error.line, error.col), index.locate(tab_offset));
    }

    #[test]
    fn an_unknown_span_is_recognisable() {
        assert!(Span::UNKNOWN.is_unknown());
        assert!(!Span::new(0, 1).is_unknown());
    }

    // ── multiline string literals ───────────────────────────────────────────
    //
    // The lexer's job here ends at the token: it captures the whole literal, delimiters included,
    // and leaves the surrounding layout stream untouched. What the *content* becomes — stripping
    // the delimiters, auto-dedenting the continuation lines — belongs to the parser, and is tested
    // there against the AST rather than here against a raw slice.

    #[test]
    fn a_multiline_literal_is_one_token() {
        use TokenKind::*;
        let src = "'first\nsecond\nthird'";
        assert_eq!(kinds(src), vec![Str, Eof]);
        assert_eq!(
            lex(src).unwrap()[0].text,
            src,
            "the whole literal, verbatim"
        );
        // Both quote styles, and the opposite quote is still ordinary content across lines.
        assert_eq!(kinds("\"first\nsecond\""), vec![Str, Eof]);
        assert_eq!(lex("'it\"s\nfine'").unwrap()[0].text, "'it\"s\nfine'");
    }

    #[test]
    fn a_multiline_fstring_is_one_token() {
        use TokenKind::*;
        let src = "f'first {x}\nsecond'";
        assert_eq!(kinds(src), vec![FStr, Eof]);
        assert_eq!(lex(src).unwrap()[0].text, src);
    }

    #[test]
    fn a_multiline_literal_leaves_the_layout_stream_intact() {
        use TokenKind::*;
        // The newlines inside the literal are content, not statement boundaries, so they must not
        // reach the indent/dedent logic: the block below opens once and closes once.
        let src = indoc! {"
            f() is
                x = 'first
                     second'
                y = 2
            z = 3
        "};
        assert_eq!(
            kinds(src),
            vec![
                Identifier, LParen, RParen, KwIs, Newline, Indent, // f() is
                Identifier, Equal, Str, Newline, // x = '…' spanning two physical lines
                Identifier, Equal, Int, Newline, Dedent, // y = 2, then back out
                Identifier, Equal, Int, Newline, // z = 3
                Eof
            ]
        );
    }

    #[test]
    fn positions_after_a_multiline_literal_stay_correct() {
        // Everything downstream locates from its own byte offset, so a token following a literal
        // that spanned three lines still reports the line it is actually on.
        let src = "x = 'one\ntwo\nthree'\ny = 2";
        let tokens = lex(src).unwrap();
        let literal = tokens
            .iter()
            .find(|token| token.kind == TokenKind::Str)
            .unwrap();
        assert_eq!((literal.line, literal.col), (1, 5), "the opening quote");
        let last = tokens
            .iter()
            .rfind(|token| token.kind == TokenKind::Identifier)
            .unwrap();
        assert_eq!(last.text, "y");
        assert_eq!((last.line, last.col), (4, 1));
    }

    #[test]
    fn an_unterminated_multiline_literal_fails_at_the_opening_quote() {
        // The accepted trade-off of spanning lines: a forgotten closing quote is only discovered at
        // end of input, but it is still reported where the literal began.
        let err = lex("x = 'abc\nmore text\n").unwrap_err();
        assert_eq!((err.line, err.col), (1, 5));
        assert!(lex("\"abc\nmore").is_err());
        assert!(lex("f'abc\nmore").is_err());
    }

    // ── tabs and CRLF ───────────────────────────────────────────────────────

    #[test]
    fn a_tab_in_indentation_is_rejected() {
        // pinp's layout is space-based. A tab used for indentation would otherwise be skipped as
        // ordinary whitespace, leaving the line at indent 0 and failing later with a confusing
        // "Expected Indent"; naming it here is far more useful.
        let err = lex("f() is\n\tx = 1\n").unwrap_err();
        assert_eq!((err.line, err.col), (2, 1));
        assert!(err.message.contains("Tab"), "message was {:?}", err.message);
        // A tab after some spaces is still indentation.
        assert!(lex("f() is\n    \tx = 1\n").is_err());
    }

    #[test]
    fn a_tab_between_tokens_is_ordinary_whitespace() {
        use TokenKind::*;
        // Only leading whitespace is layout; aligning inside a line is nobody's business.
        assert_eq!(kinds("a\t+\tb"), vec![Identifier, Plus, Identifier, Eof]);
        assert_eq!(
            kinds("'a\tb'"),
            vec![Str, Eof],
            "and a tab inside a literal"
        );
    }

    #[test]
    fn crlf_line_endings_are_accepted() {
        use TokenKind::*;
        // A Windows checkout must compile. `\r\n` is a line terminator like `\n`, indentation and
        // all.
        let src = "f() is\r\n    x = 1\r\n    x\r\ny = 2\r\n";
        assert_eq!(
            kinds(src),
            vec![
                Identifier, LParen, RParen, KwIs, Newline, Indent, // f() is
                Identifier, Equal, Int, Newline, // x = 1
                Identifier, Newline, Dedent, // x, then back out
                Identifier, Equal, Int, Newline, // y = 2
                Eof
            ]
        );
    }

    #[test]
    fn a_lone_carriage_return_is_still_an_error() {
        // Only `\r\n` is a line ending; a bare `\r` (old-Mac style) is not something to guess at.
        assert!(lex("a = 1\rb = 2").is_err());
    }

    #[test]
    fn comment_only_line_is_transparent_regardless_of_indent() {
        use TokenKind::*;
        // An over-indented comment line inside a block contributes no Indent/Dedent.
        let src = indoc! {"
            a = 1
                b = 2
                        # an over-indented remark
                c = 3
        "};
        assert_eq!(
            kinds(src),
            vec![
                Identifier, Equal, Int, Newline, Indent, // a = 1, then indent
                Identifier, Equal, Int, Newline, // b = 2
                Identifier, Equal, Int, Newline, Dedent, // c = 3, then back out
                Eof
            ]
        );
    }
}
