// SPDX-License-Identifier: MIT

//! Control-flow graph construction: a program's shape rewritten as basic blocks and the edges
//! between them.
//!
//! An AST says what a program is written as; a CFG says what order it can run in. Questions about
//! paths — can this statement run, is this value read later — are answerable over the second and
//! not the first, which is why [`crate::analysis`] is built on this module.
//!
//! Vocabulary, used by every module downstream of this one:
//!
//! * Control-flow graph (CFG): one function body as blocks joined by edges, one edge for every way
//!   execution can pass from one block to the next. Built per function, plus one for the top level.
//! * Basic block: a run of actions with no branching inside it. Once entered, every action
//!   executes, and control leaves only through the terminator. That is what lets an analysis reason
//!   about a whole block at a time instead of statement by statement.
//! * Action: one step inside a block — a statement, an expression evaluated for its value, or a
//!   loop variable taking its value. See [`Action`].
//! * Terminator: how control leaves a block — unconditionally, on a branch, or not at all. See
//!   [`Terminator`].
//! * Edge: a possible transfer of control from one block to another, as named by a terminator.
//! * Entry, exit: the block execution starts at, and the single block every path ends at.
//! * Predecessor, successor: the blocks an edge arrives from, and the blocks it leads to.
//!
//! Nothing is lowered to a new instruction set — the blocks hold borrowed AST statements — so this
//! is *construction*, not the lowering that `codegen` does to LLVM IR.

use crate::parser::{Block, ExprId, FuncDef, Node, ProgramAst, Stmt, SymId, TopLevel};

/// An index into [`Cfg::blocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

impl BlockId {
    pub fn value(self) -> usize {
        self.0 as usize
    }
}

/// One thing a basic block does, in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action<'ast> {
    /// Execute a statement.
    Statement(&'ast Stmt),
    /// Evaluate an expression for its value — a nested block's trailing result, whose value feeds
    /// the construct around it.
    Evaluate(ExprId),
    /// A loop variable taking its value for the current iteration: a write with no statement of its
    /// own, modelled so an analysis sees the definition.
    Bind(SymId),
}

/// How control leaves a basic block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Terminator {
    /// Continue unconditionally.
    Goto(BlockId),
    /// Two-way branch on a condition.
    Branch {
        cond: ExprId,
        then_block: BlockId,
        else_block: BlockId,
    },
    /// Leave the function, with its result if it has one. Its sole successor is [`Cfg::exit`].
    ///
    /// **Any number of blocks may carry this.** Exactly one does today — the body's trailing
    /// result — but an explicit `return` will produce several, and nothing here assumes otherwise.
    Return(Option<ExprId>),
    /// The unique sink, carried only by [`Cfg::exit`]. No successors.
    Exit,
}

/// A run of actions ending in exactly one terminator.
#[derive(Debug, Clone, PartialEq)]
pub struct BasicBlock<'ast> {
    pub actions: Vec<Action<'ast>>,
    pub terminator: Terminator,
}

/// The control-flow graph of one function, or of the top-level program.
#[derive(Debug)]
pub struct Cfg<'ast> {
    blocks: Vec<BasicBlock<'ast>>,
    entry: BlockId,
    exit: BlockId,
    predecessors: Vec<Vec<BlockId>>,
}

impl<'ast> Cfg<'ast> {
    /// The graph of a function body.
    pub fn build_function(ast: &'ast ProgramAst, func: &'ast FuncDef) -> Cfg<'ast> {
        let mut builder = CfgBuilder::new(ast);
        let entry = builder.new_block();
        let fallthrough = builder.build_stmts(&func.body.stmts, entry);
        // The body's trailing result is the value the function returns.
        if let Some(reached) = fallthrough {
            builder.set_terminator(reached, Terminator::Return(func.body.result));
        }
        builder.finish(entry)
    }

    /// The graph of the top-level program: every statement, in order, skipping function
    /// definitions (each of which gets its own graph).
    pub fn build_entry(ast: &'ast ProgramAst) -> Cfg<'ast> {
        let stmts: Vec<&'ast Stmt> = ast
            .top_level
            .iter()
            .filter_map(|item| match item {
                TopLevel::Stmt(stmt) => Some(stmt),
                TopLevel::Func(_) => None,
            })
            .collect();
        let mut builder = CfgBuilder::new(ast);
        let entry = builder.new_block();
        let fallthrough = builder.build_stmt_refs(&stmts, entry);
        // A program's value is its last statement's, so there is no separate result expression.
        if let Some(reached) = fallthrough {
            builder.set_terminator(reached, Terminator::Return(None));
        }
        builder.finish(entry)
    }

    pub fn blocks(&self) -> &[BasicBlock<'ast>] {
        &self.blocks
    }

    pub fn block(&self, block_id: BlockId) -> &BasicBlock<'ast> {
        &self.blocks[block_id.value()]
    }

    pub fn entry(&self) -> BlockId {
        self.entry
    }

    pub fn exit(&self) -> BlockId {
        self.exit
    }

    pub fn block_ids(&self) -> impl Iterator<Item = BlockId> {
        (0..self.blocks.len()).map(|index| BlockId(index as u32))
    }

    /// Blocks control can reach directly from `block_id`.
    pub fn successors(&self, block_id: BlockId) -> Vec<BlockId> {
        match self.blocks[block_id.value()].terminator {
            Terminator::Goto(target) => vec![target],
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => vec![then_block, else_block],
            Terminator::Return(_) => vec![self.exit],
            Terminator::Exit => Vec::new(),
        }
    }

    /// Blocks that reach `block_id` directly. Computed once at construction, since a backward
    /// analysis walks these on every pass.
    pub fn predecessors(&self, block_id: BlockId) -> &[BlockId] {
        &self.predecessors[block_id.value()]
    }

    /// Every block that leaves the function. One today; several once `return` exists.
    pub fn returning_blocks(&self) -> Vec<BlockId> {
        self.block_ids()
            .filter(|id| matches!(self.blocks[id.value()].terminator, Terminator::Return(_)))
            .collect()
    }

    /// The graph in Graphviz `dot` form. A test that fails on structure is far easier to read as a
    /// picture than as a list of indices.
    pub fn to_dot(&self) -> String {
        let mut out = String::from("digraph cfg {\n");
        for block_id in self.block_ids() {
            out.push_str(&format!(
                "  {} [label=\"{}: {} action(s)\"];\n",
                block_id.value(),
                block_id.value(),
                self.blocks[block_id.value()].actions.len()
            ));
            for successor in self.successors(block_id) {
                out.push_str(&format!(
                    "  {} -> {};\n",
                    block_id.value(),
                    successor.value()
                ));
            }
        }
        out.push_str("}\n");
        out
    }
}

mod build;
use build::CfgBuilder;

#[cfg(test)]
mod tests;
