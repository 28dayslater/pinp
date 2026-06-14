//! Recursive-descent parser with Pratt (precedence-climbing) expression parsing.
//!
//! Statements (`assignment`, `expr-stmt`) are handled by straight recursive descent;
//! expressions are parsed by [`Parser::expr`], a Pratt loop driven by the binding-power
//! table in [`infix`] rather than one grammar rule per precedence level. Each operator
//! carries a (left, right) binding power; left-associative ops use `right == left + 1`,
//! right-associative ops (`^`) use `right == left`. Prefix `-` is handled in
//! [`Parser::prefix`] at [`PREFIX_BP`].
//!
//! Output is an arena, not a tree of boxes:
//! - **AST arena** — nodes live in `Ast::nodes: Vec<Node>` and reference each other by
//!   [`ExprId`] (an index), so there is no `Box`/`Rc` and the whole AST is one contiguous
//!   allocation. The inferred [`PinpType`] of each node sits in the parallel `Ast::types`
//!   vec at the same index.
//! - **Interner** — identifiers map to [`SymId`] via `Ast::syms`, backed by `&'src str`
//!   slices borrowed straight from the source (no string copies). `SymId` -> name is the
//!   `Ast::names` vec.
//!
//! Type inference runs *inline* during parsing (see [`Parser::make_bin`]): every node gets
//! its `PinpType` as it is built, and type errors (e.g. `div`/`mod` on a `Float`, use of an
//! unassigned symbol) surface immediately as [`PinpError`].

use crate::lexer::{lex, TokenKind, Token};
use rustc_hash::FxHashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinpType {
    Int,
    Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Mod,
    Pow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Int(i64),
    Float(f64),
    Var(SymId),
    Unary { op: UnOp, operand: ExprId },
    Bin { op: BinOp, lhs: ExprId, rhs: ExprId },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Assign { target: SymId, rhs: ExprId },
    Expr(ExprId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PinpError {
    Lex(String),
    Unexpected(String),
    UnknownSymbol(String),
    Type(String),
}

#[derive(Default)]
pub struct Ast<'src> {
    pub nodes: Vec<Node>,
    pub types: Vec<PinpType>,
    pub stmts: Vec<Stmt>,
    pub names: Vec<&'src str>,
    syms: FxHashMap<&'src str, SymId>,
    sym_types: FxHashMap<SymId, PinpType>,
}

impl<'src> Ast<'src> {
    pub fn node(&self, e: ExprId) -> &Node {
        &self.nodes[e.0 as usize]
    }

    pub fn type_of(&self, e: ExprId) -> PinpType {
        self.types[e.0 as usize]
    }

    fn push(&mut self, node: Node, ty: PinpType) -> ExprId {
        let id = ExprId(self.nodes.len() as u32);
        self.nodes.push(node);
        self.types.push(ty);
        id
    }

    fn intern(&mut self, name: &'src str) -> SymId {
        if let Some(&id) = self.syms.get(name) {
            return id;
        }
        let id = SymId(self.names.len() as u32);
        self.names.push(name);
        self.syms.insert(name, id);
        id
    }
}

pub fn parse(src: &str) -> Result<Ast<'_>, PinpError> {
    let toks = lex(src).map_err(|e| PinpError::Lex(e.text))?;
    let mut p = Parser {
        toks,
        pos: 0,
        ast: Ast::default(),
    };
    p.program()?;
    Ok(p.ast)
}

struct Parser<'src> {
    toks: Vec<Token<'src>>,
    pos: usize,
    ast: Ast<'src>,
}

// (left binding power, right binding power, operator). Right-assoc ops (`^`) have
// right == left; left-assoc ops have right == left + 1.
fn infix(kind: TokenKind) -> Option<(u8, u8, BinOp)> {
    use BinOp::*;
    use TokenKind::*;
    Some(match kind {
        Plus => (60, 61, Add),
        Minus => (60, 61, Sub),
        Star => (70, 71, Mul),
        Slash => (70, 71, Div),
        KwDiv => (70, 71, IntDiv),
        KwMod => (70, 71, Mod),
        Caret => (80, 80, Pow),
        _ => return None,
    })
}

