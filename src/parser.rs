// SPDX-License-Identifier: MIT

//! Recursive-descent parser with Pratt (precedence-climbing) expression parsing.
//!
//! A program is a sequence of [`TopLevel`]s — top-level statements (which run in the global
//! scope) and function definitions ([`FuncDef`]). Statements and function bodies are parsed
//! by straight recursive descent; expressions are parsed by [`Parser::parse_expr`], a Pratt loop
//! driven by the binding-power table in [`infix`]. Each operator carries a (left, right)
//! binding power; left-associative ops use `right == left + 1`, right-associative ops (`^`)
//! use `right == left`. Prefix `-` is handled in [`Parser::parse_prefix`] at [`UNARY_MINUS_BP`].
//!
//! Output is an arena, not a tree of boxes:
//! - **AST arena** — nodes live in `Ast::nodes: Vec<Node>` and reference each other by
//!   [`ExprId`] (an index), so there is no `Box`/`Rc`. The inferred [`PinpType`] of each node
//!   sits in the parallel `Ast::types` vec at the same index.
//! - **Interner** — identifiers map to [`SymId`] via `Ast::syms`, backed by `&'src str`
//!   slices borrowed straight from the source (no string copies).
//!
//! Parsing is **purely syntactic**: it builds the structural AST and reports only lexical/layout
//! errors ([`ParseError`]). The `types` arena is left unpopulated — name resolution, scoping, and
//! type inference belong to the [`crate::sema`] pass, which fills the types and reports
//! [`crate::sema::SemaError`]. Type annotations (`int`/`float`/`void`) are the one exception: they
//! are resolved here while building the signature, since that is a fixed lexical mapping.

use crate::lexer::{lex, Token, TokenKind};
use rustc_hash::{FxHashMap, FxHashSet};

/// The type of every expression and binding. `Void` is the type of a function with no declared
/// return type (and of a call to one); a value-typed expression is never `Void`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinpType {
    Int,
    Float,
    Void,
}

/// An interned identifier: an index into [`Ast::names`]. `Copy` and cheap to compare; the
/// backing text lives once in the interner, never re-stored per use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymId(pub u32);

/// An index into the [`Ast::nodes`] arena. The node's inferred [`PinpType`] sits at the same
/// index in the parallel [`Ast::types`] vec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExprId(pub u32);

/// A binary operator. `IntDiv` and `Mod` are the `div`/`mod` keyword operators (integer-only);
/// `Pow` is `^` (right-associative). `Div` (`/`) always yields `Float`.
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

/// A unary (prefix) operator. Negation is the only one so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
}

/// An expression in the AST arena. Child expressions are referenced by [`ExprId`] index rather
/// than boxed, so a `Node` is a flat, `Clone` value and the whole tree lives in one `Vec`.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Int(i64),
    Float(f64),
    Var(SymId),    // bare name: a parameter/local, or a top-level global
    Global(SymId), // `::name`
    Unary { op: UnOp, operand: ExprId },
    Bin { op: BinOp, lhs: ExprId, rhs: ExprId },
    Call { callee: SymId, args: Vec<ExprId> },
}

/// A named location that can be read or written: a bare name (current scope) or an
/// explicit `::` global. "Place" is the standard compiler term (Rust's "place expression")
/// for such a location — it appears on either side of `=`, not just as an assignment target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    Local(SymId),
    Global(SymId),
}

/// A statement: an assignment to a [`Place`], or an expression evaluated for its value. A
/// compound assignment (`x += e`) is desugared at parse time into an `Assign`.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Assign { target: Place, rhs: ExprId },
    Expr(ExprId),
}

/// A function parameter: its interned name and resolved type.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: SymId,
    pub param_type: PinpType,
}

/// A function body: zero or more statements followed by the `result` expression, whose type is
/// the function's return value. The single-line form has empty `stmts`.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub result: ExprId,
}

/// A parsed function definition: name, parameters, return type, and body.
#[derive(Debug, Clone, PartialEq)]
pub struct FuncDef {
    pub name: SymId,
    pub params: Vec<Param>,
    pub return_type: PinpType,
    pub body: Block,
}

/// One element at the top level of a program: a function definition, or a statement that runs in
/// the global scope.
#[derive(Debug, Clone, PartialEq)]
pub enum TopLevel {
    Func(FuncDef),
    Stmt(Stmt),
}

