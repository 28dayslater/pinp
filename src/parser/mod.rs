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
//! - **AST arena** — nodes live in `ProgramAst::nodes: Vec<Node>` and reference each other by
//!   [`ExprId`] (an index), so there is no `Box`/`Rc`. The inferred [`PinpType`] of each node
//!   sits in the parallel `ProgramAst::types` vec at the same index.
//! - **Interner** — identifiers map to [`SymId`] via `ProgramAst::symbols`, backed by `&'src str`
//!   slices borrowed straight from the source (no string copies).
//!
//! Parsing is **purely syntactic**: it builds the structural AST and reports only lexical/layout
//! errors ([`ParseError`]). The `types` arena is left unpopulated — name resolution, scoping, and
//! type inference belong to the [`crate::sema`] pass, which fills the types and reports
//! [`crate::sema::SemaError`]. Type annotations (`int`/`float`/`void`) are the one exception: they
//! are resolved here while building the signature, since that is a fixed lexical mapping.

use crate::diagnostic::{Diagnose, Diagnostic};
use crate::lexer::{Span, Token, TokenKind, lex};

mod ast;
mod expr;
mod item;

#[cfg(test)]
mod tests;

pub use ast::*;

/// A syntactic failure from lexing or parsing, and the source it is about. pinp is fail-fast: the
/// first error returned stops the pass. Semantic errors live in [`crate::sema::SemaError`].
///
/// The span is attached centrally in [`parse`] rather than at each construction site: the parser
/// unwinds the moment a rule fails and never advances afterwards, so the token it stopped on *is*
/// the offending one. The exception is a rule that has already consumed its token — see
/// [`ParseErrorKind::TooDeeplyNested`]'s neighbours in `parse_primary`, which peek before parsing
/// so the literal is still current if it fails.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub span: Span,
}

impl Diagnose for ParseError {
    fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::error(self.span, self.kind.message())
    }
}

/// Comparing an error to a bare kind ignores position, which is what a test asserting *which*
/// failure occurred wants to say.
impl PartialEq<ParseErrorKind> for ParseError {
    fn eq(&self, kind: &ParseErrorKind) -> bool {
        &self.kind == kind
    }
}

/// What kind of syntactic failure occurred. The variants are what a test matches on; the message
/// each carries is what a reader sees.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseErrorKind {
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

impl ParseErrorKind {
    /// The human-readable text, which every variant carries.
    pub fn message(&self) -> &str {
        match self {
            ParseErrorKind::Lex(message)
            | ParseErrorKind::Unexpected(message)
            | ParseErrorKind::Layout(message)
            | ParseErrorKind::TooDeeplyNested(message)
            | ParseErrorKind::TooManyArms(message) => message,
        }
    }
}

/// Lexes and parses `src` into a structural [`ProgramAst`] (its `types` left for
/// [`crate::sema::analyze`] to fill). The returned AST borrows `src`. Stops at and returns the first
/// [`ParseError`].
pub fn parse(src: &str) -> Result<ProgramAst<'_>, ParseError> {
    // A lex failure already knows where it happened; carry that span rather than re-deriving one.
    let tokens = lex(src).map_err(|error| ParseError {
        kind: ParseErrorKind::Lex(error.message),
        span: error.span,
    })?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        ast: ProgramAst::default(),
        pending_dedents: 0,
        depth: 0,
    };
    match parser.parse_program() {
        Ok(()) => Ok(parser.ast),
        // The parse stopped here, so the current token is the one the rule objected to.
        Err(kind) => Err(ParseError {
            kind,
            span: parser.error_span(),
        }),
    }
}

struct Parser<'src> {
    tokens: Vec<Token<'src>>,
    pos: usize,
    ast: ProgramAst<'src>,
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
    fn enter_nesting(&mut self) -> Result<(), ParseErrorKind> {
        self.depth += 1;
        if self.depth > MAX_NESTING_DEPTH {
            return Err(ParseErrorKind::TooDeeplyNested(format!(
                "Nesting is implausibly deep (over {MAX_NESTING_DEPTH} levels); the input looks unbounded."
            )));
        }
        Ok(())
    }

    // Consume the current token if it is `kind`, otherwise report a syntax error.
    fn expect(&mut self, kind: TokenKind) -> Result<Token<'src>, ParseErrorKind> {
        if self.peek().kind == kind {
            Ok(self.advance())
        } else {
            Err(ParseErrorKind::Unexpected(format!(
                "Expected {kind:?}, found {:?}.",
                self.peek().kind
            )))
        }
    }

    // -------------------------------------------------------------------------
    // Spans
    // -------------------------------------------------------------------------

    // Extent of a construct that began at `start` and has just been fully parsed: from `start`
    // through the token most recently consumed. The `max` guards against synthetic layout tokens,
    // which carry the offset of the position they were manufactured at rather than one they occupy,
    // and so can sit behind the construct they close.
    fn span_from(&self, start: Span) -> Span {
        let last_end = self.tokens[self.pos - 1].span().end;
        Span::new(start.start, last_end.max(start.end))
    }

    // Where to report a failure that stopped on the current token.
    //
    // A rule can object while the cursor sits on a synthetic layout token (`Newline`, `Indent`,
    // `Dedent`), which occupies no source and would render as a zero-width position at the end of
    // the previous line. Point at the next token that does occupy source: that is the one the
    // reader has to look at. At end of input there is none, so the synthetic token stands.
    fn error_span(&self) -> Span {
        self.tokens[self.pos..]
            .iter()
            .find(|token| !token.text.is_empty())
            .unwrap_or(self.peek())
            .span()
    }

    // Extent of a composite node that has no token of its own: a binary operator covers both its
    // operands rather than just the operator, which is what a reader needs to see when the two
    // failed to combine. A child whose position was never recorded is skipped rather than dragging
    // the parent's span back to offset 0.
    fn joined_span(&self, lhs: ExprId, rhs: ExprId) -> Span {
        let left_span = self.ast.span_of(lhs);
        let right_span = self.ast.span_of(rhs);
        match (left_span.is_unknown(), right_span.is_unknown()) {
            (false, false) => Span::new(left_span.start, right_span.end),
            (true, false) => right_span,
            (false, true) => left_span,
            (true, true) => Span::UNKNOWN,
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
