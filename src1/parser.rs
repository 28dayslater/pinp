use crate::lexer::*;

pub mod ats {
 // Direct source code AST nodes - straight from the parser.

// An index into Ast::exprs identifying an expression node.
//
// Child nodes refer to one another by int ExprId rather than Box<Expr>, so the AST
// owns all expression nodes in one Vec.
//
// This gives us:
// - central ownership of nodes
// - stable references by index even if the Vec reallocates
// - fewer per-node heap allocations
// - better cache locality during traversal
//
// Tradeoff: each Expr still has the size of its largest ExprKind variant.
    pub struct ExprId(u32);

    pub enum PinpType<'src> {
        Bool(bool),
        Int(i64),
        Float(f64),
        String(&'src str),
    }
    
    pub enum Node {

    }
}

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
