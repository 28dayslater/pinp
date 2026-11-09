// SPDX-License-Identifier: MIT

//! Static analysis: the pass that answers questions about *how values flow* through a program.
//!
//! [`crate::sema`] checks a program by walking its shape — a scope stack and a type per node — and
//! that is enough to accept a correct program and reject an incorrect one. It cannot say whether a
//! value is ever read, or whether a statement can run at all, because those questions are about
//! paths rather than syntax. This layer answers them over the program's flat-level IR — its
//! control-flow graph, made by [`crate::desugar`] and [`crate::flatten`] — with a dataflow solver
//! and checkers written against that solver.
//!
//! Two properties hold throughout:
//!
//! * **Read-only.** Nothing here transforms the program, and the backends never consult it. A
//!   checker can be wrong without a compiled program being wrong.
//! * **Advisory and batched.** Findings are warnings, and a run reports all of them — the opposite
//!   of sema's fail-fast single error, and the reason the layer exists separately.
//!
//! Vocabulary, used by every checker. The graph terms it builds on — basic block, terminator,
//! edge — are defined in [`crate::ir`].
//!
//! * Fact: what an analysis knows at one program point. Each analysis picks its own shape: a bool
//!   for reachability, a set of variables for liveness. See [`dataflow::MergeableFact`].
//! * Transfer function: how one statement changes a fact — the rule that makes an analysis
//!   specific.
//! * Direction: whether facts travel along the edges or against them. A *forward* analysis answers
//!   what has already happened, a *backward* one what is still going to happen.
//! * Boundary: the fact where a function begins, or for a backward analysis where it returns. The
//!   one thing an analysis knows before it starts.
//! * Merge: combining the facts arriving at a point from several paths into one. Merging only ever
//!   adds information, which is what bounds the work.
//! * Fixpoint: the state where re-running every transfer changes no fact. [`dataflow::solve`]
//!   iterates to it, and that is the answer.
//! * Checker: code that reads a solved analysis and reports findings. One per rule, in
//!   [`checks`].
//! * Finding: an issue a checker reports, carrying a rule code, a severity, a position, and a
//!   message. A finding is a warning rather than an error, so a run collects every one instead of
//!   stopping at the first. See [`Diagnostic`].

pub mod checks;
pub mod dataflow;

// The finding type moved to [`crate::diagnostic`] in 0016, when the lexer, parser, and sema began
// reporting through it too. Re-exported here because a checker's findings are still what this
// layer's callers ask for.
pub use crate::diagnostic::{Diagnostic, DiagnosticCode, Severity};

use rustc_hash::FxHashSet;

use crate::desugar::desugar;
use crate::flatten::flatten_program;
use crate::parser::ProgramAst;

/// Runs every checker over a program, returning all findings in source order.
///
/// The program is desugared and flattened for the purpose: the analyses run on request, apart from
/// compiling. The AST must already have been through [`crate::sema::analyze`].
pub fn check(ast: &ProgramAst) -> Vec<Diagnostic> {
    let program = flatten_program(desugar(ast));
    let owned_globals = checks::liveness::owned_globals(&program);
    let no_globals = FxHashSet::default();

    let mut diagnostics = Vec::new();
    for (_, function) in program.functions.iter() {
        let owned = match function.id == program.entry {
            true => &owned_globals,
            false => &no_globals,
        };
        diagnostics.extend(checks::reachability::check(function, &program.tables));
        diagnostics.extend(checks::liveness::check(
            function,
            &program.tables,
            &ast.names,
            owned,
        ));
    }

    // Report top to bottom. Findings are produced per function and per checker, which is neither of
    // those orders; a position with no span sorts last rather than to the top of the file.
    diagnostics.sort_by_key(|diagnostic| match diagnostic.span.is_unknown() {
        true => (u32::MAX, u32::MAX),
        false => (diagnostic.span.start, diagnostic.span.end),
    });
    diagnostics
}
