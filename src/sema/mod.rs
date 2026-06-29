// SPDX-License-Identifier: MIT

//! Semantic analysis: the pass between [`crate::parser`] and code generation.
//!
//! The parser produces a structural [`ProgramAst`] with an unpopulated `types` arena. [`analyze`]
//! walks it, **resolves names** against a scope stack, **infers** every expression's [`PinpType`]
//! (writing it back into the arena), and **checks** the semantic rules — reporting the first
//! [`SemaError`]. Codegen then consumes the now-typed AST.
//!
//! Scoping mirrors the language: `scopes[0]` is global; a function pushes a frame seeded with its
//! parameters. A bare name resolves against the innermost frame only (never reaching globals from
//! inside a function); `::name` always targets the global frame. Function signatures are recorded
//! *after* each definition is analysed, giving define-before-use with no recursion.

use rustc_hash::FxHashMap;

use crate::parser::{BuiltinMember, Node, PinpType, ProgramAst, SymId, TopLevel};

mod analyzer;

#[cfg(test)]
mod tests;

/// A semantic failure. pinp is fail-fast: the first error stops the pass.
#[derive(Debug, Clone, PartialEq)]
pub enum SemaError {
    /// A name used but not bound: unknown variable, global, or function.
    UnknownSymbol(String),
    /// A type error: a mismatch, a bad promotion, or an operation on `Void`.
    Type(String),
}

// ---------------------------------------------------------------------------
// Type lattice helpers
// ---------------------------------------------------------------------------

/// `from` is assignable to `to` if identical, or by widening up the promotion lattice
/// `Bool -> Int -> Float`. Narrowing (e.g. `Float -> Int`, anything `-> Bool`) is never implicit.
fn assignable(from: PinpType, to: PinpType) -> bool {
    use PinpType::*;
    from == to || matches!((from, to), (Bool, Int) | (Bool, Float) | (Int, Float))
}

/// The wider of two types under the `Bool -> Int -> Float` lattice — the common type an `if`'s
/// branches join to. `None` when neither is assignable to the other (e.g. a `Void` branch), which
/// makes the whole `if` `Void` and so usable only as a statement.
fn join(left: PinpType, right: PinpType) -> Option<PinpType> {
    if assignable(left, right) {
        Some(right)
    } else if assignable(right, left) {
        Some(left)
    } else {
        None
    }
}

/// One dimension's selector in a `Node::Index2D` expression after sema classification.
enum DimSelector {
    /// A scalar (`Int`/`Bool`) — bounds-checked at runtime.
    Index,
    /// A range or FullExtent — compile-time bounds-checked; carries the slice length.
    Slice(usize),
}

/// Whether `pinp_type` is a scalar arithmetic/comparison operand — `Bool`/`Int`/`Float`, not `Void`
/// and not an aggregate like `Range`.
fn numeric(pinp_type: PinpType) -> bool {
    matches!(pinp_type, PinpType::Bool | PinpType::Int | PinpType::Float)
}

/// Whether `pinp_type` promotes to `Int` (rather than forcing a `Float` result): `Bool` or `Int`.
fn int_like(pinp_type: PinpType) -> bool {
    matches!(pinp_type, PinpType::Bool | PinpType::Int)
}

// ---------------------------------------------------------------------------
// The analysis pass
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Signature {
    params: Vec<PinpType>,
    return_type: PinpType,
}

/// Infers types and checks the semantic rules, filling the AST's `types` and `builtin_members`
/// arenas in place.
pub fn analyze(ast: &mut ProgramAst) -> Result<(), SemaError> {
    // Borrow the read-only structure and the writable arenas as disjoint fields.
    let ProgramAst {
        nodes,
        types,
        builtin_members,
        top_level,
        names,
        ..
    } = ast;
    let mut analyzer = Analyzer {
        nodes,
        names,
        types,
        builtin_members,
        scopes: vec![FxHashMap::default()], // global frame
        fn_base: 0,
        funcs: FxHashMap::default(),
        loop_vars: Vec::new(),
    };
    for item in top_level.iter() {
        match item {
            TopLevel::Stmt(stmt) => analyzer.analyze_stmt(stmt)?,
            TopLevel::Func(func) => analyzer.analyze_func(func)?,
        }
    }
    Ok(())
}

struct Analyzer<'ast, 'src> {
    nodes: &'ast [Node],
    names: &'ast [&'src str],
    types: &'ast mut Vec<PinpType>,
    /// Parallel to `nodes`; sema writes the resolved [`BuiltinMember`] for every `Node::Member`
    /// expression so codegen can pattern-match the enum without repeating string comparisons.
    builtin_members: &'ast mut Vec<Option<BuiltinMember>>,
    scopes: Vec<FxHashMap<SymId, PinpType>>,
    // Index of the current function's base frame; bare-name resolution searches `scopes[fn_base..]`
    // and never reaches the global frame from inside a function. `0` at the top level, where the
    // global frame *is* the base.
    fn_base: usize,
    funcs: FxHashMap<SymId, Signature>,
    // The loop variables of the enclosing `for`s; each is read-only inside its body, so an
    // assignment to one is rejected rather than allowed to corrupt the iteration counter.
    loop_vars: Vec<SymId>,
}
