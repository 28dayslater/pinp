// SPDX-License-Identifier: MIT

//! [`Arena`]: one function's nodes, their side tables, locals, and lists — and walks over them.

use super::{
    IdVec, List, ListPool, Local, LocalId, NodeId, Operation, Position, SideTable, TypeId, TypedId,
};
use crate::lexer::Span;

/// Everything one function's nodes are made of, stored as parallel tables indexed by id.
#[derive(Debug, Clone)]
pub struct Arena {
    pub operations: IdVec<NodeId, Operation>,
    /// `None` until a type is recorded; `verify` rejects a node left without one.
    pub types: SideTable<NodeId, Option<TypeId>>,
    /// `Span::UNKNOWN` until recorded; `verify` rejects a node left without one.
    pub spans: SideTable<NodeId, Span>,
    pub locals: IdVec<LocalId, Local>,
    pub lists: ListPool,
}

impl Default for Arena {
    fn default() -> Self {
        Arena {
            operations: IdVec::default(),
            types: SideTable::default(),
            spans: SideTable::new(Span::UNKNOWN),
            locals: IdVec::default(),
            lists: ListPool::default(),
        }
    }
}

impl Arena {
    /// Adds a node with its type and source span.
    pub fn push(&mut self, operation: Operation, type_id: TypeId, span: Span) -> NodeId {
        let node_id = self.operations.push(operation);
        self.types[node_id] = Some(type_id);
        self.spans[node_id] = span;
        node_id
    }

    /// A new list holding `items`.
    pub fn list<T: TypedId>(&mut self, items: &[T]) -> List<T> {
        self.lists.list(items)
    }

    pub fn items<T: TypedId>(&self, list: List<T>) -> &[T] {
        self.lists.items(list)
    }

    /// Forgets everything, ready for the next function; ids start again from zero.
    pub fn clear(&mut self) {
        self.operations.clear();
        self.types.clear();
        self.spans.clear();
        self.locals.clear();
        self.lists.clear();
    }

    /// Visits a node's children in execution order, each with the position it occupies.
    pub fn for_each_child_at(&self, node_id: NodeId, mut visit: impl FnMut(NodeId, Position)) {
        use Operation::*;
        use Position::{Statement, Value};
        let each =
            |list: List<NodeId>, position: Position, visit: &mut dyn FnMut(NodeId, Position)| {
                for &child in self.items(list) {
                    visit(child, position);
                }
            };
        match self.operations[node_id] {
            Constant(_) | ReadLocal(_) | ReadGlobal(_) | ArrayAllocate { .. } => {}
            WriteLocal { value, .. } | WriteGlobal { value, .. } | Evaluate(value) => {
                visit(value, Value)
            }
            Unary { operand, .. } | Convert { operand } => visit(operand, Value),
            RangeField { range, .. } => visit(range, Value),
            Load { address, .. } => visit(address, Value),
            Binary { lhs, rhs, .. } | IntegerDivide { lhs, rhs, .. } => {
                visit(lhs, Value);
                visit(rhs, Value);
            }
            Membership { value, range } => {
                visit(value, Value);
                visit(range, Value);
            }
            ElementAddress { array, index } => {
                visit(array, Value);
                visit(index, Value);
            }
            Store { address, value, .. } => {
                visit(address, Value);
                visit(value, Value);
            }
            Select {
                cond,
                if_true,
                if_false,
            } => {
                visit(cond, Value);
                visit(if_true, Value);
                visit(if_false, Value);
            }
            Call { args, .. } | RuntimeCall { args, .. } => each(args, Value, &mut visit),
            ArrayNew { elements } => each(elements, Value, &mut visit),
            Range {
                start, stop, step, ..
            } => {
                visit(start, Value);
                visit(stop, Value);
                if let Some(step) = step {
                    visit(step, Value);
                }
            }
            ElementGet { array, indices, .. } => {
                visit(array, Value);
                each(indices, Value, &mut visit);
            }
            ElementSet {
                array,
                indices,
                value,
                ..
            } => {
                visit(array, Value);
                each(indices, Value, &mut visit);
                visit(value, Value);
            }
            Slice { source, selectors } => {
                visit(source, Value);
                each(selectors, Value, &mut visit);
            }
            If {
                cond,
                then_body,
                else_body,
            } => {
                visit(cond, Value);
                each(then_body, Statement, &mut visit);
                each(else_body, Statement, &mut visit);
            }
            While { header, cond, body } => {
                each(header, Statement, &mut visit);
                visit(cond, Value);
                each(body, Statement, &mut visit);
            }
            DoWhile { body, cond, .. } => {
                each(body, Statement, &mut visit);
                visit(cond, Value);
            }
            CountedLoop { range, body, .. } => {
                visit(range, Value);
                each(body, Statement, &mut visit);
            }
        }
    }

    /// Visits a node's children in execution order.
    pub fn for_each_child(&self, node_id: NodeId, mut visit: impl FnMut(NodeId)) {
        self.for_each_child_at(node_id, |child, _| visit(child));
    }

    /// Visits every node of the tree under `root`, root first. Iterative, so a deep tree cannot
    /// overflow the stack.
    pub fn for_each_in_tree(&self, root: NodeId, mut visit: impl FnMut(NodeId)) {
        let mut pending = vec![root];
        while let Some(node_id) = pending.pop() {
            visit(node_id);
            let first_child = pending.len();
            self.for_each_child(node_id, |child| pending.push(child));
            pending[first_child..].reverse();
        }
    }

    /// Visits every local read in the tree under `root`, once per read. Write targets and loop
    /// indices are not reads.
    pub fn for_each_read(&self, root: NodeId, mut visit: impl FnMut(LocalId)) {
        self.for_each_in_tree(root, |node_id| {
            if let Operation::ReadLocal(local_id) = self.operations[node_id] {
                visit(local_id);
            }
        });
    }

    /// Whether the tree under `root` can be deleted without changing what the program does:
    /// nothing in it has an effect or may raise a runtime error.
    pub fn is_removable(&self, root: NodeId) -> bool {
        let mut removable = true;
        self.for_each_in_tree(root, |node_id| {
            let operation = self.operations[node_id];
            removable &= !operation.may_trap() && !operation.has_effects();
        });
        removable
    }
}
