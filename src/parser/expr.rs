// SPDX-License-Identifier: MIT

use super::*;
use crate::lexer::{TokenKind, lex};

// ---------------------------------------------------------------------------
// Operator tables and binding powers
// ---------------------------------------------------------------------------

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

// Binding power of the range operators `.. ..< ..>`. Above the comparison band (45) yet below
// additive (60), so `a+1..b-1` is `(a+1)..(b-1)`; the bounds are parsed one power above this, so a
// range never nests directly inside another (`1..2..3` is rejected by sema, not silently chained).
const RANGE_BP: u8 = 50;

// Binding power of `in` membership. It shares the comparison band's looseness (it yields a `Bool`
// like a comparison) and does not chain — its range operand is parsed one power above.
const MEMBERSHIP_BP: u8 = 45;

// The range operator a token introduces, if any.
fn range_kind(kind: TokenKind) -> Option<RangeKind> {
    Some(match kind {
        TokenKind::DotDot => RangeKind::Inclusive,
        TokenKind::DotDotLt => RangeKind::UpExclusive,
        TokenKind::DotDotGt => RangeKind::DownExclusive,
        _ => return None,
    })
}

// Maximum number of arms (the leading `if` plus its `elif`s) in one `if` construct. Unlike nesting,
// an `elif` ladder is parsed iteratively, so it is *not* a stack risk — this is a deliberate sanity
// cap: a construct this wide is far likelier a generation bug than intent.
const MAX_IF_ARMS: usize = 256;

// Right binding power of the prefix operators `-` and `not`: above `*`/`/` (70), below `^` (80),
// so `-a*b` is `(-a)*b` and `-a^b` is `-(a^b)`. `not` shares this level to match C's unary-tight
// `!`, so `not a == b` is `(not a) == b`.
const UNARY_MINUS_BP: u8 = 75;

// Postfix operators `arr[i]` and `obj.member` bind tighter than everything else — their left binding
// power is above exponentiation (80) so `-arr[0]` is `-(arr[0])` and `a^b[0]` is `a^(b[0])`.
const POSTFIX_BP: u8 = 90;

// The "direction" of a comparison operator. A chain of more than one comparison must keep a single
// direction so the relation is transitive and reads across the chain (`a < b <= c`), as in maths.
// `!=` is non-transitive (`a != b != c` is not "all distinct"), so it has no direction and never
// chains.
#[derive(PartialEq)]
enum ChainDirection {
    Ascending,  // < <=
    Descending, // > >=
    Equality,   // ==
}

fn chain_direction(op: BinOp) -> Option<ChainDirection> {
    Some(match op {
        BinOp::Lt | BinOp::Le => ChainDirection::Ascending,
        BinOp::Gt | BinOp::Ge => ChainDirection::Descending,
        BinOp::Eq => ChainDirection::Equality,
        _ => return None,
    })
}

fn comparison_symbol(op: BinOp) -> &'static str {
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

