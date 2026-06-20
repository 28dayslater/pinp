// SPDX-License-Identifier: MIT

//! Recursive-descent parser with Pratt (precedence-climbing) expression parsing.
//!
//! A program is a sequence of [`TopLevel`]s — top-level statements (which run in the global
//! scope) and function definitions ([`FuncDef`]). Statements and function bodies are parsed
//! by straight recursive descent; expressions are parsed by `parse_expr`, a Pratt loop
//! driven by the binding-power table in `infix`. Each operator carries a (left, right)
//! binding power; left-associative ops use `right == left + 1`, right-associative ops (`^`)
//! use `right == left`. Prefix `-` is handled in `parse_prefix` at `UNARY_MINUS_BP`.
//!
//! Output is an arena, not a tree of boxes:
//! - **AST arena** — nodes live in `Ast::nodes: Vec<Node>` and reference each other by
//!   [`ExprId`] (an index), so there is no `Box`/`Rc`. The inferred [`PinpType`] of each node
//!   sits in the parallel `Ast::types` vec at the same index.
//! - **Interner** — identifiers map to [`SymId`] via `Ast::symbols`, backed by `&'src str`
//!   slices borrowed straight from the source (no string copies).
//!
//! Parsing is **purely syntactic**: it builds the structural AST and reports only lexical/layout
//! errors ([`ParseError`]). The `types` arena is left unpopulated — name resolution, scoping, and
//! type inference belong to the [`crate::sema`] pass, which fills the types and reports
//! [`crate::sema::SemaError`]. Type annotations (`int`/`float`/`void`) are the one exception: they
//! are resolved here while building the signature, since that is a fixed lexical mapping.

use crate::lexer::{Token, TokenKind, lex};

mod ast;
mod expr;
mod item;

#[cfg(test)]
mod tests;

pub use ast::*;

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
    /// Parse nesting (parens, ternaries, or block bodies) ran past `MAX_NESTING_DEPTH` — refused
    /// as runaway input before it could overflow the stack, rather than reported as a bad token.
    TooDeeplyNested(String),
    /// A single `if` carried more arms than `MAX_IF_ARMS` — a deliberate sanity cap on an
    /// otherwise-unbounded (but stack-safe) `elif` ladder.
    TooManyArms(String),
}

/// Lexes and parses `src` into a structural [`Ast`] (its `types` left for [`crate::sema::analyze`]
/// to fill). The returned AST borrows `src`. Stops at and returns the first [`ParseError`].
pub fn parse(src: &str) -> Result<Ast<'_>, ParseError> {
    let tokens = lex(src).map_err(|error| ParseError::Lex(error.message))?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        ast: Ast::default(),
        pending_dedents: 0,
        depth: 0,
    };
    parser.parse_program()?;
    Ok(parser.ast)
}

struct Parser<'src> {
    tokens: Vec<Token<'src>>,
    pos: usize,
    ast: Ast<'src>,
    // A wrapped one-line conditional (`… if c` then `else …` on the next line, aligned to the
    // expression's column) opens an `Indent` the layout will later close with a `Dedent`. That
    // `Dedent` is *not* a block terminator, so `parse_conditional` records it here and the
    // enclosing `parse_block` swallows it instead of ending the block. See `parse_conditional`.
    // At the top level there is no enclosing `parse_block`; the stray `Dedent` is harmlessly
    // absorbed by `skip_separators` and this counter is reset each `parse_program` iteration.
    pending_dedents: usize,
    // Current nesting depth across the two recursion hubs `parse_expr` and `parse_block`, bounded by
    // `MAX_NESTING_DEPTH` so adversarially nested input fails fast instead of overflowing the stack.
    depth: usize,
}

// Maximum parse-nesting depth (parenthesised expressions, ternaries, and `if`/`while`/`loop`
// bodies). Real programs nest a few dozen levels at most; the limit exists only so adversarial
// input — thousands of nested `(` — fails fast with an error rather than overflowing the stack.
// Clang's analogous `-fbracket-depth` defaults to the same value.
const MAX_NESTING_DEPTH: usize = 256;

impl<'src> Parser<'src> {
    // -------------------------------------------------------------------------
    // Token cursor
    // -------------------------------------------------------------------------

    // The current token — the next one to be consumed.
    fn peek(&self) -> &Token<'src> {
        &self.tokens[self.pos]
    }

    // The kind of the token `offset` positions ahead — lookahead without consuming.
    fn at(&self, offset: usize) -> TokenKind {
        self.tokens[self.pos + offset].kind
    }

    // Consume the current token and return it.
    fn advance(&mut self) -> Token<'src> {
        let token = self.tokens[self.pos].clone();
        self.pos += 1;
        token
    }

    // Enter one level of recursion (`parse_expr`/`parse_block`), failing fast past the depth limit.
    // The matching `self.depth -= 1` runs on the success path; an error aborts the whole parse, so a
    // leaked count is harmless.
    fn enter_nesting(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err(ParseError::TooDeeplyNested(format!(
                "Nesting is implausibly deep (over {MAX_NESTING_DEPTH} levels); the input looks unbounded."
            )));
        }
        Ok(())
    }

    // Consume the current token if it is `kind`, otherwise report a syntax error.
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

    // Skip any run of layout tokens (newlines, indents, dedents).
    fn skip_separators(&mut self) {
        while matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent
        ) {
            self.pos += 1;
        }
    }
}
