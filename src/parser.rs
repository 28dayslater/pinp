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
//! **Scoping.** Binding/type resolution uses a scope stack: `scopes[0]` is global, and a
//! function pushes a frame seeded with its parameters. A bare identifier resolves against the
//! innermost frame only — inside a function it never reaches globals; a global is reached
//! explicitly via `::name` ([`Node::Global`]). Functions are registered in a signature table
//! *after* their body is parsed, giving define-before-use with no recursion.
//!
//! Type inference runs *inline* during parsing: every node gets its `PinpType` as it is built,
//! and type errors surface immediately as [`PinpError`].

use crate::lexer::{lex, Token, TokenKind};
use rustc_hash::FxHashMap;

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

/// A failure from lexing, parsing, or type checking. pinp is fail-fast: the first error returned
/// stops the pass — there is no error recovery or multi-error collection.
#[derive(Debug, Clone, PartialEq)]
pub enum PinpError {
    /// A lexing failure (carries the [`crate::lexer::LexError`] message).
    Lex(String),
    /// A syntax error: an unexpected or missing token.
    Unexpected(String),
    /// A name used but not bound: unknown variable, global, or function.
    UnknownSymbol(String),
    /// A type error: a mismatch, a bad promotion, or an operation on `Void`.
    Type(String),
    /// An indentation or parameter-alignment error.
    Layout(String),
}

/// A parsed program. Borrows the source for the lifetime `'src` (identifiers are slices into it).
///
/// Layout: `nodes` and `types` are parallel arenas indexed by [`ExprId`]; `top_level` is the
/// program in source order; `names` maps a [`SymId`] back to its text. Returned by [`parse`].
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

    /// The inferred type of the node at `e`.
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

/// Lexes and parses `src` into an [`Ast`], inferring every node's type inline. The returned AST
/// borrows `src`. Stops at and returns the first [`PinpError`].
pub fn parse(src: &str) -> Result<Ast<'_>, PinpError> {
    let toks = lex(src).map_err(|e| PinpError::Lex(e.message))?;
    let mut p = Parser {
        toks,
        pos: 0,
        ast: Ast::default(),
        scopes: vec![FxHashMap::default()], // global frame
        funcs: FxHashMap::default(),
    };
    p.parse_program()?;
    Ok(p.ast)
}

#[derive(Clone)]
struct Signature {
    params: Vec<PinpType>,
    return_type: PinpType,
}

struct Parser<'src> {
    toks: Vec<Token<'src>>,
    pos: usize,
    ast: Ast<'src>,
    scopes: Vec<FxHashMap<SymId, PinpType>>,
    funcs: FxHashMap<SymId, Signature>,
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

