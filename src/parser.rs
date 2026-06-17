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
    Bool,
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
/// `Pow` is `^` (right-associative). `Div` (`/`) always yields `Float`. The comparisons
/// (`Eq`..`Ge`) yield `Bool`; the logicals (`And`/`Or`/`Xor`) take and yield `Bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    IntDiv,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Xor,
}

/// A unary (prefix) operator: arithmetic negation (`-`) or logical `not`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// An expression in the AST arena. Child expressions are referenced by [`ExprId`] index rather
/// than boxed, so a `Node` is a flat, `Clone` value and the whole tree lives in one `Vec`.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Int(i64),
    Float(f64),
    Bool(bool),
    Var(SymId),    // bare name: a parameter/local, or a top-level global
    Global(SymId), // `::name`
    Unary {
        op: UnOp,
        operand: ExprId,
    },
    Bin {
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    },
    Call {
        callee: SymId,
        args: Vec<ExprId>,
    },
    /// `if`/`elif`/`else` — one node for both the block form and the one-line ternary (which is a
    /// single arm plus a mandatory `els`). Each arm's value is its body's trailing expression; the
    /// node yields a value only when `els` is present and every branch ends in one (see sema).
    If {
        arms: Vec<IfArm>,
        els: Option<Block>,
    },
}

/// One `if`/`elif` arm: a condition and the body taken when it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct IfArm {
    pub cond: ExprId,
    pub body: Block,
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
    Assign {
        target: Place,
        rhs: ExprId,
    },
    Expr(ExprId),
    /// Pre-test loop: run `body` while `cond` holds.
    While {
        cond: ExprId,
        body: Block,
    },
    /// Post-test (do–while) loop: run `body`, then repeat while `cond` holds (`until` ⇒ repeat
    /// while `cond` is *false*).
    Loop {
        body: Block,
        cond: ExprId,
        until: bool,
    },
}

/// A function parameter: its interned name and resolved type.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: SymId,
    pub param_type: PinpType,
}

/// A block of statements with an optional trailing `result` expression — its value when used as
/// one (an `if` arm or a function body). A function body always carries a `result` (the parser
/// requires it); a control-flow body that ends in a statement has `result: None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub result: Option<ExprId>,
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
        pending_dedents: 0,
    };
    p.parse_program()?;
    Ok(p.ast)
}

struct Parser<'src> {
    toks: Vec<Token<'src>>,
    pos: usize,
    ast: Ast<'src>,
    // A wrapped one-line conditional (`… if c` then `else …` on the next line, aligned to the
    // expression's column) opens an `Indent` the layout will later close with a `Dedent`. That
    // `Dedent` is *not* a block terminator, so `parse_conditional` records it here and the
    // enclosing `parse_block` swallows it instead of ending the block. See `parse_conditional`.
    // At the top level there is no enclosing `parse_block`; the stray `Dedent` is harmlessly
    // absorbed by `skip_separators` and this counter is reset each `parse_program` iteration.
    pending_dedents: usize,
}