// A chain of more than one comparison must be monotonic: every operator shares one direction
// (all ascending `< <=`, all descending `> >=`, or all `==`), and `!=` never chains. A lone
// comparison is always fine — including `!=`.
fn check_monotonic(operators: &[BinOp]) -> Result<(), ParseError> {
    if operators.len() < 2 {
        return Ok(());
    }
    let first = operators[0];
    let direction = chain_direction(first).ok_or_else(|| {
        ParseError::Unexpected(format!(
            "Cannot chain `{}`; it is not transitive.",
            comparison_symbol(first)
        ))
    })?;
    for &op in &operators[1..] {
        match chain_direction(op) {
            None => {
                return Err(ParseError::Unexpected(format!(
                    "Cannot chain `{}`; it is not transitive.",
                    comparison_symbol(op)
                )));
            }
            Some(op_direction) if op_direction != direction => {
                return Err(ParseError::Unexpected(format!(
                    "Cannot chain `{}` with `{}`.",
                    comparison_symbol(first),
                    comparison_symbol(op)
                )));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Literal converters
// ---------------------------------------------------------------------------

// Convert a lexically-valid integer literal to its value, rejecting anything that does not fit in
// an `i64` (a too-large decimal, an over-long hex/binary string, or an overflowing `mantissa*10^exp`)
// rather than panicking.
fn parse_int(text: &str) -> Result<i64, ParseError> {
    let out_of_range =
        || ParseError::Unexpected(format!("Integer literal `{text}` is out of range."));
    let digits: String = text.chars().filter(|&ch| ch != '_').collect();
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        return i64::from_str_radix(hex, 16).map_err(|_| out_of_range());
    }
    if let Some(binary) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        return i64::from_str_radix(binary, 2).map_err(|_| out_of_range());
    }
    if let Some(pos) = digits.find(['e', 'E']) {
        let mantissa: i64 = digits[..pos].parse().map_err(|_| out_of_range())?;
        let exponent: u32 = digits[pos + 1..].parse().map_err(|_| out_of_range())?;
        return 10i64
            .checked_pow(exponent)
            .and_then(|factor| mantissa.checked_mul(factor))
            .ok_or_else(out_of_range);
    }
    digits.parse().map_err(|_| out_of_range())
}

fn parse_float(text: &str) -> f64 {
    let digits: String = text.chars().filter(|&ch| ch != '_').collect();
    digits.parse().unwrap()
}

impl<'src> Parser<'src> {
    // -------------------------------------------------------------------------
    // If-expression
    // -------------------------------------------------------------------------

    // `if <cond> <block> (elif <cond> <block>)* (else <block>)?` — the block-form if-expression.
    fn parse_if(&mut self) -> Result<ExprId, ParseError> {
        self.advance(); // 'if'
        let cond = self.parse_expr(0)?;
        let body = self.parse_block()?;
        let mut arms = vec![IfArm { cond, body }];
        let mut else_block = None;
        loop {
            match self.peek().kind {
                TokenKind::KwElif => {
                    self.advance();
                    let cond = self.parse_expr(0)?;
                    let body = self.parse_block()?;
                    arms.push(IfArm { cond, body });
                    if arms.len() > MAX_IF_ARMS {
                        return Err(ParseError::TooManyArms(format!(
                            "Excessive number of if-elif constructs. The maximum is {MAX_IF_ARMS}."
                        )));
                    }
                }
                TokenKind::KwElse => {
                    self.advance();
                    else_block = Some(self.parse_block()?);
                    break;
                }
                _ => break,
            }
        }
        Ok(self.ast.push(Node::If { arms, else_block }))
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
            else_block: Some(else_block),
        }))
    }

    // -------------------------------------------------------------------------
    // Pratt expression loop
    // -------------------------------------------------------------------------

    // Precedence-climbing: parse a prefix, then fold operators binding at or above `min_bp`. The
    // chaining bands (conditional, comparison, range, `in`) are handled explicitly before the
    // `infix` binding-power table.
    pub(super) fn parse_expr(&mut self, min_bp: u8) -> Result<ExprId, ParseError> {
        self.enter_nesting()?;
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
            // A range operator turns the operand so far into the range's `start`; like the
            // comparison band, its result is not an ordinary `Bin`, so it lives outside `infix`.
            if let Some(kind) = range_kind(self.peek().kind)
                && RANGE_BP >= min_bp
            {
                lhs = self.parse_range_tail(lhs, kind)?;
                continue;
            }
            // `value in range` — membership, yielding `Bool`.
            if self.peek().kind == TokenKind::KwIn && MEMBERSHIP_BP >= min_bp {
                self.advance(); // 'in'
                let range = self.parse_expr(MEMBERSHIP_BP + 1)?;
                lhs = self.ast.push(Node::Membership { value: lhs, range });
                continue;
            }
            // Postfix `lhs[…]` — 1D element/slice, or 2D `lhs[row_sel, col_sel]`.
            if self.peek().kind == TokenKind::LBracket && POSTFIX_BP >= min_bp {
                self.advance(); // '['
                let row_or_index = self.parse_dim_selector()?;
                if self.peek().kind == TokenKind::Comma {
                    self.advance(); // ','
                    let col = self.parse_dim_selector()?;
                    self.expect(TokenKind::RBracket)?;
                    lhs = self.ast.push(Node::Index2D {
                        matrix: lhs,
                        row: row_or_index,
                        col,
                    });
                } else {
                    self.expect(TokenKind::RBracket)?;
                    lhs = self.ast.push(Node::Index {
                        array: lhs,
                        index: row_or_index,
                    });
                }
                continue;
            }
            // Postfix `lhs.member` — built-in member access (`.len`, etc.).
            if self.peek().kind == TokenKind::Dot && POSTFIX_BP >= min_bp {
                self.advance(); // '.'
                let member_name = self.expect(TokenKind::Identifier)?.text;
                let member = self.ast.intern(member_name);
                lhs = self.ast.push(Node::Member {
                    object: lhs,
                    member,
                });
                continue;
            }
            let Some((left_bp, right_bp, op)) = infix(self.peek().kind) else {
                break;
            };
            if left_bp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_expr(right_bp)?;
            lhs = self.ast.push(Node::Bin { op, lhs, rhs });
        }
        self.depth -= 1;
        Ok(lhs)
    }

    // Parse `first op1 e1 op2 e2 …` (`first` already parsed) and desugar it to the `and` of its
    // adjacent comparisons: `a < b < c` becomes `(a < b) and (b < c)`, with the middle operand
    // shared. A chain of one comparison is just that comparison — no `and` is introduced.
    fn parse_comparison_chain(&mut self, first: ExprId) -> Result<ExprId, ParseError> {
        let mut prev = first;
        let mut operators: Vec<BinOp> = Vec::new();
        let mut result: Option<ExprId> = None;
        while let Some(op) = comparison_op(self.peek().kind) {
            self.advance();
            // Operands bind just above the band so a following comparison starts a new link
            // rather than nesting inside this one.
            let next = self.parse_expr(COMPARISON_BP + 1)?;
            let comparison = self.ast.push(Node::Bin {
                op,
                lhs: prev,
                rhs: next,
            });
            result = Some(match result {
                None => comparison,
                Some(accumulated) => self.ast.push(Node::Bin {
                    op: BinOp::And,
                    lhs: accumulated,
                    rhs: comparison,
                }),
            });
            operators.push(op);
            prev = next;
        }
        check_monotonic(&operators)?;
        Ok(result.expect("a chain is only started when a comparison operator is next"))
    }

    // Tail of a range, entered at the range operator with `start` already parsed. The stop bound
    // (and the optional `:step`) bind one power above `RANGE_BP`, so neither swallows a following
    // range, comparison, or `in`.
    fn parse_range_tail(&mut self, start: ExprId, kind: RangeKind) -> Result<ExprId, ParseError> {
        self.advance(); // '..' / '..<' / '..>'
        let stop = self.parse_expr(RANGE_BP + 1)?;
        let step = if self.peek().kind == TokenKind::Colon {
            self.advance(); // ':'
            Some(self.parse_expr(RANGE_BP + 1)?)
        } else {
            None
        };
        Ok(self.ast.push(Node::Range {
            start,
            stop,
            step,
            kind,
        }))
    }

    // -------------------------------------------------------------------------
    // Prefix, primary, and calls
    // -------------------------------------------------------------------------

    // A prefix operator (`-` / `not`) applied to an operand, or else a primary.
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

    // An atom: a literal, a name or `::global`, a parenthesised expression, a call, or a block-form
    // `if`.
    fn parse_primary(&mut self) -> Result<ExprId, ParseError> {
        match self.peek().kind {
            TokenKind::Int => {
                let value = parse_int(self.advance().text)?;
                Ok(self.ast.push(Node::Int(value)))
            }
            TokenKind::Float => {
                let value = parse_float(self.advance().text);
                Ok(self.ast.push(Node::Float(value)))
            }
            TokenKind::KwTrue => {
                self.advance();
                Ok(self.ast.push(Node::Bool(true)))
            }
            TokenKind::KwFalse => {
                self.advance();
                Ok(self.ast.push(Node::Bool(false)))
            }
            TokenKind::Str => {
                // Strip the matching quotes; the interior is the content (single-line for now).
                let text = self.advance().text;
                let content = text[1..text.len() - 1].to_string();
                let str_id = self.ast.push_string_literal(content);
                Ok(self.ast.push(Node::Str(str_id)))
            }
            TokenKind::FStr => {
                // Strip the leading `f` and the matching quotes, then split into segments.
                let text = self.advance().text;
                let content = &text[2..text.len() - 1];
                let segments = self.parse_fstring_segments(content)?;
                Ok(self.ast.push(Node::FStr { segments }))
            }
            TokenKind::Identifier => {
                if self.at(1) == TokenKind::LParen {
                    return self.parse_call();
                }
                let name = self.advance().text;
                let sym_id = self.ast.intern(name);
                Ok(self.ast.push(Node::Var(sym_id)))
            }
            TokenKind::ColonColon => {
                self.advance(); // '::'
                let name = self.expect(TokenKind::Identifier)?.text;
                let sym_id = self.ast.intern(name);
                Ok(self.ast.push(Node::Global(sym_id)))
            }
            // `if` opening an expression is the block form; the one-line ternary is reached from
            // the `parse_expr` loop instead, after a left operand.
            TokenKind::KwIf => self.parse_if(),
            TokenKind::LParen => {
                self.advance();
                let expr_id = self.parse_expr(0)?;
                self.expect(TokenKind::RParen)?;
                Ok(expr_id)
            }
            TokenKind::LBracket => self.parse_array_or_comprehension(),
            other => Err(ParseError::Unexpected(format!(
                "Unexpected token {other:?}."
            ))),
        }
    }

    // `[…]` — a 1D array literal or an array comprehension.
    //
    // Literal: `[e0, e1, …]` — a comma-separated list of elements (trailing comma allowed).
    // Comprehension: `[element for var[:type] in source]` — the `for` keyword after the first
    // element is the discriminator. The `:type` annotation promotes the loop variable; without it
    // the variable type defaults to `Int` (the range's native element type).
    fn parse_array_or_comprehension(&mut self) -> Result<ExprId, ParseError> {
        self.advance(); // '['

        // Empty `[]` is always an error — a zero-length array literal has no deducible element type.
        if self.peek().kind == TokenKind::RBracket {
            return Err(ParseError::Unexpected(
                "Array literal cannot be empty.".into(),
            ));
        }

        // The column of the first element is the alignment anchor for multi-line matrix rows.
        let anchor_col = self.peek().col;
        let first = self.parse_expr(0)?;

        // Comprehension: `[element for var[:type] in source]`.
        if self.peek().kind == TokenKind::KwFor {
            self.advance(); // 'for'
            let var_name = self.expect(TokenKind::Identifier)?.text;
            let var = self.ast.intern(var_name);
            let var_type = if self.peek().kind == TokenKind::Colon {
                self.advance(); // ':'
                self.parse_type()?
            } else {
                PinpType::Int
            };
            self.expect(TokenKind::KwIn)?;
            let source = self.parse_expr(0)?;
            self.expect(TokenKind::RBracket)?;
            return Ok(self.ast.push(Node::Comprehension {
                element: first,
                var,
                var_type,
                source,
            }));
        }

        // Array or matrix literal: parse the first row.
        let mut first_row = vec![first];
        while self.peek().kind == TokenKind::Comma {
            self.advance(); // ','
            if self.peek().kind == TokenKind::RBracket {
                break; // trailing comma before `]`
            }
            if self.peek().kind == TokenKind::Semicolon {
                return Err(ParseError::Unexpected(
                    "Trailing `,` before `;` in matrix row.".into(),
                ));
            }
            first_row.push(self.parse_expr(0)?);
        }

        // Matrix literal: `[r0_e0, …; r1_e0, …; …]` — a `;` follows the first row.
        if self.peek().kind == TokenKind::Semicolon {
            let mut rows = vec![first_row];
            while self.peek().kind == TokenKind::Semicolon {
                let sep_line = self.peek().line;
                self.advance(); // ';'
                if self.peek().kind == TokenKind::RBracket {
                    return Err(ParseError::Unexpected(
                        "Trailing `;` in matrix literal.".into(),
                    ));
                }
                // A row that begins on a new line must align its first element with row 0.
                if self.peek().line != sep_line && self.peek().col != anchor_col {
                    return Err(ParseError::Layout(format!(
                        "Matrix row at column {} must align with the first row at column {}.",
                        self.peek().col,
                        anchor_col,
                    )));
                }
                let mut row = vec![self.parse_expr(0)?];
                while self.peek().kind == TokenKind::Comma {
                    self.advance(); // ','
                    if self.peek().kind == TokenKind::RBracket {
                        break; // trailing comma before `]`
                    }
                    if self.peek().kind == TokenKind::Semicolon {
                        return Err(ParseError::Unexpected(
                            "Trailing `,` before `;` in matrix row.".into(),
                        ));
                    }
                    row.push(self.parse_expr(0)?);
                }
                rows.push(row);
            }
            self.expect(TokenKind::RBracket)?;
            return Ok(self.ast.push(Node::MatrixLiteral { rows }));
        }

        // 1D array literal.
        self.expect(TokenKind::RBracket)?;
        Ok(self.ast.push(Node::ArrayLiteral {
            elements: first_row,
        }))
    }

    // One dimension selector inside `obj[…, …]`:
    //   `:` immediately followed by `,` or `]` → `Node::FullExtent` (full extent of that dimension)
    //   otherwise → an arbitrary expression (scalar index or range)
    fn parse_dim_selector(&mut self) -> Result<ExprId, ParseError> {
        if self.peek().kind == TokenKind::Colon
            && matches!(self.at(1), TokenKind::Comma | TokenKind::RBracket)
        {
            self.advance(); // ':'
            return Ok(self.ast.push(Node::FullExtent));
        }
        self.parse_expr(0)
    }

    // `<name>(<args>)` — a function call.
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
                        )));
                    }
                }
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(self.ast.push(Node::Call { callee, args }))
    }

    // Split an f-string's inter-delimiter content into alternating literal/interpolation segments.
    // A `{` opens a `{name}`/`{::name}` hole closed by the next `}`; every other character is literal
    // text. Adjacent holes (and a leading/trailing `{`) emit no empty literal run. The content is
    // single-line for now — multi-line auto-dedent of the literal runs is a later step.
    fn parse_fstring_segments(
        &mut self,
        content: &'src str,
    ) -> Result<Vec<FStrSegment>, ParseError> {
        let mut segments = Vec::new();
        let bytes = content.as_bytes(); // `{` and `}` are ASCII, so byte scanning is UTF-8-safe
        let mut literal_start = 0;
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'{' => {
                    if index > literal_start {
                        let run = content[literal_start..index].to_string();
                        segments.push(FStrSegment::Literal(self.ast.push_string_literal(run)));
                    }
                    let rest = &content[index + 1..];
                    let close = rest.find('}').ok_or_else(|| {
                        ParseError::Unexpected("Unterminated `{` in f-string.".into())
                    })?;
                    let place = self.fstring_place(&rest[..close])?;
                    segments.push(FStrSegment::Interp(place));
                    index += 1 + close + 1; // step past the `}`
                    literal_start = index;
                }
                b'}' => {
                    return Err(ParseError::Unexpected("Unmatched `}` in f-string.".into()));
                }
                _ => index += 1,
            }
        }
        if literal_start < bytes.len() {
            let run = content[literal_start..].to_string();
            segments.push(FStrSegment::Literal(self.ast.push_string_literal(run)));
        }
        Ok(segments)
    }

    // Resolve one f-string hole to its [`Place`]. The hole's text is re-lexed — the lexer is the
    // single source of truth for what a name is — so it must tokenise to exactly `name` (a local) or
    // `::name` (a global); anything else (empty, a number, an operator, two names) is a parse error.
    fn fstring_place(&mut self, hole: &'src str) -> Result<Place, ParseError> {
        let tokens = lex(hole).map_err(|error| ParseError::Lex(error.message))?;
        match tokens.as_slice() {
            [name, eof] if name.kind == TokenKind::Identifier && eof.kind == TokenKind::Eof => {
                Ok(Place::Local(self.ast.intern(name.text)))
            }
            [marker, name, eof]
                if marker.kind == TokenKind::ColonColon
                    && name.kind == TokenKind::Identifier
                    && eof.kind == TokenKind::Eof =>
            {
                Ok(Place::Global(self.ast.intern(name.text)))
            }
            _ => Err(ParseError::Unexpected(format!(
                "Invalid f-string interpolation `{{{hole}}}`; expected a name."
            ))),
        }
    }
}