/// `from` is assignable to `to` if identical, or via the one allowed promotion `Int -> Float`.
fn assignable(from: PinpType, to: PinpType) -> bool {
    from == to || (from == PinpType::Int && to == PinpType::Float)
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

    fn expect(&mut self, kind: TokenKind) -> Result<Token<'src>, PinpError> {
        if self.peek().kind == kind {
            Ok(self.advance())
        } else {
            Err(PinpError::Unexpected(format!(
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

    fn parse_program(&mut self) -> Result<(), PinpError> {
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
                    other => return Err(PinpError::Unexpected(format!("Unexpected token {other:?}."))),
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

    fn parse_func_def(&mut self) -> Result<(), PinpError> {
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

        // Seed a fresh local frame with the parameters.
        let mut frame: FxHashMap<SymId, PinpType> = FxHashMap::default();
        for p in &params {
            if frame.insert(p.name, p.param_type).is_some() {
                return Err(PinpError::Type(format!(
                    "Duplicate parameter `{}`.",
                    self.ast.names[p.name.0 as usize]
                )));
            }
        }
        self.scopes.push(frame);
        let body = self.parse_func_body(has_return_type)?;
        self.scopes.pop();

        let result_type = self.ast.type_of(body.result);
        if !assignable(result_type, return_type) {
            return Err(PinpError::Type(format!(
                "Function `{}` body yields {result_type:?} but is declared {return_type:?}.",
                name_tok.text
            )));
        }

        // Register only now: a call can reach a function only after its definition (so no
        // forward references and no recursion).
        self.funcs.insert(
            name,
            Signature {
                params: params.iter().map(|p| p.param_type).collect(),
                return_type,
            },
        );
        self.ast.top_level.push(TopLevel::Func(FuncDef {
            name,
            params,
            return_type,
            body,
        }));
        Ok(())
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, PinpError> {
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
                return Err(PinpError::Layout(format!(
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

    fn parse_type(&mut self) -> Result<PinpType, PinpError> {
        let tok = self.expect(TokenKind::Identifier)?;
        match tok.text {
            "int" => Ok(PinpType::Int),
            "float" => Ok(PinpType::Float),
            "void" => Ok(PinpType::Void),
            other => Err(PinpError::Type(format!("Unknown type `{other}`."))),
        }
    }

    fn parse_func_body(&mut self, has_return_type: bool) -> Result<Block, PinpError> {
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
                    other => return Err(PinpError::Unexpected(format!("Unexpected token {other:?}."))),
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
                    return Err(PinpError::Type(
                        "Function body must end with an expression.".into(),
                    ))
                }
                None => return Err(PinpError::Unexpected("Empty function body.".into())),
            };
            Ok(Block {
                stmts: lines,
                result,
            })
        } else {
            // Single-line form: `is` then an expression — which must declare a return type.
            if !has_return_type {
                return Err(PinpError::Type(
                    "Single-line function must declare a return type.".into(),
                ));
            }
            let result = self.parse_expr(0)?;
            match self.peek().kind {
                TokenKind::Newline | TokenKind::Eof => {}
                other => return Err(PinpError::Unexpected(format!("Unexpected token {other:?}."))),
            }
            Ok(Block {
                stmts: Vec::new(),
                result,
            })
        }
    }

    fn parse_stmt(&mut self) -> Result<Stmt, PinpError> {
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
            self.finish_assign(place, op, rhs, name)
        } else {
            let e = self.parse_expr(0)?;
            Ok(Stmt::Expr(e))
        }
    }

    fn finish_assign(
        &mut self,
        place: Place,
        op: AssignKind,
        rhs: ExprId,
        name: &str,
    ) -> Result<Stmt, PinpError> {
        let existing = self.lookup_place(place);
        match op {
            AssignKind::Plain => {
                // Globals are created at top level via a bare name; `::name` only refers to an
                // existing global.
                if matches!(place, Place::Global(_)) && existing.is_none() {
                    return Err(PinpError::UnknownSymbol(format!("Unknown global `::{name}`.")));
                }
                let rhs_type = self.ast.type_of(rhs);
                if rhs_type == PinpType::Void {
                    return Err(PinpError::Type("Cannot assign a void value.".into()));
                }
                self.bind_place(place, rhs_type);
                Ok(Stmt::Assign { target: place, rhs })
            }
            AssignKind::Compound(binop) => {
                let place_type = existing.ok_or_else(|| {
                    PinpError::UnknownSymbol(format!("Compound assignment to unbound `{name}`."))
                })?;
                let read = self.ast.push(Node::from(place), place_type);
                let combined = self.make_bin(binop, read, rhs)?;
                let result_type = self.ast.type_of(combined);
                if !assignable(result_type, place_type) {
                    return Err(PinpError::Type(format!(
                        "Compound assignment yields {result_type:?}, not assignable to {place_type:?}."
                    )));
                }
                Ok(Stmt::Assign {
                    target: place,
                    rhs: combined,
                })
            }
        }
    }

    fn lookup_place(&self, place: Place) -> Option<PinpType> {
        match place {
            Place::Local(s) => self.scopes.last().unwrap().get(&s).copied(),
            Place::Global(s) => self.scopes[0].get(&s).copied(),
        }
    }

    fn bind_place(&mut self, place: Place, ty: PinpType) {
        match place {
            Place::Local(s) => {
                self.scopes.last_mut().unwrap().insert(s, ty);
            }
            Place::Global(s) => {
                self.scopes[0].insert(s, ty);
            }
        }
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<ExprId, PinpError> {
        let mut lhs = self.parse_prefix()?;
        while let Some((lbp, rbp, op)) = infix(self.peek().kind) {
            if lbp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_expr(rbp)?;
            lhs = self.make_bin(op, lhs, rhs)?;
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Result<ExprId, PinpError> {
        if self.peek().kind == TokenKind::Minus {
            self.advance();
            let operand = self.parse_expr(UNARY_MINUS_BP)?;
            let operand_type = self.ast.type_of(operand);
            if operand_type == PinpType::Void {
                return Err(PinpError::Type("Unary minus on a void value.".into()));
            }
            return Ok(self
                .ast
                .push(Node::Unary { op: UnOp::Neg, operand }, operand_type));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<ExprId, PinpError> {
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
                if self.at(1) == TokenKind::LParen {
                    return self.parse_call();
                }
                let name = self.advance().text;
                let resolved = self
                    .ast
                    .syms
                    .get(name)
                    .copied()
                    .and_then(|s| self.lookup_local(s).map(|t| (s, t)));
                match resolved {
                    Some((s, t)) => Ok(self.ast.push(Node::Var(s), t)),
                    None => Err(PinpError::UnknownSymbol(format!("Unknown symbol `{name}`."))),
                }
            }
            TokenKind::ColonColon => {
                self.advance(); // '::'
                let name = self.expect(TokenKind::Identifier)?.text;
                let resolved = self
                    .ast
                    .syms
                    .get(name)
                    .copied()
                    .and_then(|s| self.lookup_global(s).map(|t| (s, t)));
                match resolved {
                    Some((s, t)) => Ok(self.ast.push(Node::Global(s), t)),
                    None => Err(PinpError::UnknownSymbol(format!("Unknown global `::{name}`."))),
                }
            }
            TokenKind::LParen => {
                self.advance();
                let e = self.parse_expr(0)?;
                self.expect(TokenKind::RParen)?;
                Ok(e)
            }
            other => Err(PinpError::Unexpected(format!("Unexpected token {other:?}."))),
        }
    }

    fn parse_call(&mut self) -> Result<ExprId, PinpError> {
        let name = self.advance().text; // Identifier
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
                    other => return Err(PinpError::Unexpected(format!("Unexpected token {other:?}."))),
                }
            }
        }
        self.expect(TokenKind::RParen)?;

        let sym = self.ast.syms.get(name).copied();
        let sig = match sym.and_then(|s| self.funcs.get(&s).cloned()) {
            Some(sig) => sig,
            None => {
                return Err(PinpError::UnknownSymbol(format!(
                    "Call to undefined function `{name}`."
                )))
            }
        };
        if args.len() != sig.params.len() {
            return Err(PinpError::Type(format!(
                "Function `{name}` expects {} argument(s), got {}.",
                sig.params.len(),
                args.len()
            )));
        }
        for (i, (&arg, &pt)) in args.iter().zip(sig.params.iter()).enumerate() {
            let at = self.ast.type_of(arg);
            if !assignable(at, pt) {
                return Err(PinpError::Type(format!(
                    "Argument {} of `{name}`: {at:?} not assignable to {pt:?}.",
                    i + 1
                )));
            }
        }
        let callee = sym.unwrap();
        Ok(self.ast.push(Node::Call { callee, args }, sig.return_type))
    }

    fn lookup_local(&self, s: SymId) -> Option<PinpType> {
        self.scopes.last().unwrap().get(&s).copied()
    }

    fn lookup_global(&self, s: SymId) -> Option<PinpType> {
        self.scopes[0].get(&s).copied()
    }

    fn make_bin(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId) -> Result<ExprId, PinpError> {
        use PinpType::*;
        let (left_type, right_type) = (self.ast.type_of(lhs), self.ast.type_of(rhs));
        if left_type == Void || right_type == Void {
            return Err(PinpError::Type("Arithmetic on a void value.".into()));
        }
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
                    return Err(PinpError::Type(format!("{op:?} requires Int operands.")));
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

    fn ast_type(src: &str) -> PinpType {
        let ast = parse_ok(src);
        ast.type_of(root(&ast))
    }

    // --- iteration 1 (expressions) -------------------------------------------------------

    #[test]
    fn precedence_mul_over_add() {
        let ast = parse_ok("2 + 3 * 4");
        let r = root(&ast);
        let Node::Bin { op: BinOp::Add, lhs, rhs } = *ast.node(r) else {
            panic!("Expected an Add node at the root.");
        };
        assert_eq!(*ast.node(lhs), Node::Int(2));
        assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Mul, .. }));
        assert_eq!(ast.type_of(r), PinpType::Int);
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
    fn type_inference_rules() {
        assert_eq!(ast_type("2 + 3"), PinpType::Int);
        assert_eq!(ast_type("10 / 4"), PinpType::Float);
        assert_eq!(ast_type("10 div 4"), PinpType::Int);
        assert_eq!(ast_type("7 mod 3"), PinpType::Int);
        assert_eq!(ast_type("2 ^ 10"), PinpType::Int);
        assert_eq!(ast_type("2.0 ^ 10"), PinpType::Float);
        assert_eq!(ast_type("-3.14"), PinpType::Float);
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

    // --- iteration 2 (functions) ---------------------------------------------------------

    #[test]
    fn single_line_function() {
        let ast = parse_ok("fu(a:float, b:float, c:float): float is b^2 - 4*a*c");
        let f = func(&ast, 0);
        assert_eq!(ast.names[f.name.0 as usize], "fu");
        assert_eq!(f.params.len(), 3);
        assert!(f.params.iter().all(|p| p.param_type == PinpType::Float));
        assert_eq!(f.return_type, PinpType::Float);
        assert!(f.body.stmts.is_empty());
        assert_eq!(ast.type_of(f.body.result), PinpType::Float);

        // Omitting the return type on the single-line form is an explicit, specific error —
        // it must name the missing return type, not fall back to a generic syntax message.
        let Err(err) = parse("fu(a: float) is a") else {
            panic!("Expected a missing-return-type error, but parsing succeeded.");
        };
        assert!(
            matches!(&err, PinpError::Type(msg) if msg.contains("return type")),
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
        assert_eq!(ast.type_of(f.body.result), PinpType::Int);
    }

    #[test]
    fn multiline_params_aligned() {
        // `bb` aligns under `aa` (both column 3).
        let ast = parse_ok("f(aa: float,\n  bb: float): float is aa^2 + bb^2");
        let f = func(&ast, 0);
        assert_eq!(f.params.len(), 2);
        assert_eq!(ast.type_of(f.body.result), PinpType::Float);
    }

    #[test]
    fn misaligned_param_is_error() {
        // `bb` is one column past `aa`.
        assert!(matches!(
            parse("f(aa: int,\n   bb: int): int is aa+bb"),
            Err(PinpError::Layout(_))
        ));
    }

    #[test]
    fn call_typechecks() {
        let ast = parse_ok(indoc! {"
            sq(x: int): int is x*x
            sq(5)
        "});
        let r = root(&ast);
        assert_eq!(ast.type_of(r), PinpType::Int);
        assert!(matches!(ast.node(r), Node::Call { .. }));
    }

    #[test]
    fn call_arg_promotes_int_to_float() {
        let ast = parse_ok(indoc! {"
            f(x: float): float is x
            f(3)
        "});
        assert_eq!(ast.type_of(root(&ast)), PinpType::Float);
    }

    #[test]
    fn call_arity_mismatch_is_error() {
        assert!(matches!(
            parse(indoc! {"
                sq(x: int): int is x*x
                sq(1, 2)
            "}),
            Err(PinpError::Type(_))
        ));
    }

    #[test]
    fn call_arg_type_mismatch_is_error() {
        assert!(matches!(
            parse(indoc! {"
                sq(x: int): int is x*x
                sq(1.5)
            "}),
            Err(PinpError::Type(_))
        ));
    }

    #[test]
    fn call_before_definition_is_error() {
        assert!(matches!(
            parse(indoc! {"
                foo(1)
                foo(x: int): int is x
            "}),
            Err(PinpError::UnknownSymbol(_))
        ));
    }

    #[test]
    fn block_body_calls_earlier_single_line_function() {
        let ast = parse_ok(indoc! {"
            fu(a: int): int is a + 2
            bar(b: int): int is
                b + fu(b)
        "});
        let bar = func(&ast, 1);
        assert_eq!(ast.names[bar.name.0 as usize], "bar");
        assert_eq!(bar.return_type, PinpType::Int);
        // The trailing expression `b + fu(b)` is an Int-typed `Bin` whose rhs calls `fu`.
        let Node::Bin { rhs, .. } = *ast.node(bar.body.result) else {
            panic!("Expected the body to end in a binary expression.");
        };
        assert!(matches!(ast.node(rhs), Node::Call { .. }));
        assert_eq!(ast.type_of(bar.body.result), PinpType::Int);
    }

    #[test]
    fn global_access_and_compound_assign() {
        let ast = parse_ok(indoc! {"
            g = 10
            bump(a: int): int is
                ::g += 1
                a + ::g
        "});
        let f = func(&ast, 1);
        assert!(matches!(
            f.body.stmts[0],
            Stmt::Assign { target: Place::Global(_), .. }
        ));
        assert_eq!(ast.type_of(f.body.result), PinpType::Int);
    }

    #[test]
    fn bare_name_does_not_see_global() {
        assert!(matches!(
            parse(indoc! {"
                g = 10
                f(a: int): int is a + g
            "}),
            Err(PinpError::UnknownSymbol(_))
        ));
    }

    #[test]
    fn compound_assign_local() {
        let ast = parse_ok(indoc! {"
            f(a: int): int is
                b = a
                b += 1
                b
        "});
        assert_eq!(ast.type_of(func(&ast, 0).body.result), PinpType::Int);
    }

    #[test]
    fn compound_div_breaks_int_place() {
        // `b` is Int; `b /= 2` yields Float, which is not assignable back to Int.
        assert!(matches!(
            parse(indoc! {"
                f(a: int): int is
                    b = a
                    b /= 2
                    b
            "}),
            Err(PinpError::Type(_))
        ));
    }

    #[test]
    fn void_function_with_value_body_is_error() {
        assert!(matches!(
            parse(indoc! {"
                f(a: int) is
                    a + 1
            "}),
            Err(PinpError::Type(_))
        ));
    }

    #[test]
    fn return_type_promotes_int_to_float() {
        let ast = parse_ok("f(a: int): float is a");
        assert_eq!(func(&ast, 0).return_type, PinpType::Float);
    }

    #[test]
    fn return_float_to_int_is_error() {
        assert!(matches!(
            parse("f(a: float): int is a"),
            Err(PinpError::Type(_))
        ));
    }

    #[test]
    fn duplicate_param_is_error() {
        assert!(matches!(
            parse("f(a: int, a: int): int is a"),
            Err(PinpError::Type(_))
        ));
    }

    #[test]
    fn unknown_type_name_is_error() {
        assert!(matches!(
            parse("f(a: blah): int is a"),
            Err(PinpError::Type(_))
        ));
    }

    #[test]
    fn program_with_several_top_level() {
        // global, two functions (bump calls the earlier sq), then a top-level call
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
        assert_eq!(ast.type_of(root(&ast)), PinpType::Int);
    }
}