const PREFIX_BP: u8 = 75;

impl<'src> Parser<'src> {
    fn peek(&self) -> &Token<'src> {
        &self.toks[self.pos]
    }

    fn at(&self, offset: usize) -> TokenKind {
        self.toks[self.pos + offset].kind
    }

    fn advance(&mut self) -> Token<'src> {
        let t = self.toks[self.pos].clone();
        self.pos += 1;
        t
    }

    fn skip_separators(&mut self) {
        while matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            self.pos += 1;
        }
    }

    fn program(&mut self) -> Result<(), PinpError> {
        loop {
            self.skip_separators();
            if self.peek().kind == TokenKind::Eof {
                return Ok(());
            }
            self.statement()?;
            match self.peek().kind {
                TokenKind::Newline | TokenKind::Eof => {}
                other => return Err(PinpError::Unexpected(format!("{other:?}"))),
            }
        }
    }

    fn statement(&mut self) -> Result<(), PinpError> {
        if self.peek().kind == TokenKind::Identifier && self.at(1) == TokenKind::Equal {
            let name = self.advance().text;
            self.advance(); // '='
            let rhs = self.expr(0)?;
            let rhs_type = self.ast.type_of(rhs);
            let target = self.ast.intern(name);
            self.ast.sym_types.insert(target, rhs_type);
            self.ast.stmts.push(Stmt::Assign { target, rhs });
        } else {
            let e = self.expr(0)?;
            self.ast.stmts.push(Stmt::Expr(e));
        }
        Ok(())
    }

    fn expr(&mut self, min_bp: u8) -> Result<ExprId, PinpError> {
        let mut lhs = self.prefix()?;
        while let Some((lbp, rbp, op)) = infix(self.peek().kind) {
            if lbp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.expr(rbp)?;
            lhs = self.make_bin(op, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn prefix(&mut self) -> Result<ExprId, PinpError> {
        if self.peek().kind == TokenKind::Minus {
            self.advance();
            let operand = self.expr(PREFIX_BP)?;
            let operand_type = self.ast.type_of(operand);
            return Ok(self
                .ast
                .push(Node::Unary { op: UnOp::Neg, operand }, operand_type));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<ExprId, PinpError> {
        match self.peek().kind {
            TokenKind::Int => {
                let v = parse_int(self.advance().text);
                Ok(self.ast.push(Node::Int(v), PinpType::Int))
            }
            TokenKind::Float => {
                let v = parse_float(self.advance().text);
                Ok(self.ast.push(Node::Float(v), PinpType::Float))
            }
            TokenKind::Identifier => {
                let name = self.advance().text;
                match self.ast.syms.get(name).copied() {
                    Some(sym) if self.ast.sym_types.contains_key(&sym) => {
                        let var_type = self.ast.sym_types[&sym];
                        Ok(self.ast.push(Node::Var(sym), var_type))
                    }
                    _ => Err(PinpError::UnknownSymbol(name.to_string())),
                }
            }
            TokenKind::LParen => {
                self.advance();
                let e = self.expr(0)?;
                if self.peek().kind != TokenKind::RParen {
                    return Err(PinpError::Unexpected(format!("{:?}", self.peek().kind)));
                }
                self.advance();
                Ok(e)
            }
            other => Err(PinpError::Unexpected(format!("{other:?}"))),
        }
    }

    fn make_bin(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId) -> Result<ExprId, PinpError> {
        use PinpType::*;
        let (left_type, right_type) = (self.ast.type_of(lhs), self.ast.type_of(rhs));
        let both_int = left_type == Int && right_type == Int;
        let result_type = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Pow => {
                if both_int {
                    Int
                } else {
                    Float
                }
            }
            BinOp::Div => Float,
            BinOp::IntDiv | BinOp::Mod => {
                if !both_int {
                    return Err(PinpError::Type(format!("{op:?} requires Int operands")));
                }
                Int
            }
        };
        Ok(self.ast.push(Node::Bin { op, lhs, rhs }, result_type))
    }
}

fn parse_int(text: &str) -> i64 {
    let s: String = text.chars().filter(|&c| c != '_').collect();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).unwrap();
    }
    if let Some(b) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        return i64::from_str_radix(b, 2).unwrap();
    }
    if let Some(pos) = s.find(['e', 'E']) {
        let exp: u32 = s[pos + 1..].parse().unwrap();
        return s[..pos].parse::<i64>().unwrap() * 10i64.pow(exp);
    }
    s.parse().unwrap()
}