// Binding-power "table". See articles on Pratt parsing.
// Regarding performance, `match` is register-only and const-foldable; a static table would just add
// a load and an ordering footgun — no speedup.
fn infix(kind: TokenKind) -> Option<(u8, u8, BinOp)> {
    use BinOp::*;
    use TokenKind::*;
    Some(match kind {
        KwOr => (20, 21, Or),
        KwXor => (25, 26, Xor),
        KwAnd => (30, 31, And),
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

// The comparison operators (`< > <= >= == !=`) form a single non-associative band that *chains*
// (see `parse_comparison_chain`) rather than left-folding, so they live here, not in `infix`.
fn comparison_op(kind: TokenKind) -> Option<BinOp> {
    use TokenKind::*;
    Some(match kind {
        EqEq => BinOp::Eq,
        Ne => BinOp::Ne,
        Lt => BinOp::Lt,
        Gt => BinOp::Gt,
        Le => BinOp::Le,
        Ge => BinOp::Ge,
        _ => return None,
    })
}

// Binding power of the comparison band. Looser than additive (60), tighter than the logicals
// (`and`=30); chain operands are parsed just above it so a comparison never swallows another.
const COMPARISON_BP: u8 = 45;

// Binding power of the one-line conditional `e1 if c else e2`. The loosest operator of all — below
// `or` (20) — so `a or b if c else d` is `(a or b) if c else d`. Its else-tail is right-associative
// (the right operand is parsed at this same power), giving `a if p else b if q else c` =
// `a if p else (b if q else c)`. Handled apart from `infix`, like the comparison chain.
const CONDITIONAL_BP: u8 = 10;

// Right binding power of the prefix operators `-` and `not`: above `*`/`/` (70), below `^` (80),
// so `-a*b` is `(-a)*b` and `-a^b` is `-(a^b)`. `not` shares this level to match C's unary-tight
// `!`, so `not a == b` is `(not a) == b`.
const UNARY_MINUS_BP: u8 = 75;

// The "direction" of a comparison operator. A chain of more than one comparison must keep a single
// direction so the relation is transitive and reads across the chain (`a < b <= c`), as in maths.
// `!=` is non-transitive (`a != b != c` is not "all distinct"), so it has no direction and never
// chains.
#[derive(PartialEq)]
enum ChainDir {
    Ascending,  // < <=
    Descending, // > >=
    Equality,   // ==
}

fn chain_dir(op: BinOp) -> Option<ChainDir> {
    Some(match op {
        BinOp::Lt | BinOp::Le => ChainDir::Ascending,
        BinOp::Gt | BinOp::Ge => ChainDir::Descending,
        BinOp::Eq => ChainDir::Equality,
        _ => return None,
    })
}

fn cmp_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        _ => unreachable!("not a comparison operator"),
    }
}

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
            self.pending_dedents = 0;
            self.skip_separators();
            if self.peek().kind == TokenKind::Eof {
                return Ok(());
            }
            if self.looks_like_func_def() {
                self.parse_func_def()?;
            } else {
                let stmt = self.parse_stmt()?;
                self.ast.top_level.push(TopLevel::Stmt(stmt));
                // A single-line statement must reach end of line; a multiline construct (`while`/
                // `loop`/block `if`) ends on a layout token, after which the next statement may
                // begin straight away. Any stray continuation dedents are absorbed by
                // `skip_separators` on the next iteration. (`parse_block` applies the same
                // end-of-statement rule for nested blocks; keep the two in sync.)
                let ended_at_layout = matches!(
                    self.toks[self.pos - 1].kind,
                    TokenKind::Newline | TokenKind::Dedent | TokenKind::Indent
                );
                match self.peek().kind {
                    TokenKind::Newline | TokenKind::Eof | TokenKind::Dedent => {}
                    _ if ended_at_layout => {}
                    other => {
                        return Err(ParseError::Unexpected(format!(
                            "Unexpected token {other:?}."
                        )))
                    }
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
            "bool" => Ok(PinpType::Bool),
            "int" => Ok(PinpType::Int),
            "float" => Ok(PinpType::Float),
            "void" => Ok(PinpType::Void),
            other => Err(ParseError::Unexpected(format!("Unknown type `{other}`."))),
        }
    }

    fn parse_func_body(&mut self, has_return_type: bool) -> Result<Block, ParseError> {
        if self.peek().kind == TokenKind::Newline {
            // Block form: a run of statements ending in a result expression.
            let block = self.parse_block()?;
            if block.result.is_none() {
                return Err(ParseError::Unexpected(
                    "Function body must end with an expression.".into(),
                ));
            }
            Ok(block)
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
                other => {
                    return Err(ParseError::Unexpected(format!(
                        "Unexpected token {other:?}."
                    )))
                }
            }
            Ok(Block {
                stmts: Vec::new(),
                result: Some(result),
            })
        }
    }

    // Parse an indented block — `Newline Indent <stmts> Dedent` — shared by function bodies and
    // every control-flow body (`if`/`while`/`loop`). A trailing expression statement becomes the
    // block's `result` (its value when the block is used as one); a block ending in any other
    // statement has `result: None`.
    fn parse_block(&mut self) -> Result<Block, ParseError> {
        self.expect(TokenKind::Newline)?;
        self.expect(TokenKind::Indent)?;
        let mut stmts: Vec<Stmt> = Vec::new();
        loop {
            let stmt = self.parse_stmt()?;
            stmts.push(stmt);
            // A wrapped one-line conditional inside this statement opened indents whose closing
            // dedents sit just past the statement's newline; swallow them so they are not taken
            // for this block's terminator.
            if self.pending_dedents > 0 {
                if self.peek().kind == TokenKind::Newline {
                    self.pos += 1;
                }
                while self.pending_dedents > 0 && self.peek().kind == TokenKind::Dedent {
                    self.pos += 1;
                    self.pending_dedents -= 1;
                }
            }
            // A multiline construct (or a swallowed continuation) leaves us on a layout token, so
            // the next statement may follow with no separator of its own.
            let ended_at_layout = matches!(
                self.toks[self.pos - 1].kind,
                TokenKind::Newline | TokenKind::Dedent | TokenKind::Indent
            );
            match self.peek().kind {
                TokenKind::Newline => self.pos += 1,
                TokenKind::Dedent => {
                    self.pos += 1;
                    break;
                }
                TokenKind::Eof => break,
                _ if ended_at_layout => {}
                other => {
                    return Err(ParseError::Unexpected(format!(
                        "Unexpected token {other:?}."
                    )))
                }
            }
            if self.peek().kind == TokenKind::Dedent {
                self.pos += 1;
                break;
            }
            if self.peek().kind == TokenKind::Eof {
                break;
            }
        }
        // A trailing expression statement becomes the block's result value.
        let result = match stmts.last() {
            Some(Stmt::Expr(e)) => {
                let e = *e;
                stmts.pop();
                Some(e)
            }
            _ => None,
        };
        Ok(Block { stmts, result })
    }

    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        self.advance(); // 'while'
        let cond = self.parse_expr(0)?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_loop(&mut self) -> Result<Stmt, ParseError> {
        self.advance(); // 'loop'
        let body = self.parse_block()?;
        let until = match self.peek().kind {
            TokenKind::KwWhile => false,
            TokenKind::KwUntil => true,
            other => {
                return Err(ParseError::Unexpected(format!(
                    "Expected `while` or `until` after a loop body, found {other:?}."
                )))
            }
        };
        self.advance(); // 'while' / 'until'
        let cond = self.parse_expr(0)?;
        Ok(Stmt::Loop { body, cond, until })
    }

    // Block form `if cond <body> [elif cond <body>]* [else <body>]`, reached when `if` opens an
    // expression. Each arm and the optional `else` is an indented block; their values (the trailing
    // expressions) are what an `if`-as-a-value yields.
    fn parse_if(&mut self) -> Result<ExprId, ParseError> {
        self.advance(); // 'if'
        let cond = self.parse_expr(0)?;
        let body = self.parse_block()?;
        let mut arms = vec![IfArm { cond, body }];
        let mut els = None;
        loop {
            match self.peek().kind {
                TokenKind::KwElif => {
                    self.advance();
                    let cond = self.parse_expr(0)?;
                    let body = self.parse_block()?;
                    arms.push(IfArm { cond, body });
                }
                TokenKind::KwElse => {
                    self.advance();
                    els = Some(self.parse_block()?);
                    break;
                }
                _ => break,
            }
        }
        Ok(self.ast.push(Node::If { arms, els }))
    }

    // Tail of the one-line conditional `then_val if cond else else_val`, entered at `if` with
    // `then_val` already parsed. `start_col` is the column where the whole expression began; a
    // wrapped `else` (on its own line) must align to it.
    fn parse_conditional(
        &mut self,
        then_val: ExprId,
        start_col: u32,
    ) -> Result<ExprId, ParseError> {
        self.advance(); // 'if'
        let cond = self.parse_expr(0)?;
        // `else` may sit on the next line. Skip the continuation's layout tokens, recording any
        // `Indent` so the enclosing block ignores its later closing `Dedent`.
        let mut wrapped = false;
        while matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            if self.peek().kind == TokenKind::Indent {
                self.pending_dedents += 1;
            }
            wrapped = true;
            self.pos += 1;
        }
        if self.peek().kind != TokenKind::KwElse {
            return Err(ParseError::Unexpected(
                "Expected `else` in a conditional expression.".into(),
            ));
        }
        if wrapped && self.peek().col != start_col {
            return Err(ParseError::Layout(format!(
                "Continuation `else` at column {} must align to the conditional's column {}.",
                self.peek().col,
                start_col
            )));
        }
        self.advance(); // 'else'
        let else_val = self.parse_expr(CONDITIONAL_BP)?;
        let then_block = Block {
            stmts: Vec::new(),
            result: Some(then_val),
        };
        let else_block = Block {
            stmts: Vec::new(),
            result: Some(else_val),
        };
        Ok(self.ast.push(Node::If {
            arms: vec![IfArm {
                cond,
                body: then_block,
            }],
            els: Some(else_block),
        }))
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek().kind {
            TokenKind::KwWhile => return self.parse_while(),
            TokenKind::KwLoop => return self.parse_loop(),
            _ => {}
        }
        // An assignment is a `place` (bare name or `::name`) immediately followed by an
        // assignment operator; anything else is an expression statement.
        let is_global =
            if self.peek().kind == TokenKind::Identifier && assign_op(self.at(1)).is_some() {
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
        let start_col = self.peek().col;
        let mut lhs = self.parse_prefix()?;
        loop {
            // The one-line conditional `lhs if c else e` is the loosest operator and chains right;
            // like the comparison band it sits outside the `infix` table.
            if self.peek().kind == TokenKind::KwIf && CONDITIONAL_BP >= min_bp {
                lhs = self.parse_conditional(lhs, start_col)?;
                continue;
            }
            // The comparison band chains rather than left-folds, so it is handled apart from the
            // `infix` binding-power loop.
            if comparison_op(self.peek().kind).is_some() && COMPARISON_BP >= min_bp {
                lhs = self.parse_comparison_chain(lhs)?;
                continue;
            }
            let Some((lbp, rbp, op)) = infix(self.peek().kind) else {
                break;
            };
            if lbp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_expr(rbp)?;
            lhs = self.ast.push(Node::Bin { op, lhs, rhs });
        }
        Ok(lhs)
    }

    // Parse `first op1 e1 op2 e2 …` (`first` already parsed) and desugar it to the `and` of its
    // adjacent comparisons: `a < b < c` becomes `(a < b) and (b < c)`, with the middle operand
    // shared. A chain of one comparison is just that comparison — no `and` is introduced.
    fn parse_comparison_chain(&mut self, first: ExprId) -> Result<ExprId, ParseError> {
        let mut prev = first;
        let mut ops: Vec<BinOp> = Vec::new();
        let mut result: Option<ExprId> = None;
        while let Some(op) = comparison_op(self.peek().kind) {
            self.advance();
            // Operands bind just above the band so a following comparison starts a new link
            // rather than nesting inside this one.
            let next = self.parse_expr(COMPARISON_BP + 1)?;
            let cmp = self.ast.push(Node::Bin {
                op,
                lhs: prev,
                rhs: next,
            });
            result = Some(match result {
                None => cmp,
                Some(acc) => self.ast.push(Node::Bin {
                    op: BinOp::And,
                    lhs: acc,
                    rhs: cmp,
                }),
            });
            ops.push(op);
            prev = next;
        }
        check_monotonic(&ops)?;
        Ok(result.expect("a chain is only started when a comparison operator is next"))
    }

    fn parse_prefix(&mut self) -> Result<ExprId, ParseError> {
        let op = match self.peek().kind {
            TokenKind::Minus => UnOp::Neg,
            TokenKind::KwNot => UnOp::Not,
            _ => return self.parse_primary(),
        };
        self.advance();
        let operand = self.parse_expr(UNARY_MINUS_BP)?;
        Ok(self.ast.push(Node::Unary { op, operand }))
    }

    fn parse_primary(&mut self) -> Result<ExprId, ParseError> {
        match self.peek().kind {
            TokenKind::Int => {
                let v = parse_int(self.advance().text)?;
                Ok(self.ast.push(Node::Int(v)))
            }
            TokenKind::Float => {
                let v = parse_float(self.advance().text);
                Ok(self.ast.push(Node::Float(v)))
            }
            TokenKind::KwTrue => {
                self.advance();
                Ok(self.ast.push(Node::Bool(true)))
            }
            TokenKind::KwFalse => {
                self.advance();
                Ok(self.ast.push(Node::Bool(false)))
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
            // `if` opening an expression is the block form; the one-line ternary is reached from
            // the `parse_expr` loop instead, after a left operand.
            TokenKind::KwIf => self.parse_if(),
            TokenKind::LParen => {
                self.advance();
                let e = self.parse_expr(0)?;
                self.expect(TokenKind::RParen)?;
                Ok(e)
            }
            other => Err(ParseError::Unexpected(format!(
                "Unexpected token {other:?}."
            ))),
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
                    other => {
                        return Err(ParseError::Unexpected(format!(
                            "Unexpected token {other:?}."
                        )))
                    }
                }
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(self.ast.push(Node::Call { callee, args }))
    }
}

