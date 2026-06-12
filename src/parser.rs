use crate::lexer::*;




#[cfg(test)]
mod tests {
    use logos::Logos;
    use crate::lexer::*;

    #[test]
    fn can_use_lexer() {
        let mut lex = Token::lexer("_fu = 42");
        assert_eq!(lex.next(), Some(Ok(Token::Identifier)));
    }
}