/// A syntactic failure from lexing or parsing. pinp is fail-fast: the first error returned stops
/// the pass. Semantic errors live in [`crate::sema::SemaError`].
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// A lexing failure (carries the [`crate::lexer::LexError`] message).
    Lex(String),
    /// A syntax error: an unexpected/missing token, or a malformed declaration (a duplicate
    /// parameter, an unknown type name, a single-line body without a return type, ...).
    Unexpected(String),
    /// An indentation or parameter-alignment error.
    Layout(String),
}

/// A parsed program. Borrows the source for the lifetime `'src` (identifiers are slices into it).
///
/// Layout: `nodes` and `types` are parallel arenas indexed by [`ExprId`]; `top_level` is the
/// program in source order; `names` maps a [`SymId`] back to its text. Returned by [`parse`] with
/// `types` unpopulated; [`crate::sema::analyze`] fills `types` in.
#[derive(Default)]
pub struct Ast<'src> {
    pub nodes: Vec<Node>,
    pub types: Vec<PinpType>,
    pub top_level: Vec<TopLevel>,
    pub names: Vec<&'src str>,
    syms: FxHashMap<&'src str, SymId>,
}

impl<'src> Ast<'src> {
    /// Borrows the node at `e`.
    pub fn node(&self, e: ExprId) -> &Node {
        &self.nodes[e.0 as usize]
    }

    /// The type of the node at `e` — valid only after [`crate::sema::analyze`] has run.
    pub fn type_of(&self, e: ExprId) -> PinpType {
        self.types[e.0 as usize]
    }

    /// Pushes a node, returning its id. The type is left as a placeholder for sema to fill.
    fn push(&mut self, node: Node) -> ExprId {
        let id = ExprId(self.nodes.len() as u32);
        self.nodes.push(node);
        self.types.push(PinpType::Void);
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

/// Lexes and parses `src` into a structural [`Ast`] (its `types` left for [`crate::sema::analyze`]
/// to fill). The returned AST borrows `src`. Stops at and returns the first [`ParseError`].
pub fn parse(src: &str) -> Result<Ast<'_>, ParseError> {
    let toks = lex(src).map_err(|e| ParseError::Lex(e.message))?;
    let mut p = Parser {
        toks,
        pos: 0,
        ast: Ast::default(),
    };
    p.parse_program()?;
    Ok(p.ast)
}

struct Parser<'src> {
    toks: Vec<Token<'src>>,
    pos: usize,
    ast: Ast<'src>,
}

// Binding-power "table". See articles on Pratt parsing.
// Regarding performance, `match` is register-only and const-foldable; a static table would just add
// a load and an ordering footgun — no speedup.
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

// Right binding power of prefix `-`: above `*`/`/` (70), below `^` (80), so `-a*b` is `(-a)*b`
// and `-a^b` is `-(a^b)`.
const UNARY_MINUS_BP: u8 = 75;

enum AssignKind {
    Plain,
    Compound(BinOp), // += *= ...
}

fn assign_op(kind: TokenKind) -> Option<AssignKind> {
    use TokenKind::*;
    Some(match kind {
        Equal => AssignKind::Plain,
        PlusEq => AssignKind::Compound(BinOp::Add),
        MinusEq => AssignKind::Compound(BinOp::Sub),
        StarEq => AssignKind::Compound(BinOp::Mul),
        SlashEq => AssignKind::Compound(BinOp::Div),
        CaretEq => AssignKind::Compound(BinOp::Pow),
        DivEq => AssignKind::Compound(BinOp::IntDiv),
        ModEq => AssignKind::Compound(BinOp::Mod),
        _ => return None,
    })
}