// Convert a lexically-valid integer literal to its value, rejecting anything that does not fit in
// an `i64` (a too-large decimal, an over-long hex/binary string, or an overflowing `mantissa*10^exp`)
// rather than panicking.
fn parse_int(text: &str) -> Result<i64, ParseError> {
    let out_of_range =
        || ParseError::Unexpected(format!("Integer literal `{text}` is out of range."));
    let s: String = text.chars().filter(|&c| c != '_').collect();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).map_err(|_| out_of_range());
    }
    if let Some(b) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        return i64::from_str_radix(b, 2).map_err(|_| out_of_range());
    }
    if let Some(pos) = s.find(['e', 'E']) {
        let mantissa: i64 = s[..pos].parse().map_err(|_| out_of_range())?;
        let exp: u32 = s[pos + 1..].parse().map_err(|_| out_of_range())?;
        return 10i64
            .checked_pow(exp)
            .and_then(|factor| mantissa.checked_mul(factor))
            .ok_or_else(out_of_range);
    }
    s.parse().map_err(|_| out_of_range())
}

fn parse_float(text: &str) -> f64 {
    let s: String = text.chars().filter(|&c| c != '_').collect();
    s.parse().unwrap()
}

// A chain of more than one comparison must be monotonic: every operator shares one direction
// (all ascending `< <=`, all descending `> >=`, or all `==`), and `!=` never chains. A lone
// comparison is always fine — including `!=`.
fn check_monotonic(ops: &[BinOp]) -> Result<(), ParseError> {
    if ops.len() < 2 {
        return Ok(());
    }
    let first = ops[0];
    let dir = chain_dir(first).ok_or_else(|| {
        ParseError::Unexpected(format!(
            "Cannot chain `{}`; it is not transitive.",
            cmp_symbol(first)
        ))
    })?;
    for &op in &ops[1..] {
        match chain_dir(op) {
            None => {
                return Err(ParseError::Unexpected(format!(
                    "Cannot chain `{}`; it is not transitive.",
                    cmp_symbol(op)
                )))
            }
            Some(d) if d != dir => {
                return Err(ParseError::Unexpected(format!(
                    "Cannot chain `{}` with `{}`.",
                    cmp_symbol(first),
                    cmp_symbol(op)
                )))
            }
            Some(_) => {}
        }
    }
    Ok(())
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
            other => panic!("Program does not end in an expression: {other:?}."),
        }
    }

    // --- expression structure ------------------------------------------------------------

    #[test]
    fn precedence_mul_over_add() {
        let ast = parse_ok("2 + 3 * 4");
        let Node::Bin {
            op: BinOp::Add,
            lhs,
            rhs,
        } = *ast.node(root(&ast))
        else {
            panic!("Expected an Add node at the root.");
        };
        assert_eq!(*ast.node(lhs), Node::Int(2));
        assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Mul, .. }));
    }

    #[test]
    fn power_is_right_assoc() {
        let ast = parse_ok("2 ^ 2 ^ 3");
        let Node::Bin {
            op: BinOp::Pow,
            lhs,
            rhs,
        } = *ast.node(root(&ast))
        else {
            panic!("Expected a Pow node at the root.");
        };
        assert_eq!(*ast.node(lhs), Node::Int(2));
        assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Pow, .. }));
    }

    #[test]
    fn unary_minus_binds_below_power() {
        let ast = parse_ok("-2 ^ 2");
        let Node::Unary {
            op: UnOp::Neg,
            operand,
        } = *ast.node(root(&ast))
        else {
            panic!("Expected a unary Neg node at the root.");
        };
        assert!(matches!(
            ast.node(operand),
            Node::Bin { op: BinOp::Pow, .. }
        ));
    }

    #[test]
    fn paren_grouping() {
        let ast = parse_ok("(2 + 3) * 4");
        let Node::Bin {
            op: BinOp::Mul,
            lhs,
            ..
        } = *ast.node(root(&ast))
        else {
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
        let TopLevel::Stmt(Stmt::Assign {
            target: Place::Local(_),
            rhs,
        }) = ast.top_level.last().unwrap()
        else {
            panic!("Expected a desugared local assignment.");
        };
        let Node::Bin {
            op: BinOp::Add,
            lhs,
            ..
        } = *ast.node(*rhs)
        else {
            panic!("Expected the rhs to be an Add.");
        };
        assert!(matches!(ast.node(lhs), Node::Var(_)));
    }

    // --- bool literals, logical & comparison operators -----------------------------------

    #[test]
    fn bool_literals() {
        let t = parse_ok("true");
        assert_eq!(*t.node(root(&t)), Node::Bool(true));
        let f = parse_ok("false");
        assert_eq!(*f.node(root(&f)), Node::Bool(false));
    }

    #[test]
    fn logical_precedence_and_xor_or() {
        // `and` (30) > `xor` (25) > `or` (20): `a and b or a xor b` = `(a and b) or (a xor b)`.
        let ast = parse_ok("a and b or a xor b");
        let Node::Bin {
            op: BinOp::Or,
            lhs,
            rhs,
        } = *ast.node(root(&ast))
        else {
            panic!("Expected an Or at the root.");
        };
        assert!(matches!(ast.node(lhs), Node::Bin { op: BinOp::And, .. }));
        assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Xor, .. }));
    }

    #[test]
    fn not_is_unary_tight() {
        // C precedence: `not` binds tighter than every infix op, so `not a == b` = `(not a) == b`.
        let ast = parse_ok("not a == b");
        let Node::Bin {
            op: BinOp::Eq, lhs, ..
        } = *ast.node(root(&ast))
        else {
            panic!("Expected an Eq at the root.");
        };
        assert!(matches!(ast.node(lhs), Node::Unary { op: UnOp::Not, .. }));
    }

    #[test]
    fn not_chains() {
        let ast = parse_ok("not not a");
        let Node::Unary {
            op: UnOp::Not,
            operand,
        } = *ast.node(root(&ast))
        else {
            panic!("Expected a Not at the root.");
        };
        assert!(matches!(
            ast.node(operand),
            Node::Unary { op: UnOp::Not, .. }
        ));
    }

    #[test]
    fn comparison_binds_looser_than_arithmetic() {
        // `a + b < c` = `(a + b) < c`.
        let ast = parse_ok("a + b < c");
        let Node::Bin {
            op: BinOp::Lt, lhs, ..
        } = *ast.node(root(&ast))
        else {
            panic!("Expected a Lt at the root.");
        };
        assert!(matches!(ast.node(lhs), Node::Bin { op: BinOp::Add, .. }));
    }

    #[test]
    fn lone_comparison_has_no_and() {
        let ast = parse_ok("a > b");
        assert!(matches!(
            ast.node(root(&ast)),
            Node::Bin { op: BinOp::Gt, .. }
        ));
    }

    #[test]
    fn chained_comparison_desugars_with_shared_operand() {
        // `a < b < c` => `(a < b) and (b < c)`, with the middle `b` shared (same ExprId).
        let ast = parse_ok("a < b < c");
        let Node::Bin {
            op: BinOp::And,
            lhs,
            rhs,
        } = *ast.node(root(&ast))
        else {
            panic!("Expected an And at the root.");
        };
        let Node::Bin {
            op: BinOp::Lt,
            rhs: left_b,
            ..
        } = *ast.node(lhs)
        else {
            panic!("Expected the first comparison to be a Lt.");
        };
        let Node::Bin {
            op: BinOp::Lt,
            lhs: right_b,
            ..
        } = *ast.node(rhs)
        else {
            panic!("Expected the second comparison to be a Lt.");
        };
        assert_eq!(
            left_b, right_b,
            "the shared middle operand must reuse one ExprId"
        );
    }

    #[test]
    fn monotonic_chains_parse() {
        parse_ok("a <= b < c");
        parse_ok("d >= e >= f > g");
        parse_ok("a == b == c");
    }

    #[test]
    fn non_monotonic_chains_are_errors() {
        assert!(matches!(parse("a < b > c"), Err(ParseError::Unexpected(_))));
        assert!(matches!(
            parse("a <= b == c"),
            Err(ParseError::Unexpected(_))
        ));
        assert!(matches!(
            parse("a != b != c"),
            Err(ParseError::Unexpected(_))
        ));
    }

    // --- control flow: `if` expression, `while`/`loop` statements ------------------------

    #[test]
    fn ternary_binds_below_or() {
        // `a or b if c else d` = `(a or b) if c else d`.
        let ast = parse_ok("a or b if c else d");
        let Node::If { arms, els } = ast.node(root(&ast)) else {
            panic!("Expected an If at the root.");
        };
        assert_eq!(arms.len(), 1);
        let then = arms[0].body.result.unwrap();
        assert!(matches!(ast.node(then), Node::Bin { op: BinOp::Or, .. }));
        assert!(els.is_some());
    }

    #[test]
    fn ternary_else_tail_is_right_associative() {
        // `a if p else b if q else c` = `a if p else (b if q else c)`.
        let ast = parse_ok("a if p else b if q else c");
        let Node::If { els, .. } = ast.node(root(&ast)) else {
            panic!("Expected an If at the root.");
        };
        let tail = els.as_ref().unwrap().result.unwrap();
        assert!(matches!(ast.node(tail), Node::If { .. }));
    }

    #[test]
    fn wrapped_conditional_alignment() {
        // The `else` line must align to the column where the then-expression (`42`) starts.
        parse_ok("fu = 42 + 142 if a > 42\n     else 42");
        assert!(matches!(
            parse("fu = 42 + 142 if a > 42\n   else 42"),
            Err(ParseError::Layout(_))
        ));
    }

    #[test]
    fn block_if_shape() {
        let ast = parse_ok(indoc! {"
            if a > b
                a
            elif a == b
                b
            else
                c
        "});
        let Node::If { arms, els } = ast.node(root(&ast)) else {
            panic!("Expected an If at the root.");
        };
        assert_eq!(arms.len(), 2); // `if` + one `elif`
        assert!(els.is_some());
        assert!(arms[0].body.result.is_some());
    }

    #[test]
    fn block_if_as_function_result() {
        let ast = parse_ok(indoc! {"
            mx(a: int, b: int): int is
                if a > b
                    a
                else
                    b
        "});
        let f = func(&ast, 0);
        assert!(matches!(ast.node(f.body.result.unwrap()), Node::If { .. }));
    }

    #[test]
    fn while_and_loop_shapes() {
        let w = parse_ok(indoc! {"
            while a > 0
                a
        "});
        assert!(matches!(w.top_level[0], TopLevel::Stmt(Stmt::While { .. })));

        let l = parse_ok(indoc! {"
            loop
                a
            while a > 0
        "});
        assert!(matches!(
            l.top_level[0],
            TopLevel::Stmt(Stmt::Loop { until: false, .. })
        ));

        let u = parse_ok(indoc! {"
            loop
                a
            until a > 0
        "});
        assert!(matches!(
            u.top_level[0],
            TopLevel::Stmt(Stmt::Loop { until: true, .. })
        ));
    }

    #[test]
    fn body_ending_in_statement_has_no_result() {
        let ast = parse_ok(indoc! {"
            while a > 0
                b = 1
        "});
        let TopLevel::Stmt(Stmt::While { body, .. }) = &ast.top_level[0] else {
            panic!("Expected a while statement.");
        };
        assert_eq!(body.stmts.len(), 1);
        assert!(body.result.is_none());
    }

    #[test]
    fn loop_without_trailing_condition_is_error() {
        assert!(matches!(
            parse("loop\n    a\nb"),
            Err(ParseError::Unexpected(_))
        ));
    }

    #[test]
    fn bool_type_annotation() {
        let ast = parse_ok("f(a: bool): bool is a");
        assert_eq!(func(&ast, 0).params[0].param_type, PinpType::Bool);
        assert_eq!(func(&ast, 0).return_type, PinpType::Bool);
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
        let Node::Bin { rhs, .. } = *ast.node(bar.body.result.unwrap()) else {
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
            Stmt::Assign {
                target: Place::Global(_),
                ..
            }
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

    #[test]
    fn integer_literal_overflow_is_error() {
        // Lexically valid but out of i64 range, in every base — rejected, not panicked.
        assert!(matches!(
            parse("99999999999999999999"),
            Err(ParseError::Unexpected(_))
        )); // decimal > i64::MAX
        assert!(matches!(
            parse("0xFFFF_FFFF_FFFF_FFFF_F"),
            Err(ParseError::Unexpected(_))
        )); // 68-bit hex
        assert!(matches!(parse("9E99"), Err(ParseError::Unexpected(_)))); // mantissa*10^exp overflows
        let wide_binary = format!("0b{}", "1".repeat(65));
        assert!(matches!(
            parse(&wide_binary),
            Err(ParseError::Unexpected(_))
        )); // 65-bit binary
    }

    #[test]
    fn max_int_literal_is_accepted() {
        let ast = parse_ok("9223372036854775807"); // i64::MAX
        assert_eq!(*ast.node(root(&ast)), Node::Int(i64::MAX));
    }
}
