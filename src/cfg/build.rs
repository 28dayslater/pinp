// SPDX-License-Identifier: MIT

use super::*;

/// Builds one [`Cfg`]. Threads a "current block" through a recursive walk of the statements.
pub(super) struct CfgBuilder<'ast> {
    ast: &'ast ProgramAst<'ast>,
    blocks: Vec<BasicBlock<'ast>>,
}

impl<'ast> CfgBuilder<'ast> {
    pub(super) fn new(ast: &'ast ProgramAst<'ast>) -> CfgBuilder<'ast> {
        CfgBuilder {
            ast,
            blocks: Vec::new(),
        }
    }

    pub(super) fn new_block(&mut self) -> BlockId {
        self.blocks.push(BasicBlock {
            actions: Vec::new(),
            // A placeholder every construction path overwrites once the block's fate is known. It
            // is deliberately the same value the real exit carries, so a block that somehow kept it
            // behaves as a sink rather than pointing somewhere wrong.
            terminator: Terminator::Exit,
        });
        BlockId(self.blocks.len() as u32 - 1)
    }

    pub(super) fn set_terminator(&mut self, block_id: BlockId, terminator: Terminator) {
        self.blocks[block_id.value()].terminator = terminator;
    }

    fn push_action(&mut self, block_id: BlockId, action: Action<'ast>) {
        self.blocks[block_id.value()].actions.push(action);
    }

    /// Lowers a statement list, returning the block control falls through to — or `None` when it
    /// cannot, because every path has left the function.
    ///
    /// Nothing yields `None` today: pinp has no `return`, `break`, or `continue`. The shape is here
    /// so that adding them is a local change rather than a rewrite of every arm below.
    pub(super) fn build_stmts(&mut self, stmts: &'ast [Stmt], current: BlockId) -> Option<BlockId> {
        let refs: Vec<&'ast Stmt> = stmts.iter().collect();
        self.build_stmt_refs(&refs, current)
    }

    pub(super) fn build_stmt_refs(
        &mut self,
        stmts: &[&'ast Stmt],
        current: BlockId,
    ) -> Option<BlockId> {
        let mut current = Some(current);
        for stmt in stmts {
            current = self.build_stmt(stmt, current?);
        }
        current
    }

    fn build_stmt(&mut self, stmt: &'ast Stmt, current: BlockId) -> Option<BlockId> {
        match stmt {
            // An `if` in statement position is real control flow, so it becomes real branches.
            Stmt::Expr(expr_id) if matches!(self.ast.node(*expr_id), Node::If { .. }) => {
                self.build_if(*expr_id, current)
            }
            Stmt::Assign { .. }
            | Stmt::Expr(_)
            | Stmt::IndexedAssign { .. }
            | Stmt::IndexedAssign2D { .. } => {
                self.push_action(current, Action::Statement(stmt));
                Some(current)
            }
            Stmt::While { cond, body } => self.build_while(*cond, body, current),
            Stmt::Loop { body, cond, until } => self.build_loop(body, *cond, *until, current),
            Stmt::For { var, source, body } => {
                self.build_counted_loop(&[*var], *source, body, current)
            }
            Stmt::ForArray {
                binders,
                source,
                body,
            } => self.build_counted_loop(binders, *source, body, current),
        }
    }

    /// `if`/`elif`/`else` as a chain of diamonds joined at one merge block.
    fn build_if(&mut self, expr_id: ExprId, current: BlockId) -> Option<BlockId> {
        let Node::If { arms, else_block } = self.ast.node(expr_id) else {
            unreachable!("build_if on a non-If node")
        };
        let merge = self.new_block();
        let mut testing = current;
        // Whether any path actually reaches the merge; if none does, control does not continue.
        let mut merge_reached = false;

        for arm in arms {
            let then_block = self.new_block();
            let next = self.new_block();
            self.set_terminator(
                testing,
                Terminator::Branch {
                    cond: arm.cond,
                    then_block,
                    else_block: next,
                },
            );
            if let Some(arm_end) = self.build_block(&arm.body, then_block) {
                self.set_terminator(arm_end, Terminator::Goto(merge));
                merge_reached = true;
            }
            testing = next;
        }

        // The trailing `else` occupies the final fall-through block; without one, that block simply
        // continues to the merge.
        match else_block {
            Some(else_body) => {
                if let Some(else_end) = self.build_block(else_body, testing) {
                    self.set_terminator(else_end, Terminator::Goto(merge));
                    merge_reached = true;
                }
            }
            None => {
                self.set_terminator(testing, Terminator::Goto(merge));
                merge_reached = true;
            }
        }

        merge_reached.then_some(merge)
    }

    /// A nested block: its statements, then its trailing result evaluated where the block ends.
    fn build_block(&mut self, block: &'ast Block, current: BlockId) -> Option<BlockId> {
        let end = self.build_stmts(&block.stmts, current)?;
        if let Some(result) = block.result {
            self.push_action(end, Action::Evaluate(result));
        }
        Some(end)
    }

    /// Pre-test loop: a header that tests, a body that jumps back to it, and an exit.
    fn build_while(
        &mut self,
        cond: ExprId,
        body: &'ast Block,
        current: BlockId,
    ) -> Option<BlockId> {
        let header = self.new_block();
        let body_block = self.new_block();
        let exit = self.new_block();
        self.set_terminator(current, Terminator::Goto(header));
        self.set_terminator(
            header,
            Terminator::Branch {
                cond,
                then_block: body_block,
                else_block: exit,
            },
        );
        if let Some(body_end) = self.build_block(body, body_block) {
            // The back edge: what makes the solver iterate rather than run once.
            self.set_terminator(body_end, Terminator::Goto(header));
        }
        Some(exit)
    }

    /// Post-test loop: the body runs first, then the condition decides whether to repeat. `until`
    /// swaps the successors — repeat while the condition is *false*.
    fn build_loop(
        &mut self,
        body: &'ast Block,
        cond: ExprId,
        until: bool,
        current: BlockId,
    ) -> Option<BlockId> {
        let body_block = self.new_block();
        let exit = self.new_block();
        self.set_terminator(current, Terminator::Goto(body_block));
        // A `None` body end would mean the body always leaves the function, so the condition is
        // never reached and the loop needs no branch at all.
        if let Some(body_end) = self.build_block(body, body_block) {
            let (then_block, else_block) = match until {
                true => (exit, body_block),
                false => (body_block, exit),
            };
            self.set_terminator(
                body_end,
                Terminator::Branch {
                    cond,
                    then_block,
                    else_block,
                },
            );
        }
        Some(exit)
    }

    /// `for` over a range, array, or matrix. The source is evaluated once before the loop; each
    /// iteration binds the loop variables, so an analysis sees a definition per pass.
    fn build_counted_loop(
        &mut self,
        binders: &[SymId],
        source: ExprId,
        body: &'ast Block,
        current: BlockId,
    ) -> Option<BlockId> {
        self.push_action(current, Action::Evaluate(source));
        let header = self.new_block();
        let body_block = self.new_block();
        let exit = self.new_block();
        self.set_terminator(current, Terminator::Goto(header));
        // The header decides whether another iteration runs. It has no condition expression of its
        // own — the counter is implicit — so it branches on the source, which is already evaluated.
        self.set_terminator(
            header,
            Terminator::Branch {
                cond: source,
                then_block: body_block,
                else_block: exit,
            },
        );
        for binder in binders {
            self.push_action(body_block, Action::Bind(*binder));
        }
        if let Some(body_end) = self.build_block(body, body_block) {
            self.set_terminator(body_end, Terminator::Goto(header));
        }
        Some(exit)
    }

    /// Adds the unique exit block, wires predecessors, and hands over the finished graph.
    pub(super) fn finish(mut self, entry: BlockId) -> Cfg<'ast> {
        let exit = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock {
            actions: Vec::new(),
            terminator: Terminator::Exit,
        });

        let mut cfg = Cfg {
            blocks: self.blocks,
            entry,
            exit,
            predecessors: Vec::new(),
        };
        let mut predecessors = vec![Vec::new(); cfg.blocks.len()];
        for block_id in cfg.block_ids() {
            for successor in cfg.successors(block_id) {
                predecessors[successor.value()].push(block_id);
            }
        }
        cfg.predecessors = predecessors;
        cfg
    }
}