/// The expression that *reads* a [`Place`]: `x` -> `Var`, `::g` -> `Global`. Used to build the
/// `place` operand when desugaring `place <op>= e` into `place = place <op> e`.
impl From<Place> for Node {
    fn from(place: Place) -> Node {
        match place {
            Place::Local(s) => Node::Var(s),
            Place::Global(s) => Node::Global(s),
        }
    }
}

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

    fn expect(&mut self, kind: TokenKind) -> Result<Token<'src>, ParseError> {
        if self.peek().kind == kind {
            Ok(self.advance())
        } else {
            Err(ParseError::Unexpected(format!(
                "Expected {kind:?}, found {:?}.",
                self.peek().kind
            )))
        }
    }

    fn skip_separators(&mut self) {
        while matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            self.pos += 1;
        }
    }

    fn parse_program(&mut self) -> Result<(), ParseError> {
        loop {
            self.skip_separators();
            if self.peek().kind == TokenKind::Eof {
                return Ok(());
            }
            if self.looks_like_func_def() {
                self.parse_func_def()?;
            } else {
                let stmt = self.parse_stmt()?;
                self.ast.top_level.push(TopLevel::Stmt(stmt));
                match self.peek().kind {
                    TokenKind::Newline | TokenKind::Eof => {}
                    other => return Err(ParseError::Unexpected(format!("Unexpected token {other:?}."))),
                }
            }
        }
    }

    // A func-def is `Ident "(" … ")" [":" type] "is"`. The trailing `is` is what tells it
    // apart from a top-level call statement `f(args)`.
    fn looks_like_func_def(&self) -> bool {
        if self.peek().kind != TokenKind::Identifier || self.at(1) != TokenKind::LParen {
            return false;
        }
        let close = match self.matching_paren(self.pos + 1) {
            Some(i) => i,
            None => return false,
        };
        let mut j = close + 1;
        if self.toks.get(j).map(|t| t.kind) == Some(TokenKind::Colon) {
            j += 2; // ": type"
        }
        self.toks.get(j).map(|t| t.kind) == Some(TokenKind::KwIs)
    }

    // Index of the `)` that closes the `(` at `lparen`, found by counting nesting depth.
    // A non-consuming lookahead (it indexes `toks` directly, never advances `pos`).
    // Used by `looks_like_func_def` to skip past the parameter list and peek at what follows.
    // Returns `None` if the parens are unbalanced or `Eof` is hit first.
    fn matching_paren(&self, lparen: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut i = lparen;
        while i < self.toks.len() {
            match self.toks[i].kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                TokenKind::Eof => return None,
                _ => {}
            }
            i += 1;
        }
        None
    }

    fn parse_func_def(&mut self) -> Result<(), ParseError> {
        let name_tok = self.advance(); // Identifier
        let name = self.ast.intern(name_tok.text);
        self.expect(TokenKind::LParen)?;
        let params = self.parse_params()?;
        self.expect(TokenKind::RParen)?;

        let has_return_type = self.peek().kind == TokenKind::Colon;
        let return_type = if has_return_type {
            self.advance(); // ':'
            self.parse_type()?
        } else {
            PinpType::Void
        };
        self.expect(TokenKind::KwIs)?;

        // Reject duplicate parameter names — a structural check (no types needed).
        let mut seen = FxHashSet::default();
        for p in &params {
            if !seen.insert(p.name) {
                return Err(ParseError::Unexpected(format!(
                    "Duplicate parameter `{}`.",
                    self.ast.names[p.name.0 as usize]
                )));
            }
        }

        let body = self.parse_func_body(has_return_type)?;
        self.ast.top_level.push(TopLevel::Func(FuncDef {
            name,
            params,
            return_type,
            body,
        }));
        Ok(())
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if self.peek().kind == TokenKind::RParen {
            return Ok(params);
        }
        // The first parameter fixes the column that continuation lines must align to.
        let first = self.peek().clone();
        loop {
            let (line, col) = {
                let t = self.peek();
                (t.line, t.col)
            };
            if line != first.line && col != first.col {
                return Err(ParseError::Layout(format!(
                    "Parameter at line {line} col {col} must align to the first parameter's column {}.",
                    first.col
                )));
            }
            let name_tok = self.expect(TokenKind::Identifier)?;
            let name = self.ast.intern(name_tok.text);
            self.expect(TokenKind::Colon)?;
            let param_type = self.parse_type()?;
            params.push(Param { name, param_type });
            if self.peek().kind == TokenKind::Comma {
                self.advance();
            } else {
                break;
            }
        }
        Ok(params)
    }

    fn parse_type(&mut self) -> Result<PinpType, ParseError> {
        let tok = self.expect(TokenKind::Identifier)?;
        match tok.text {
            "int" => Ok(PinpType::Int),
            "float" => Ok(PinpType::Float),
            "void" => Ok(PinpType::Void),
            other => Err(ParseError::Unexpected(format!("Unknown type `{other}`."))),
        }
    }

    fn parse_func_body(&mut self, has_return_type: bool) -> Result<Block, ParseError> {
        if self.peek().kind == TokenKind::Newline {
            // Block form: a run of statements ending in a result expression.
            self.expect(TokenKind::Newline)?;
            self.expect(TokenKind::Indent)?;
            let mut lines: Vec<Stmt> = Vec::new();
            loop {
                let stmt = self.parse_stmt()?;
                lines.push(stmt);
                match self.peek().kind {
                    TokenKind::Newline => self.pos += 1,
                    TokenKind::Dedent => {
                        self.pos += 1;
                        break;
                    }
                    TokenKind::Eof => break,
                    other => return Err(ParseError::Unexpected(format!("Unexpected token {other:?}."))),
                }
                if self.peek().kind == TokenKind::Dedent {
                    self.pos += 1;
                    break;
                }
                if self.peek().kind == TokenKind::Eof {
                    break;
                }
            }
            let result = match lines.pop() {
                Some(Stmt::Expr(e)) => e,
                Some(Stmt::Assign { .. }) => {
                    return Err(ParseError::Unexpected(
                        "Function body must end with an expression.".into(),
                    ))
                }
                None => return Err(ParseError::Unexpected("Empty function body.".into())),
            };
            Ok(Block {
                stmts: lines,
                result,
            })
        } else {
            // Single-line form: `is` then an expression — which must declare a return type.
            if !has_return_type {
                return Err(ParseError::Unexpected(
                    "Single-line function must declare a return type.".into(),
                ));
            }
            let result = self.parse_expr(0)?;
            match self.peek().kind {
                TokenKind::Newline | TokenKind::Eof => {}
                other => return Err(ParseError::Unexpected(format!("Unexpected token {other:?}."))),
            }
            Ok(Block {
                stmts: Vec::new(),
                result,
            })
        }
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        // An assignment is a `place` (bare name or `::name`) immediately followed by an
        // assignment operator; anything else is an expression statement.
        let is_global = if self.peek().kind == TokenKind::Identifier
            && assign_op(self.at(1)).is_some()
        {
            Some(false)
        } else if self.peek().kind == TokenKind::ColonColon
            && self.at(1) == TokenKind::Identifier
            && assign_op(self.at(2)).is_some()
        {
            Some(true)
        } else {
            None
        };

        if let Some(global) = is_global {
            if global {
                self.advance(); // '::'
            }
            let name = self.advance().text; // Identifier
            let sym = self.ast.intern(name);
            let op = assign_op(self.advance().kind).unwrap();
            let rhs = self.parse_expr(0)?;
            let place = if global {
                Place::Global(sym)
            } else {
                Place::Local(sym)
            };
            Ok(self.finish_assign(place, op, rhs))
        } else {
            let e = self.parse_expr(0)?;
            Ok(Stmt::Expr(e))
        }
    }

    // Desugar `place <op>= e` into `place = place <op> e`. Purely structural — the binding and
    // type checks happen later in sema.
    fn finish_assign(&mut self, place: Place, op: AssignKind, rhs: ExprId) -> Stmt {
        match op {
            AssignKind::Plain => Stmt::Assign { target: place, rhs },
            AssignKind::Compound(binop) => {
                let read = self.ast.push(Node::from(place));
                let combined = self.ast.push(Node::Bin {
                    op: binop,
                    lhs: read,
                    rhs,
                });
                Stmt::Assign {
                    target: place,
                    rhs: combined,
                }
            }
        }
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<ExprId, ParseError> {
        let mut lhs = self.parse_prefix()?;
        while let Some((lbp, rbp, op)) = infix(self.peek().kind) {
            if lbp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_expr(rbp)?;
            lhs = self.ast.push(Node::Bin { op, lhs, rhs });
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<ExprId, ParseError> {
        if self.peek().kind == TokenKind::Minus {
            self.advance();
            let operand = self.parse_expr(UNARY_MINUS_BP)?;
            return Ok(self.ast.push(Node::Unary { op: UnOp::Neg, operand }));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<ExprId, ParseError> {
        match self.peek().kind {
            TokenKind::Int => {
                let v = parse_int(self.advance().text);
                Ok(self.ast.push(Node::Int(v)))
            }
            TokenKind::Float => {
                let v = parse_float(self.advance().text);
                Ok(self.ast.push(Node::Float(v)))
            }
            TokenKind::Identifier => {
                if self.at(1) == TokenKind::LParen {
                    return self.parse_call();
                }
                let name = self.advance().text;
                let sym = self.ast.intern(name);
                Ok(self.ast.push(Node::Var(sym)))
            }
            TokenKind::ColonColon => {
                self.advance(); // '::'
                let name = self.expect(TokenKind::Identifier)?.text;
                let sym = self.ast.intern(name);
                Ok(self.ast.push(Node::Global(sym)))
            }
            TokenKind::LParen => {
                self.advance();
                let e = self.parse_expr(0)?;
                self.expect(TokenKind::RParen)?;
                Ok(e)
            }
            other => Err(ParseError::Unexpected(format!("Unexpected token {other:?}."))),
        }
    }

    fn parse_call(&mut self) -> Result<ExprId, ParseError> {
        let name = self.advance().text; // Identifier
        let callee = self.ast.intern(name);
        self.expect(TokenKind::LParen)?;
        let mut args = Vec::new();
        if self.peek().kind != TokenKind::RParen {
            loop {
                args.push(self.parse_expr(0)?);
                match self.peek().kind {
                    TokenKind::Comma => {
                        self.advance();
                    }
                    TokenKind::RParen => break,
                    other => return Err(ParseError::Unexpected(format!("Unexpected token {other:?}."))),
                }
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(self.ast.push(Node::Call { callee, args }))
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
    use indoc::indoc;

    fn parse_ok(src: &str) -> Ast<'_> {
        parse(src).unwrap()
    }

    fn func<'a>(ast: &'a Ast, i: usize) -> &'a FuncDef {
        match &ast.top_level[i] {
            TopLevel::Func(f) => f,
            other => panic!("Top-level element {i} is not a function: {other:?}."),
        }
    }

    // ExprId of the last top-level statement's expression.
    fn root(ast: &Ast) -> ExprId {
        match ast.top_level.last().unwrap() {
            TopLevel::Stmt(Stmt::Expr(e)) => *e,
            TopLevel::Stmt(Stmt::Assign { rhs, .. }) => *rhs,
            TopLevel::Func(_) => panic!("Program ends in a function definition, not an expression."),
        }
    }

    // --- expression structure ------------------------------------------------------------

    #[test]
    fn precedence_mul_over_add() {
        let ast = parse_ok("2 + 3 * 4");
        let Node::Bin { op: BinOp::Add, lhs, rhs } = *ast.node(root(&ast)) else {
            panic!("Expected an Add node at the root.");
        };
        assert_eq!(*ast.node(lhs), Node::Int(2));
        assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Mul, .. }));
    }

    #[test]
    fn power_is_right_assoc() {
        let ast = parse_ok("2 ^ 2 ^ 3");
        let Node::Bin { op: BinOp::Pow, lhs, rhs } = *ast.node(root(&ast)) else {
            panic!("Expected a Pow node at the root.");
        };
        assert_eq!(*ast.node(lhs), Node::Int(2));
        assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Pow, .. }));
    }

    #[test]
    fn unary_minus_binds_below_power() {
        let ast = parse_ok("-2 ^ 2");
        let Node::Unary { op: UnOp::Neg, operand } = *ast.node(root(&ast)) else {
            panic!("Expected a unary Neg node at the root.");
        };
        assert!(matches!(ast.node(operand), Node::Bin { op: BinOp::Pow, .. }));
    }

    #[test]
    fn paren_grouping() {
        let ast = parse_ok("(2 + 3) * 4");
        let Node::Bin { op: BinOp::Mul, lhs, .. } = *ast.node(root(&ast)) else {
            panic!("Expected a Mul node at the root.");
        };
        assert!(matches!(ast.node(lhs), Node::Bin { op: BinOp::Add, .. }));
    }

    #[test]
    fn grouped_int_literal_value() {
        let ast = parse_ok("12_000_321");
        assert_eq!(*ast.node(root(&ast)), Node::Int(12_000_321));
    }

    #[test]
    fn compound_assign_desugars_to_read_and_op() {
        // `b += 1` becomes `b = (b + 1)`: an Assign whose rhs is a Bin reading the target.
        let ast = parse_ok("b = 1\nb += 1");
        let TopLevel::Stmt(Stmt::Assign { target: Place::Local(_), rhs }) =
            ast.top_level.last().unwrap()
        else {
            panic!("Expected a desugared local assignment.");
        };
        let Node::Bin { op: BinOp::Add, lhs, .. } = *ast.node(*rhs) else {
            panic!("Expected the rhs to be an Add.");
        };
        assert!(matches!(ast.node(lhs), Node::Var(_)));
    }

    // --- function & program structure ----------------------------------------------------

    #[test]
    fn single_line_function() {
        let ast = parse_ok("fu(a:float, b:float, c:float): float is b^2 - 4*a*c");
        let f = func(&ast, 0);
        assert_eq!(ast.names[f.name.0 as usize], "fu");
        assert_eq!(f.params.len(), 3);
        assert!(f.params.iter().all(|p| p.param_type == PinpType::Float));
        assert_eq!(f.return_type, PinpType::Float);
        assert!(f.body.stmts.is_empty());

        // Omitting the return type on the single-line form is an explicit, specific error —
        // it must name the missing return type, not fall back to a generic syntax message.
        let Err(err) = parse("fu(a: float) is a") else {
            panic!("Expected a missing-return-type error, but parsing succeeded.");
        };
        assert!(
            matches!(&err, ParseError::Unexpected(msg) if msg.contains("return type")),
            "Expected an explicit missing-return-type error, got {err:?}."
        );
    }

    #[test]
    fn block_function_with_local() {
        let ast = parse_ok(indoc! {"
            _fu_bar_baz_1(a:int, b:int): int is
                xx = a+b*b
                xx
        "});
        let f = func(&ast, 0);
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.return_type, PinpType::Int);
        assert_eq!(f.body.stmts.len(), 1);
        assert!(matches!(f.body.stmts[0], Stmt::Assign { .. }));
    }

    #[test]
    fn multiline_params_aligned() {
        // `bb` aligns under `aa` (both column 3).
        let ast = parse_ok("f(aa: float,\n  bb: float): float is aa^2 + bb^2");
        assert_eq!(func(&ast, 0).params.len(), 2);
    }

    #[test]
    fn block_body_trailing_call_shape() {
        // A block body's trailing expression `b + fu(b)` is a `Bin` whose rhs is a call.
        let ast = parse_ok(indoc! {"
            fu(a: int): int is a + 2
            bar(b: int): int is
                b + fu(b)
        "});
        let bar = func(&ast, 1);
        let Node::Bin { rhs, .. } = *ast.node(bar.body.result) else {
            panic!("Expected the body to end in a binary expression.");
        };
        assert!(matches!(ast.node(rhs), Node::Call { .. }));
    }

    #[test]
    fn global_compound_assign_desugars_to_global_target() {
        let ast = parse_ok(indoc! {"
            g = 10
            bump(a: int): int is
                ::g += 1
                a + ::g
        "});
        assert!(matches!(
            func(&ast, 1).body.stmts[0],
            Stmt::Assign { target: Place::Global(_), .. }
        ));
    }

    #[test]
    fn return_type_annotation_resolved() {
        let ast = parse_ok("f(a: int): float is a");
        assert_eq!(func(&ast, 0).return_type, PinpType::Float);
    }

    #[test]
    fn program_shape_with_several_top_level() {
        let ast = parse_ok(indoc! {"
            g = 100
            sq(x: int): int is x*x
            bump(a: int): int is
                ::g += 1
                a + ::g + sq(2)
            sq(3)
        "});
        assert_eq!(ast.top_level.len(), 4);
        assert!(matches!(ast.top_level[0], TopLevel::Stmt(_)));
        assert!(matches!(ast.top_level[1], TopLevel::Func(_)));
        assert!(matches!(ast.top_level[2], TopLevel::Func(_)));
        assert!(matches!(ast.top_level[3], TopLevel::Stmt(_)));
    }

    // --- syntactic / structural errors ---------------------------------------------------

    #[test]
    fn misaligned_param_is_error() {
        // `bb` is one column past `aa`.
        assert!(matches!(
            parse("f(aa: int,\n   bb: int): int is aa+bb"),
            Err(ParseError::Layout(_))
        ));
    }

    #[test]
    fn duplicate_param_is_error() {
        assert!(matches!(
            parse("f(a: int, a: int): int is a"),
            Err(ParseError::Unexpected(_))
        ));
    }

    #[test]
    fn unknown_type_name_is_error() {
        assert!(matches!(
            parse("f(a: blah): int is a"),
            Err(ParseError::Unexpected(_))
        ));
    }
}
