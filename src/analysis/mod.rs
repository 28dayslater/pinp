// SPDX-License-Identifier: MIT

//! Static analysis: the pass that answers questions about *how values flow* through a program.
//!
//! [`crate::sema`] checks a program by walking its shape — a scope stack and a type per node — and
//! that is enough to accept a correct program and reject an incorrect one. It cannot say whether a
//! value is ever read, or whether a statement can run at all, because those questions are about
//! paths rather than syntax. This layer builds the machinery for them: a control-flow graph, a
//! dataflow solver over it, and checkers written against that solver.
//!
//! Two properties hold throughout:
//!
//! * **Read-only.** Nothing here transforms the program, and codegen never consults it. A checker
//!   can be wrong without a compiled program being wrong.
//! * **Advisory and batched.** Findings are warnings, and a run reports all of them — the opposite
//!   of sema's fail-fast single error, and the reason the layer exists separately.
//!
//! Vocabulary, used by every checker. The graph terms it builds on — basic block, terminator,
//! edge, entry and exit — are defined in [`crate::cfg`].
//!
//! * Fact: what an analysis knows at one program point. Each analysis picks its own shape: a bool
//!   for reachability, a set of symbols for liveness. See [`dataflow::MergeableFact`].
//! * Transfer function: how one action changes a fact — the rule that makes an analysis specific.
//! * Direction: whether facts travel along the edges or against them. A *forward* analysis answers
//!   what has already happened, a *backward* one what is still going to happen.
//! * Boundary: the fact where a graph begins, or for a backward analysis where it ends. The one
//!   thing an analysis knows before it starts.
//! * Merge: combining the facts arriving at a point from several paths into one. Merging only ever
//!   adds information, which is what bounds the work.
//! * Fixpoint: the state where re-running every transfer changes no fact. [`dataflow::solve`]
//!   iterates to it, and that is the answer.
//! * Checker: code that reads a solved analysis and reports findings. One per rule, in
//!   [`checks`].
//! * Finding: an issue a checker reports, carrying a rule code, a severity, a position, and a
//!   message. A finding is a warning rather than an error, so a run collects every one instead of
//!   stopping at the first. See [`Diagnostic`].
//!
//! The layer depends on [`crate::parser`] and [`crate::cfg`] only, so it builds and tests without
//! the `llvm` feature.

pub mod checks;
pub mod dataflow;

// The finding type moved to [`crate::diagnostic`] in 0016, when the lexer, parser, and sema began
// reporting through it too. Re-exported here because a checker's findings are still what this
// layer's callers ask for.
pub use crate::diagnostic::{Diagnostic, DiagnosticCode, Severity};

use crate::cfg::Cfg;
use crate::parser::{ProgramAst, TopLevel};

/// Runs every checker over a program, returning all findings in source order.
///
/// One control-flow graph per function plus one for the top level, each handed to every checker.
/// The AST must already have been through [`crate::sema::analyze`]: the checkers read types and
/// assume names resolve.
pub fn check(ast: &ProgramAst) -> Vec<Diagnostic> {
    let escaping = checks::liveness::escaping_symbols(ast);
    let mut graphs = vec![Cfg::build_entry(ast)];
    for item in &ast.top_level {
        if let TopLevel::Func(func) = item {
            graphs.push(Cfg::build_function(ast, func));
        }
    }

    let mut diagnostics = Vec::new();
    for graph in &graphs {
        diagnostics.extend(checks::reachability::check(ast, graph));
        diagnostics.extend(checks::liveness::check(ast, graph, &escaping));
    }

    // Report top to bottom. Findings are produced per graph and per checker, which is neither of
    // those orders; a position with no span sorts last rather than to the top of the file.
    diagnostics.sort_by_key(|diagnostic| match diagnostic.span.is_unknown() {
        true => (u32::MAX, u32::MAX),
        false => (diagnostic.span.start, diagnostic.span.end),
    });
    diagnostics
}
