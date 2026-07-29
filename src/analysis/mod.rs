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
pub mod diagnostic;

pub use diagnostic::{Diagnostic, DiagnosticCode, Severity};
