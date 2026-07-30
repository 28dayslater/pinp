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
//! The layer depends on [`crate::parser`] only, so it builds and tests without the `llvm` feature.

pub mod cfg;
pub mod checks;
pub mod dataflow;
pub mod diagnostic;

pub use diagnostic::{Diagnostic, DiagnosticCode, Severity};

use crate::parser::{ProgramAst, TopLevel};

/// Runs every checker over a program, returning all findings in source order.
///
/// One control-flow graph per function plus one for the top level, each handed to every checker.
/// The AST must already have been through [`crate::sema::analyze`]: the checkers read types and
/// assume names resolve.
pub fn check(ast: &ProgramAst) -> Vec<Diagnostic> {
    let escaping = checks::liveness::escaping_symbols(ast);
    let mut graphs = vec![cfg::Cfg::build_entry(ast)];
    for item in &ast.top_level {
        if let TopLevel::Func(func) = item {
            graphs.push(cfg::Cfg::build_function(ast, func));
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
