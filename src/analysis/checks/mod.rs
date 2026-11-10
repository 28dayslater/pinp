// SPDX-License-Identifier: MIT

//! The checkers: analyses written against [`crate::analysis::dataflow`] that turn solved states
//! into diagnostics. Each is a lattice, transfer functions, and a walk over the solution.

pub mod liveness;
pub mod reachability;
