mod lexer;
mod parser;

use lexer::{SourceLoc, Token};
use logos::Logos;

fn main() {
    let mut lex = Token::lexer(
        "
    12_000_000    
      .1 127.0  3.14E-15",
    );
    assert_eq!(lex.next(), Some(Ok(Token::NewLineIndent(4))));
    assert_eq!(lex.next(), Some(Ok(Token::Int)));
    assert_eq!(
        lex.extras.source_location(lex.span().start),
        SourceLoc { line: 2, column: 5 }
    );
    assert_eq!(lex.next(), Some(Ok(Token::NewLineIndent(6))));
    assert_eq!(lex.next(), Some(Ok(Token::Float)));
    assert_eq!(
        lex.extras.source_location(lex.span().start),
        SourceLoc { line: 3, column: 7 }
    );
    assert_eq!(lex.next(), Some(Ok(Token::Float)));
    assert_eq!(lex.next(), Some(Ok(Token::Float)));
}