fn parse_float(text: &str) -> f64 {
    let s: String = text.chars().filter(|&c| c != '_').collect();
    s.parse().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> Ast<'_> {
        parse(src).unwrap()
    }

    fn root(ast: &Ast) -> ExprId {
        match ast.stmts.last().unwrap() {
            Stmt::Expr(e) => *e,
            Stmt::Assign { rhs, .. } => *rhs,
        }
    }

    #[test]
    fn precedence_mul_over_add() {
        let ast = parse_ok("2 + 3 * 4");
        let r = root(&ast);
        let Node::Bin { op: BinOp::Add, lhs, rhs } = *ast.node(r) else {
            panic!("expected add at root");
        };
        assert_eq!(*ast.node(lhs), Node::Int(2));
        assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Mul, .. }));
        assert_eq!(ast.type_of(r), PinpType::Int);
    }

    #[test]
    fn power_is_right_assoc() {
        let ast = parse_ok("2 ^ 2 ^ 3");
        let Node::Bin { op: BinOp::Pow, lhs, rhs } = *ast.node(root(&ast)) else {
            panic!();
        };
        assert_eq!(*ast.node(lhs), Node::Int(2));
        assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Pow, .. }));
    }

    #[test]
    fn unary_minus_binds_below_power() {
        // -2 ^ 2 == -(2 ^ 2)
        let ast = parse_ok("-2 ^ 2");
        let Node::Unary { op: UnOp::Neg, operand } = *ast.node(root(&ast)) else {
            panic!("expected unary neg at root");
        };
        assert!(matches!(ast.node(operand), Node::Bin { op: BinOp::Pow, .. }));
    }

    #[test]
    fn paren_grouping() {
        let ast = parse_ok("(2 + 3) * 4");
        let Node::Bin { op: BinOp::Mul, lhs, .. } = *ast.node(root(&ast)) else {
            panic!();
        };
        assert!(matches!(ast.node(lhs), Node::Bin { op: BinOp::Add, .. }));
    }

    #[test]
    fn type_inference_rules() {
        assert_eq!(ast_type("2 + 3"), PinpType::Int);
        assert_eq!(ast_type("10 / 4"), PinpType::Float);
        assert_eq!(ast_type("10 div 4"), PinpType::Int);
        assert_eq!(ast_type("7 mod 3"), PinpType::Int);
        assert_eq!(ast_type("2 ^ 10"), PinpType::Int);
        assert_eq!(ast_type("2.0 ^ 10"), PinpType::Float);
        assert_eq!(ast_type("-3.14"), PinpType::Float);
    }

    fn ast_type(src: &str) -> PinpType {
        let ast = parse_ok(src);
        ast.type_of(root(&ast))
    }

    #[test]
    fn grouped_int_literal_value() {
        let ast = parse_ok("12_000_321");
        assert_eq!(*ast.node(root(&ast)), Node::Int(12_000_321));
    }

    #[test]
    fn int_promotes_to_float() {
        let ast = parse_ok("a = 2\n2.0 * a");
        assert_eq!(ast.type_of(root(&ast)), PinpType::Float);
    }

    #[test]
    fn assignment_then_reference() {
        let ast = parse_ok("a = 2 + 3\na * a");
        let r = root(&ast);
        assert_eq!(ast.type_of(r), PinpType::Int);
        assert!(matches!(ast.node(r), Node::Bin { op: BinOp::Mul, .. }));
    }

    #[test]
    fn float_div_is_type_error() {
        assert!(matches!(parse("2.0 div 1"), Err(PinpError::Type(_))));
    }

    #[test]
    fn unassigned_symbol_is_error() {
        assert!(matches!(parse("x + 1"), Err(PinpError::UnknownSymbol(_))));
    }
}
