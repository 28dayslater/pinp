// SPDX-License-Identifier: MIT

use super::*;
use crate::lexer::TokenKind;
use rustc_hash::FxHashSet;

// The binary operator behind a compound-assignment token (`+=` -> `Add`, …). Plain `=` is not a
// compound op and is handled directly. Compound assignment is single-target, single-value.
fn compound_assign_op(kind: TokenKind) -> Option<BinOp> {
    use TokenKind::*;
    Some(match kind {
        PlusEq => BinOp::Add,
        MinusEq => BinOp::Sub,
        StarEq => BinOp::Mul,
        SlashEq => BinOp::Div,
        CaretEq => BinOp::Pow,
        DivEq => BinOp::IntDiv,
        ModEq => BinOp::Mod,
        _ => return None,
    })
}

impl<'src> Parser<'src> {
    // -------------------------------------------------------------------------
    // Program structure
    // -------------------------------------------------------------------------

    // Top level: a run of function definitions and statements, parsed until `Eof`.
    pub(super) fn parse_program(&mut self) -> Result<(), ParseError> {
        loop {
            self.pending_dedents = 0;
            self.depth = 0;
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
                    self.tokens[self.pos - 1].kind,
                    TokenKind::Newline | TokenKind::Dedent | TokenKind::Indent
                );
                match self.peek().kind {
                    TokenKind::Newline | TokenKind::Eof | TokenKind::Dedent => {}
                    _ if ended_at_layout => {}
                    other => {
                        return Err(ParseError::Unexpected(format!(
                            "Unexpected token {other:?}."
                        )));
                    }
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Function definitions
    // -------------------------------------------------------------------------

    // A func-def is `Ident "(" … ")" [":" type] "is"`. The trailing `is` is what tells it
    // apart from a top-level call statement `f(args)`.
    fn looks_like_func_def(&self) -> bool {
        if self.peek().kind != TokenKind::Identifier || self.at(1) != TokenKind::LParen {
            return false;
        }
        let close = match self.matching_paren(self.pos + 1) {
            Some(index) => index,
            None => return false,
        };
        let mut cursor = close + 1;
        if self.tokens.get(cursor).map(|token| token.kind) == Some(TokenKind::Colon) {
            cursor += 2; // ": type"
        }
        self.tokens.get(cursor).map(|token| token.kind) == Some(TokenKind::KwIs)
    }

    // Index of the `)` that closes the `(` at `lparen`, found by counting nesting depth.
    // A non-consuming lookahead (it indexes `tokens` directly, never advances `pos`).
    // Used by `looks_like_func_def` to skip past the parameter list and peek at what follows.
    // Returns `None` if the parens are unbalanced or `Eof` is hit first.
    fn matching_paren(&self, lparen: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut index = lparen;
        while index < self.tokens.len() {
            match self.tokens[index].kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index);
                    }
                }
                TokenKind::Eof => return None,
                _ => {}
            }
            index += 1;
        }
        None
    }

    // `<name>(<params>) [: <type>] is <body>` — a function definition.
    fn parse_func_def(&mut self) -> Result<(), ParseError> {
        let name_token = self.advance(); // Identifier
        let name = self.ast.intern(name_token.text);
        self.expect(TokenKind::LParen)?;
        let params = self.parse_params(name_token.line, name_token.col)?; // line/col anchor the hung-param rule
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
        for param in &params {
            if !seen.insert(param.name) {
                return Err(ParseError::Unexpected(format!(
                    "Duplicate parameter `{}`.",
                    self.ast.names[param.name.value()]
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

    // The parenthesised parameter list `<name>: <type>, …`, enforcing the column-alignment rules.
    fn parse_params(
        &mut self,
        function_line: u32,
        function_col: u32,
    ) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if self.peek().kind == TokenKind::RParen {
            return Ok(params);
        }
        // The first parameter fixes the column that continuation lines must align to.
        let first = self.peek().clone();
        // A first parameter hung onto a later line — the opening line left bare — must be indented
        // past the function's own line, mirroring how a block body indents under its header.
        if first.line != function_line && first.col <= function_col {
            return Err(ParseError::Layout(format!(
                "Parameter at line {} col {} must be indented past the function at column {function_col}.",
                first.line, first.col
            )));
        }
        loop {
            let (line, col) = {
                let token = self.peek();
                (token.line, token.col)
            };
            if line != first.line && col != first.col {
                return Err(ParseError::Layout(format!(
                    "Parameter at line {line} col {col} must align to the first parameter's column {}.",
                    first.col
                )));
            }
            let name_token = self.expect(TokenKind::Identifier)?;
            let name = self.ast.intern(name_token.text);
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

    // A type annotation: `bool` / `int` / `float` / `void`.
    pub(super) fn parse_type(&mut self) -> Result<PinpType, ParseError> {
        let token = self.expect(TokenKind::Identifier)?;
        match token.text {
            "bool" => Ok(PinpType::Bool),
            "int" => Ok(PinpType::Int),
            "float" => Ok(PinpType::Float),
            "void" => Ok(PinpType::Void),
            other => Err(ParseError::Unexpected(format!("Unknown type `{other}`."))),
        }
    }

    // A function body: an indented block, or the single-line form `is <expr>` (which must declare
    // a return type).
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
                    )));
                }
            }
            Ok(Block {
                stmts: Vec::new(),
                result: Some(result),
            })
        }
    }

    // -------------------------------------------------------------------------
    // Blocks and loops
    // -------------------------------------------------------------------------

    // Parse an indented block — `Newline Indent <stmts> Dedent` — shared by function bodies and
    // every control-flow body (`if`/`while`/`loop`). A trailing expression statement becomes the
    // block's `result` (its value when the block is used as one); a block ending in any other
    // statement has `result: None`.
    pub(super) fn parse_block(&mut self) -> Result<Block, ParseError> {
        // A wrapped one-line conditional inside the statement just parsed opened indents whose
        // closing dedents sit just past the statement's newline; swallow them so they are not taken
        // for this block's terminator.
        fn swallow_pending_dedents(parser: &mut Parser<'_>) {
            if parser.pending_dedents == 0 {
                return;
            }
            if parser.peek().kind == TokenKind::Newline {
                parser.pos += 1;
            }
            while parser.pending_dedents > 0 && parser.peek().kind == TokenKind::Dedent {
                parser.pos += 1;
                parser.pending_dedents -= 1;
            }
        }

        // Consume the separator between two statements and report whether the block has ended. A
        // multiline construct (or a swallowed continuation) leaves us already on a layout token, so
        // the next statement may follow with no separator of its own.
        fn reached_block_end(parser: &mut Parser<'_>) -> Result<bool, ParseError> {
            let ended_at_layout = matches!(
                parser.tokens[parser.pos - 1].kind,
                TokenKind::Newline | TokenKind::Dedent | TokenKind::Indent
            );
            match parser.peek().kind {
                TokenKind::Newline => parser.pos += 1,
                TokenKind::Dedent => {
                    parser.pos += 1;
                    return Ok(true);
                }
                TokenKind::Eof => return Ok(true),
                _ if ended_at_layout => {}
                other => {
                    return Err(ParseError::Unexpected(format!(
                        "Unexpected token {other:?}."
                    )));
                }
            }
            if parser.peek().kind == TokenKind::Dedent {
                parser.pos += 1;
                return Ok(true);
            }
            Ok(parser.peek().kind == TokenKind::Eof)
        }

        // A trailing expression statement is the block's result value, not one of its statements.
        fn take_trailing_result(stmts: &mut Vec<Stmt>) -> Option<ExprId> {
            match stmts.last() {
                Some(Stmt::Expr(expr_id)) => {
                    let expr_id = *expr_id;
                    stmts.pop();
                    Some(expr_id)
                }
                _ => None,
            }
        }

        self.enter_nesting()?;
        self.expect(TokenKind::Newline)?;
        self.expect(TokenKind::Indent)?;
        let mut stmts: Vec<Stmt> = Vec::new();
        loop {
            stmts.push(self.parse_stmt()?);
            swallow_pending_dedents(self);
            if reached_block_end(self)? {
                break;
            }
        }
        let result = take_trailing_result(&mut stmts);
        self.depth -= 1;
        Ok(Block { stmts, result })
    }

    // `while <cond> <block>` — a pre-tested loop.
    fn parse_while(&mut self) -> Result<Stmt, ParseError> {
        self.advance(); // 'while'
        let cond = self.parse_expr(0)?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    // `loop <block> (while | until) <cond>` — a post-tested loop (`until` flips the sense).
    fn parse_loop(&mut self) -> Result<Stmt, ParseError> {
        self.advance(); // 'loop'
        let body = self.parse_block()?;
        let until = match self.peek().kind {
            TokenKind::KwWhile => false,
            TokenKind::KwUntil => true,
            other => {
                return Err(ParseError::Unexpected(format!(
                    "Expected `while` or `until` after a loop body, found {other:?}."
                )));
            }
        };
        self.advance(); // 'while' / 'until'
        let cond = self.parse_expr(0)?;
        Ok(Stmt::Loop { body, cond, until })
    }

    // `for <binder>[, <binder>…] in <source> <block>`.
    // 1 binder → `Stmt::For`; 2–3 binders → `Stmt::ForArray`; 4+ → `ParseError`.
    fn parse_for(&mut self) -> Result<Stmt, ParseError> {
        self.advance(); // 'for'
        let first_sym = self.expect_binder()?;
        if self.peek().kind == TokenKind::Comma {
            let mut binders = vec![first_sym];
            while self.peek().kind == TokenKind::Comma {
                self.advance(); // ','
                binders.push(self.expect_binder()?);
            }
            if binders.len() > 3 {
                return Err(ParseError::Unexpected(
                    "Too many binders in for loop.".into(),
                ));
            }
            self.expect(TokenKind::KwIn)?;
            let source = self.parse_expr(0)?;
            let body = self.parse_block()?;
            return Ok(Stmt::ForArray {
                binders,
                source,
                body,
            });
        }
        self.expect(TokenKind::KwIn)?;
        let source = self.parse_expr(0)?;
        let body = self.parse_block()?;
        Ok(Stmt::For {
            var: first_sym,
            source,
            body,
        })
    }

    // -------------------------------------------------------------------------
    // Statements and assignment
    // Consume the next token as a for-loop binder name: either a plain `Identifier` or the
    // don't-care `_` (`Underscore`). Returns the interned `SymId`; errors on anything else.
    fn expect_binder(&mut self) -> Result<SymId, ParseError> {
        let token = self.peek();
        match token.kind {
            TokenKind::Identifier | TokenKind::Underscore => {
                let text = token.text;
                let sym = self.ast.intern(text);
                self.advance();
                Ok(sym)
            }
            _ => Err(ParseError::Unexpected(format!(
                "Expected a binder name or `_`, found `{}`.",
                token.text
            ))),
        }
    }

    // -------------------------------------------------------------------------

    // One statement: a control-flow construct (`while`/`loop`/`for`), an expression statement, or a
    // (parallel/chained/compound) assignment.
    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        // Build `target_list = (target_list =)* value_list` from its `=`-separated groups: the last
        // group is the values; each earlier group is a target list checked for matching arity.
        fn finish_assignment(
            parser: &mut Parser<'_>,
            mut groups: Vec<Vec<ExprId>>,
        ) -> Result<Stmt, ParseError> {
            let values = groups.pop().expect("the first group is always present");
            let mut target_lists = Vec::with_capacity(groups.len());
            for group in groups {
                // Convert each target before the arity check, so a non-place like `1, 2 = 3` reports
                // the more specific "invalid target" rather than an arity mismatch.
                let mut targets = Vec::with_capacity(group.len());
                for expr_id in group {
                    targets.push(parser.expr_as_place(expr_id)?);
                }
                if targets.len() != values.len() {
                    return Err(ParseError::Unexpected(format!(
                        "Assignment has {} target(s) but {} value(s).",
                        targets.len(),
                        values.len()
                    )));
                }
                target_lists.push(targets);
            }
            Ok(Stmt::Assign {
                target_lists,
                values,
            })
        }

        match self.peek().kind {
            TokenKind::KwWhile => return self.parse_while(),
            TokenKind::KwLoop => return self.parse_loop(),
            TokenKind::KwFor => return self.parse_for(),
            _ => {}
        }
        // Otherwise parse as Python does: one or more comma-separated lists joined by `=`. A single
        // list with no following `=` is an expression statement; a `place <op>= e` is compound
        // assignment; everything else is a plain (parallel/chained) assignment.
        let first_group = self.parse_expr_list()?;

        // Compound assignment is single-target, single-value: `place <op>= e`.
        if first_group.len() == 1
            && let Some(op) = compound_assign_op(self.peek().kind)
        {
            let node = self.ast.node(first_group[0]).clone();
            // `arr[idx] <op>= rhs` — value is just the RHS; compound_op carries the operator.
            if let Node::Index { array, index } = node {
                let target = match self.ast.node(array) {
                    Node::Var(sym_id) => Place::Local(*sym_id),
                    Node::Global(sym_id) => Place::Global(*sym_id),
                    _ => return Err(ParseError::Unexpected("Invalid assignment target.".into())),
                };
                self.advance(); // the compound operator
                let value = self.parse_expr(0)?;
                return Ok(Stmt::IndexedAssign {
                    target,
                    index,
                    value,
                    compound_op: Some(op),
                });
            }
            // `mat[row, col] <op>= rhs` — compound_op carries the operator.
            if let Node::Index2D { matrix, row, col } = node {
                let target = match self.ast.node(matrix) {
                    Node::Var(sym_id) => Place::Local(*sym_id),
                    Node::Global(sym_id) => Place::Global(*sym_id),
                    _ => return Err(ParseError::Unexpected("Invalid assignment target.".into())),
                };
                self.advance(); // the compound operator
                let value = self.parse_expr(0)?;
                return Ok(Stmt::IndexedAssign2D {
                    target,
                    row,
                    col,
                    value,
                    compound_op: Some(op),
                });
            }
            let place = self.expr_as_place(first_group[0])?;
            self.advance(); // the compound operator
            let rhs = self.parse_expr(0)?;
            return Ok(self.finish_compound(place, op, rhs));
        }

        // `arr[idx] = value` — 1D indexed assignment.
        if first_group.len() == 1 && self.peek().kind == TokenKind::Equal {
            let node = self.ast.node(first_group[0]).clone();
            if let Node::Index { array, index } = node {
                let target = match self.ast.node(array) {
                    Node::Var(sym_id) => Place::Local(*sym_id),
                    Node::Global(sym_id) => Place::Global(*sym_id),
                    _ => return Err(ParseError::Unexpected("Invalid assignment target.".into())),
                };
                self.advance(); // '='
                let value = self.parse_expr(0)?;
                return Ok(Stmt::IndexedAssign {
                    target,
                    index,
                    value,
                    compound_op: None,
                });
            }
            // `mat[row, col] = value` — 2D indexed assignment.
            if let Node::Index2D { matrix, row, col } = node {
                let target = match self.ast.node(matrix) {
                    Node::Var(sym_id) => Place::Local(*sym_id),
                    Node::Global(sym_id) => Place::Global(*sym_id),
                    _ => return Err(ParseError::Unexpected("Invalid assignment target.".into())),
                };
                self.advance(); // '='
                let value = self.parse_expr(0)?;
                return Ok(Stmt::IndexedAssign2D {
                    target,
                    row,
                    col,
                    value,
                    compound_op: None,
                });
            }
        }

        if self.peek().kind != TokenKind::Equal {
            // No `=` — an expression statement, which is exactly one expression.
            if first_group.len() == 1 {
                return Ok(Stmt::Expr(first_group[0]));
            }
            // A comma-list with no `=`: if the next line starts with a comma, the user tried a
            // leading-comma continuation — the comma must instead trail the current line.
            if self.next_line_starts_with_comma() {
                return Err(ParseError::Layout(
                    "Multi-line assignment: comma must not start a line.".into(),
                ));
            }
            return Err(ParseError::Unexpected(
                "Expected `=` after a comma-separated target list.".into(),
            ));
        }

        // Plain assignment: collect the `=`-separated groups, then resolve their targets and values.
        let mut groups = vec![first_group];
        while self.peek().kind == TokenKind::Equal {
            self.advance(); // '='
            groups.push(self.parse_expr_list()?);
        }
        finish_assignment(self, groups)
    }

    // A comma-separated list of expressions, e.g. `1, 2, 3` or an assignment's target/value list.
    // A trailing comma continues the list onto the next line (an assignment LHS or RHS that wraps);
    // the continuation must align to the column where the list began.
    fn parse_expr_list(&mut self) -> Result<Vec<ExprId>, ParseError> {
        let list_col = self.peek().col;
        let mut exprs = vec![self.parse_expr(0)?];
        while self.peek().kind == TokenKind::Comma {
            self.advance(); // ','
            // A comma at the end of a line continues the list; the next item aligns to `list_col`.
            if self.peek().kind == TokenKind::Newline {
                self.consume_list_continuation(list_col)?;
            }
            exprs.push(self.parse_expr(0)?);
        }
        Ok(exprs)
    }

    // Skip the layout tokens of a wrapped comma-list and require the continuation to start at
    // `list_col`. A continuation indented past the enclosing block opens an `Indent`, banked into
    // `pending_dedents` so `parse_block` later swallows the matching `Dedent` (as for a wrapped
    // `else`); a same-indent continuation is just a `Newline`.
    fn consume_list_continuation(&mut self, list_col: u32) -> Result<(), ParseError> {
        while matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            if self.peek().kind == TokenKind::Indent {
                self.pending_dedents += 1;
            }
            self.pos += 1;
        }
        if self.peek().col != list_col {
            return Err(ParseError::Layout(format!(
                "Continuation at column {} must align to the list's column {list_col}.",
                self.peek().col
            )));
        }
        Ok(())
    }

    // Non-consuming: does the next line (past the layout tokens at the cursor) begin with a comma?
    // Used only to give the specific "comma must not start a line" diagnostic.
    fn next_line_starts_with_comma(&self) -> bool {
        let mut index = self.pos;
        while matches!(
            self.tokens[index].kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            index += 1;
        }
        self.tokens[index].kind == TokenKind::Comma
    }

    // Reinterpret an already-parsed expression as an assignment target. Only a bare name or `::name`
    // is a valid place; anything else (a literal, a call, an arithmetic node) is rejected. The read
    // `Var`/`Global` node parsed for the target is left unused in the arena.
    fn expr_as_place(&self, expr_id: ExprId) -> Result<Place, ParseError> {
        match self.ast.node(expr_id) {
            Node::Var(sym_id) => Ok(Place::Local(*sym_id)),
            Node::Global(sym_id) => Ok(Place::Global(*sym_id)),
            _ => Err(ParseError::Unexpected(
                "Invalid assignment target; only names and `::globals` can be assigned.".into(),
            )),
        }
    }

    // Desugar `place <op>= e` into `place = place <op> e` — the single-target, single-value shape.
    fn finish_compound(&mut self, place: Place, op: BinOp, rhs: ExprId) -> Stmt {
        let read = self.ast.push(Node::from(place));
        let combined = self.ast.push(Node::Bin { op, lhs: read, rhs });
        Stmt::Assign {
            target_lists: vec![vec![place]],
            values: vec![combined],
        }
    }
}
