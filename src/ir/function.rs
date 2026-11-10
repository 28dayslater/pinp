// SPDX-License-Identifier: MIT

//! [`Function`] and its two shapes: [`Structured`] (a body of nested statements) and [`Flat`]
//! (basic blocks joined by [`Terminator`]s — the control-flow graph).

use super::{
    Arena, BlockId, ConstantId, FunctionId, IdVec, Level, List, NodeId, Position, SideTable,
    VerifyError,
};

/// One function: its nodes and, depending on the level, how its statements are arranged.
#[derive(Debug, Clone)]
pub struct Function<S: Shape> {
    pub id: FunctionId,
    pub parameter_count: u32,
    pub arena: Arena,
    pub shape: S,
}

/// How a function's statements are arranged; fixes the function's [`Level`].
pub trait Shape {
    const LEVEL: Level;

    /// Visits the root of every tree the function holds, in execution order, with the position
    /// each occupies.
    fn for_each_root(&self, arena: &Arena, visit: impl FnMut(NodeId, Position));

    /// Checks everything but the trees: that every block referred to exists.
    fn verify_blocks(&self, arena: &Arena) -> Result<(), VerifyError>;
}

/// The structured level: one body, whose statements may nest bodies of their own.
#[derive(Debug, Clone, Copy)]
pub struct Structured {
    pub body: List<NodeId>,
    /// The value the function returns, computed after the body.
    pub result: Option<NodeId>,
}

impl Shape for Structured {
    const LEVEL: Level = Level::Structured;

    fn for_each_root(&self, arena: &Arena, mut visit: impl FnMut(NodeId, Position)) {
        for &statement in arena.items(self.body) {
            visit(statement, Position::Statement);
        }
        if let Some(result) = self.result {
            visit(result, Position::Value);
        }
    }

    fn verify_blocks(&self, _arena: &Arena) -> Result<(), VerifyError> {
        Ok(())
    }
}

/// How a basic block ends: where control goes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminator {
    Goto(BlockId),
    Branch {
        cond: NodeId,
        then_block: BlockId,
        else_block: BlockId,
    },
    /// Continues at `targets[tag]`, or at `default` when `tag` names no target — the flat form of
    /// a `match` on an algebraic data type's tag.
    Switch {
        tag: NodeId,
        targets: List<BlockId>,
        default: BlockId,
    },
    Return(Option<NodeId>),
    /// Stops the program with this message.
    RuntimeError(ConstantId),
}

const _: () = assert!(size_of::<Terminator>() == 16);

impl Terminator {
    /// The node the terminator reads, if any.
    pub fn operand(&self) -> Option<NodeId> {
        match *self {
            Terminator::Branch { cond, .. } => Some(cond),
            Terminator::Switch { tag, .. } => Some(tag),
            Terminator::Return(value) => value,
            Terminator::Goto(_) | Terminator::RuntimeError(_) => None,
        }
    }
}

/// A straight run of statements, then a terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicBlock {
    pub statements: List<NodeId>,
    pub terminator: Terminator,
}

/// The flat level: the control-flow graph.
#[derive(Debug, Clone)]
pub struct Flat {
    pub blocks: IdVec<BlockId, BasicBlock>,
    pub entry: BlockId,
    /// The range each counted loop's header block tests, by header: what the loop's structure
    /// was, which its blocks no longer show.
    pub counted_loop_ranges: SideTable<BlockId, Option<NodeId>>,
}

impl Flat {
    /// Every block a terminator can continue at, one entry per edge.
    fn for_each_successor(&self, arena: &Arena, block_id: BlockId, mut visit: impl FnMut(BlockId)) {
        match self.blocks[block_id].terminator {
            Terminator::Goto(target) => visit(target),
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => {
                visit(then_block);
                visit(else_block);
            }
            Terminator::Switch {
                targets, default, ..
            } => {
                arena
                    .items(targets)
                    .iter()
                    .for_each(|&target| visit(target));
                visit(default);
            }
            Terminator::Return(_) | Terminator::RuntimeError(_) => {}
        }
    }
}

impl Shape for Flat {
    const LEVEL: Level = Level::Flat;

    fn for_each_root(&self, arena: &Arena, mut visit: impl FnMut(NodeId, Position)) {
        for (_, block) in self.blocks.iter() {
            for &statement in arena.items(block.statements) {
                visit(statement, Position::Statement);
            }
            if let Some(operand) = block.terminator.operand() {
                visit(operand, Position::Value);
            }
        }
    }

    fn verify_blocks(&self, arena: &Arena) -> Result<(), VerifyError> {
        let exists = |block_id: BlockId| match self.blocks.get(block_id) {
            Some(_) => Ok(()),
            None => Err(VerifyError::DanglingBlock(block_id)),
        };
        exists(self.entry)?;
        let mut targets = Vec::new();
        for (block_id, _) in self.blocks.iter() {
            self.for_each_successor(arena, block_id, |target| targets.push(target));
        }
        targets.into_iter().try_for_each(exists)
    }
}

impl Function<Flat> {
    /// The blocks control can pass to from `block_id`, one entry per edge: a branch whose targets
    /// coincide lists that block twice.
    pub fn successors(&self, block_id: BlockId) -> Vec<BlockId> {
        let mut successors = Vec::new();
        self.shape
            .for_each_successor(&self.arena, block_id, |target| successors.push(target));
        successors
    }

    /// Every block's predecessors, one entry per edge, in block order. Unreachable blocks count:
    /// reachability is an analysis, not a property of the graph.
    pub fn predecessors(&self) -> SideTable<BlockId, Vec<BlockId>> {
        let mut predecessors: SideTable<BlockId, Vec<BlockId>> = SideTable::default();
        for (block_id, _) in self.shape.blocks.iter() {
            self.shape
                .for_each_successor(&self.arena, block_id, |target| {
                    predecessors[target].push(block_id)
                });
        }
        predecessors
    }
}
