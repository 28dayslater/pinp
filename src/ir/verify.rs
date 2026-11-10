// SPDX-License-Identifier: MIT

//! [`verify`]: the IR's invariants. Desugaring and flattening check their output in debug builds,
//! and every later pass that builds or edits a function is to do the same.

use super::{BlockId, Function, Level, NodeId, Operation, Position, Shape, SideTable};

/// A broken invariant — always a compiler bug, never a problem with the pinp program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// The node is reached twice (or reaches itself): the function is not made of trees.
    SharedNode(NodeId),
    /// A statement where a value belongs, or a value where a statement belongs.
    WrongPosition(NodeId),
    /// The operation may not appear at the function's level.
    WrongLevel(NodeId, Level),
    /// The id names no node of this function.
    DanglingNode(NodeId),
    /// The id names no block of this function.
    DanglingBlock(BlockId),
    /// The node names a local this function does not have.
    DanglingLocal(NodeId),
    Untyped(NodeId),
    Unspanned(NodeId),
}

/// Checks that `function` is well formed: every block it refers to exists; every node is reached
/// exactly once, exists, has a type and a span, sits in the position its kind requires, is allowed
/// at the function's level, and refers only to existing locals.
pub fn verify<S: Shape>(function: &Function<S>) -> Result<(), VerifyError> {
    let arena = &function.arena;
    function.shape.verify_blocks(arena)?;
    let mut seen: SideTable<NodeId, bool> = SideTable::default();
    let mut pending = Vec::new();
    function
        .shape
        .for_each_root(arena, |root, position| pending.push((root, position)));
    pending.reverse();
    while let Some((node_id, position)) = pending.pop() {
        let Some(&operation) = arena.operations.get(node_id) else {
            return Err(VerifyError::DanglingNode(node_id));
        };
        if seen[node_id] {
            return Err(VerifyError::SharedNode(node_id));
        }
        seen[node_id] = true;
        if arena.types[node_id].is_none() {
            return Err(VerifyError::Untyped(node_id));
        }
        if arena.spans[node_id].is_unknown() {
            return Err(VerifyError::Unspanned(node_id));
        }
        if operation.is_statement() != (position == Position::Statement) {
            return Err(VerifyError::WrongPosition(node_id));
        }
        if !operation.allowed_in(S::LEVEL) {
            return Err(VerifyError::WrongLevel(node_id, S::LEVEL));
        }
        let local_id = match operation {
            Operation::ReadLocal(local_id)
            | Operation::WriteLocal { local_id, .. }
            | Operation::CountedLoop {
                index: local_id, ..
            } => Some(local_id),
            _ => None,
        };
        if local_id.is_some_and(|local_id| arena.locals.get(local_id).is_none()) {
            return Err(VerifyError::DanglingLocal(node_id));
        }
        let first_child = pending.len();
        arena.for_each_child_at(node_id, |child, position| pending.push((child, position)));
        pending[first_child..].reverse();
    }
    Ok(())
}
