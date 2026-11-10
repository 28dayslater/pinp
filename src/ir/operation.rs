// SPDX-License-Identifier: MIT

//! [`Operation`], the one node type of both levels, and what can be asked of a single node.

use super::{ConstantId, FunctionId, GlobalId, List, LocalId, NodeId};
use crate::parser::{BinOp, RangeKind, UnOp};

/// A level of the IR: which operations may still appear in a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Nested control flow: `If`, `While`, `DoWhile`, `CountedLoop`.
    Structured,
    /// Basic blocks and terminators; no nested control flow. Every value operation is allowed at
    /// both levels.
    Flat,
}

/// Where a node sits: in a body, run for its effect, or under another node, computing a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Statement,
    Value,
}

/// How many bytes a `Load` or `Store` moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    /// 1 byte (a `bool` array element).
    Byte,
    /// 8 bytes (an `int` or `float`).
    Word,
}

/// Which part of a range value a `RangeField` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeFieldKind {
    Start,
    Stop,
    /// Never zero: building a range with a zero step is a runtime error.
    Step,
    /// Whether the stop is included (`..`), as a `bool`.
    Inclusive,
}

/// A function of pinp's runtime library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFunction {
    /// Joins its string arguments into a new string; no arguments give the empty string.
    StrConcat,
    /// A new string with the same content: what storing a string another variable still owns
    /// takes.
    StrCopy,
    /// Releases a string's storage, evaluated for its effect. Freeing the empty string a variable
    /// holds before its first write is harmless.
    StrFree,
    StrLength,
    /// Prints the allocator's statistics.
    MemInfo,
}

impl RuntimeFunction {
    /// How the printer spells the function.
    pub fn name(self) -> &'static str {
        match self {
            RuntimeFunction::StrConcat => "str_concat",
            RuntimeFunction::StrCopy => "str_copy",
            RuntimeFunction::StrFree => "str_free",
            RuntimeFunction::StrLength => "str_length",
            RuntimeFunction::MemInfo => "meminfo",
        }
    }
}

/// One node. Every payload is `u32` ids plus a few one-byte fields, so a node is 16 bytes;
/// anything larger lives out of line behind an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    // Both levels: every value operation, and the statements that are not control flow.
    Constant(ConstantId),
    ReadLocal(LocalId),
    ReadGlobal(GlobalId),
    WriteLocal {
        local_id: LocalId,
        value: NodeId,
    },
    WriteGlobal {
        global_id: GlobalId,
        value: NodeId,
    },
    /// Computes a value and discards it (a call made for its effect).
    Evaluate(NodeId),
    Unary {
        op: UnOp,
        operand: NodeId,
    },
    /// Never `and`/`or` (they short-circuit, so they become `If`) nor `div`/`mod`
    /// (`IntegerDivide`).
    Binary {
        op: BinOp,
        lhs: NodeId,
        rhs: NodeId,
    },
    /// `div` or, with `remainder`, `mod`. While `checked`, a zero divisor or an overflowing
    /// quotient raises a runtime error.
    IntegerDivide {
        remainder: bool,
        checked: bool,
        lhs: NodeId,
        rhs: NodeId,
    },
    /// Converts `operand` to the node's own type (an implicit promotion made explicit).
    Convert {
        operand: NodeId,
    },
    /// One part of a range value; how a flattened counted loop reads its range.
    RangeField {
        range: NodeId,
        field: RangeFieldKind,
    },
    /// `cond ? if_true : if_false`. Both values are computed, so they must have no effects.
    Select {
        cond: NodeId,
        if_true: NodeId,
        if_false: NodeId,
    },
    Call {
        callee: FunctionId,
        args: List<NodeId>,
    },
    RuntimeCall {
        function: RuntimeFunction,
        args: List<NodeId>,
    },
    /// The address of `array`'s element `index` (already one flat index).
    ElementAddress {
        array: NodeId,
        index: NodeId,
    },
    Load {
        address: NodeId,
        offset: u32,
        stored: ScalarKind,
    },
    Store {
        address: NodeId,
        value: NodeId,
        offset: u32,
        stored: ScalarKind,
    },

    /// An explicit `step` may be zero, which raises a runtime error.
    Range {
        kind: RangeKind,
        start: NodeId,
        stop: NodeId,
        step: Option<NodeId>,
    },
    /// `value in range`.
    Membership {
        value: NodeId,
        range: NodeId,
    },
    ArrayNew {
        elements: List<NodeId>,
    },
    /// A new array of the node's type, its elements zeroed or left unset.
    ArrayAllocate {
        zeroed: bool,
    },
    /// While `checked`, an index out of range raises a runtime error.
    ElementGet {
        array: NodeId,
        indices: List<NodeId>,
        checked: bool,
    },
    ElementSet {
        array: NodeId,
        indices: List<NodeId>,
        value: NodeId,
        checked: bool,
    },
    Slice {
        source: NodeId,
        selectors: List<NodeId>,
    },

    // Structured level only.
    If {
        cond: NodeId,
        then_body: List<NodeId>,
        else_body: List<NodeId>,
    },
    /// Runs `header`, tests `cond`, runs `body`, repeats. The header holds what computing the
    /// condition needs, such as an if-expression's temporary.
    While {
        header: List<NodeId>,
        cond: NodeId,
        body: List<NodeId>,
    },
    /// Runs `body`, then repeats while `cond` holds — or, with `until`, while it does not.
    DoWhile {
        body: List<NodeId>,
        cond: NodeId,
        until: bool,
    },
    /// Runs `body` once for each value of `range`, held in `index`.
    CountedLoop {
        index: LocalId,
        range: NodeId,
        body: List<NodeId>,
    },
}

const _: () = assert!(size_of::<Operation>() == 16);

impl Operation {
    /// Whether this operation may appear at `level`.
    pub fn allowed_in(&self, level: Level) -> bool {
        use Operation::*;
        match self {
            Binary { op, .. } => !matches!(op, BinOp::And | BinOp::Or | BinOp::IntDiv | BinOp::Mod),
            If { .. } | While { .. } | DoWhile { .. } | CountedLoop { .. } => {
                level == Level::Structured
            }
            _ => true,
        }
    }

    /// Whether this operation is run for its effect (sits in a body) rather than computing a value.
    pub fn is_statement(&self) -> bool {
        use Operation::*;
        matches!(
            self,
            WriteLocal { .. }
                | WriteGlobal { .. }
                | Evaluate(_)
                | ElementSet { .. }
                | Store { .. }
                | If { .. }
                | While { .. }
                | DoWhile { .. }
                | CountedLoop { .. }
        )
    }

    /// Whether this node itself may raise a runtime error; its children are not considered.
    pub fn may_trap(&self) -> bool {
        use Operation::*;
        match self {
            IntegerDivide { checked, .. }
            | ElementGet { checked, .. }
            | ElementSet { checked, .. } => *checked,
            Range { step, .. } => step.is_some(),
            Call { .. } | RuntimeCall { .. } | Slice { .. } => true,
            _ => false,
        }
    }

    /// Whether this node itself changes state other than its own result: memory, a variable, or
    /// the heap. Its children are not considered.
    pub fn has_effects(&self) -> bool {
        use Operation::*;
        matches!(
            self,
            WriteLocal { .. }
                | WriteGlobal { .. }
                | Call { .. }
                | RuntimeCall { .. }
                | ArrayNew { .. }
                | ArrayAllocate { .. }
                | ElementSet { .. }
                | Store { .. }
        )
    }
}
