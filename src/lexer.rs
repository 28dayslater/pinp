use logos::{Lexer, Logos};

#[derive(Debug, PartialEq)]
pub struct SourceLoc {
    pub line: usize,
    pub column: usize,
}

pub struct LexerState {
    // source byte offsets of line beginnings for diagnostic line/column mapping
    line_starts: Vec<usize>,
}

// Default ctor - need line[0] to be 0
impl Default for LexerState {
    fn default() -> Self {
        Self {
            line_starts: vec![0],
        }
    }
}

impl LexerState {
    // (line, column) of the current token
    pub fn source_location(&self, byte_offset: usize) -> SourceLoc {
        // Find the last line which starts below or at the byte offset
        let lineidx = self
            .line_starts
            .partition_point(|&start| start <= byte_offset)
            .saturating_sub(1);
        SourceLoc {
            line: lineidx + 1,
            column: byte_offset - self.line_starts[lineidx] + 1,
        }
    }
}

// Add source_location() to Lexer
#[allow(dead_code)] // The trait starts being used by invoking source_location() on lex.
trait LexerExt {
    fn source_location(&self, byte_offset: usize) -> SourceLoc;
}

impl LexerExt for logos::Lexer<'_, Token> {
    fn source_location(&self, byte_offset: usize) -> SourceLoc {
        self.extras.source_location(byte_offset)
    }
}

fn indent_cb(lex: &mut Lexer<Token>) -> usize {
    let slice = lex.slice();
    lex.extras.line_starts.push(lex.span().start + 1);
    slice.len() - 1
}

#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"[ \t]+")] // ignore whitespace between tokens in a line
#[logos(extras = LexerState)]
pub enum Token {
    #[regex("\n *", indent_cb)]
    // Number of leading spaces in a line - used for indent/dedent
    NewLineIndent(usize),

    // Plain integer literal. Scientific notation is allowed.
    // The exponent must be (>=0).
    // Examples: 1234 12E3
    #[regex("[0-9]+([eE][0-9]+)?")]
    // Integer literal with groups separated by _ for better readability.
    // Groups are required to be three digit.
    // Scientific notation not allowed.
    // Example: 12_000_321
    // This will not match: 12_11
    #[regex(r"[0-9]{1,3}(_[0-9]{3})+")]
    Int,

    // Hex: 0x... with arbitrary grouping by _
    #[regex(r"0x[a-fA-F0-9]+(_[a-fA-F0-9]+)*")]
    HexInt,

    // Binary: 0b... with arbitrary grouping by _
    #[regex(r"0b[01]+(_[01]+)*")]
    BinInt,

    // Floating point literal with optional digit groups.
    // Examples: .12 123.456 3.14E12 12_000_00.2_333_668E-1_000 (unrealistic).
    // Groups in the exponent are supported, despite not having any sense.
    // Unfortunately multiline RE with comments is not supported, so this is one ugly beast.
    // See match_float_test before modifying.
    #[regex(r"([0-9]{1,3}(_[0-9]{3})+|[0-9]+)?\.([0-9]{1,3}(_[0-9]{3})+|[0-9]+)([eE][+-]?([0-9]{1,3}(_[0-9]{3})+|[0-9]+))?")]
    Float,

    #[regex(r"_*[a-zA-Z]([a-zA-Z0-9_])*")]
    Identifier,

    #[token("+")]
    Plus,

    #[token("-")]
    Minus,

    #[token("*")]
    Mul,

    #[token("/")]
    Div,

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

    #[token(":")]
    Colon,

    #[token(",")]
    Comma,
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    macro_rules! assert_tok {
        ($lex:expr, $tok:expr) => {{
            assert_eq!($lex.next(), Some(Ok($tok)));
        }};

        ($lex:expr, $tok:expr, $slice:expr) => {{
            assert_eq!($lex.next(), Some(Ok($tok)));
            assert_eq!($lex.slice(), $slice);
        }};
    }

    macro_rules! assert_tokens {
        ($lex:expr, $($tok:expr),+ $(,)?) => {{
            $(
                assert_eq!($lex.next(), Some(Ok($tok)));
            )+
        }};
    }

    mod numeric_literals {
        use super::*;
        #[test]
        fn match_int() {
            let mut lex = Token::lexer("0    1234 12_000_000_000 16E10 1234e0");
            assert_tok!(lex, Token::Int, "0");
            assert_tok!(lex, Token::Int, "1234");
            assert_tok!(lex, Token::Int, "12_000_000_000");
            assert_tok!(lex, Token::Int, "16E10");
            assert_tok!(lex, Token::Int, "1234e0");
        }

        #[test]
        fn match_hex() {
            let mut lex = Token::lexer("0x0 0xFFF2  0xcde  0x7000_12_FF_123a");
            assert_tok!(lex, Token::HexInt, "0x0");
            assert_tok!(lex, Token::HexInt, "0xFFF2");
            assert_tok!(lex, Token::HexInt, "0xcde");
            assert_tok!(lex, Token::HexInt, "0x7000_12_FF_123a");
        }

        #[test]
        fn match_bin() {
            let mut lex = Token::lexer("0b0 0b100010101010100111 0b1_1_11_0000_0010_1010");
            assert_tok!(lex, Token::BinInt, "0b0");
            assert_tok!(lex, Token::BinInt, "0b100010101010100111");
            assert_tok!(lex, Token::BinInt, "0b1_1_11_0000_0010_1010");
        }

        #[test]
        fn match_float() {
            let mut lex = Token::lexer(
                ".0 0.0 .1933 .1_123 127.000001 3.14E12 1_000.000_000e-1_000 1_000.0 100.000_001 12_000_000.2_333_668E-1_000",
            );
            assert_tok!(lex, Token::Float, ".0");
            assert_tok!(lex, Token::Float, "0.0");
            assert_tok!(lex, Token::Float, ".1933");
            assert_tok!(lex, Token::Float, ".1_123");
            assert_tok!(lex, Token::Float, "127.000001");
            assert_tok!(lex, Token::Float, "3.14E12");
            assert_tok!(lex, Token::Float, "1_000.000_000e-1_000");
            assert_tok!(lex, Token::Float, "1_000.0");
            assert_tok!(lex, Token::Float, "100.000_001");
            assert_tok!(lex, Token::Float, "12_000_000.2_333_668E-1_000");
        }
    }

    #[test]
    fn match_identifier() {
        let mut lex = Token::lexer("a Fu _BAR _baz_baz_ _fu12_11_bar");
        assert_tok!(lex, Token::Identifier, "a");
        assert_tok!(lex, Token::Identifier, "Fu");
        assert_tok!(lex, Token::Identifier, "_BAR");
        assert_tok!(lex, Token::Identifier, "_baz_baz_");
        assert_tok!(lex, Token::Identifier, "_fu12_11_bar");
    }

    #[test]
    fn test_source_location() {
        let mut lex = Token::lexer(indoc! {"
            fu = 1234
                bar =  0.1 + 123 * fu
            "});
        assert_tokens!(lex, Token::Identifier, Token::Equal, Token::Int);
        assert_tok!(lex, Token::NewLineIndent(4));
        assert_tok!(lex, Token::Identifier);
        assert_eq!(
            lex.source_location(lex.span().start),
            // fu column is 1, bar is 5
            SourceLoc { line: 2, column: 5 }
        );
        assert_tokens!(
            lex,
            Token::Equal,
            Token::Float,
            Token::Plus,
            Token::Int,
            Token::Mul,
            Token::Identifier
        );
        assert_eq!(
            lex.source_location(lex.span().start),
            SourceLoc {
                line: 2,
                column: 24
            }
        );
    }
}
