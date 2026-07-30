// SPDX-License-Identifier: MIT

//! The checkers: analyses written against [`crate::analysis::dataflow`] that turn facts into
//! findings. Each is a fact type, a transfer function, and a walk over the solution.

pub mod liveness;
pub mod reachability;
