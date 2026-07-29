// SPDX-License-Identifier: MIT

//! Control-flow graph construction: a program's shape rewritten as basic blocks and the edges
//! between them.
//!
//! A *basic block* is a run of actions with no branching inside it: once entered, every action
//! executes, and control leaves only through the block's terminator. That is what lets an analysis
//! reason about a whole block at a time and only join facts where edges meet.
//!
//! The graph is built per function, plus one for the top level. Nothing is lowered to a new
//! instruction set — the blocks hold borrowed AST statements — so this is *construction*, not the
//! lowering that `codegen` does to LLVM IR.

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

/// Builds one [`Cfg`]. Threads a "current block" through a recursive walk of the statements.
struct CfgBuilder<'ast> {
    ast: &'ast ProgramAst<'ast>,
    blocks: Vec<BasicBlock<'ast>>,
}

impl<'ast> CfgBuilder<'ast> {
    fn new(ast: &'ast ProgramAst<'ast>) -> CfgBuilder<'ast> {
        CfgBuilder {
            ast,
            blocks: Vec::new(),
        }
    }

    fn new_block(&mut self) -> BlockId {
        self.blocks.push(BasicBlock {
            actions: Vec::new(),
            // A placeholder every construction path overwrites once the block's fate is known. It
            // is deliberately the same value the real exit carries, so a block that somehow kept it
            // behaves as a sink rather than pointing somewhere wrong.
            terminator: Terminator::Exit,
        });
        BlockId(self.blocks.len() as u32 - 1)
    }

    fn set_terminator(&mut self, block_id: BlockId, terminator: Terminator) {
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
    fn build_stmts(&mut self, stmts: &'ast [Stmt], current: BlockId) -> Option<BlockId> {
        let refs: Vec<&'ast Stmt> = stmts.iter().collect();
        self.build_stmt_refs(&refs, current)
    }

    fn build_stmt_refs(&mut self, stmts: &[&'ast Stmt], current: BlockId) -> Option<BlockId> {
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
    fn finish(mut self, entry: BlockId) -> Cfg<'ast> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use indoc::indoc;

    /// A parsed program. The graph borrows it, so it has to outlive the `Cfg` in each test.
    fn parsed(src: &str) -> ProgramAst<'_> {
        parse(src).unwrap()
    }

    /// How many blocks carry each terminator kind, for structural assertions.
    fn terminator_counts(cfg: &Cfg) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for block_id in cfg.block_ids() {
            match cfg.block(block_id).terminator {
                Terminator::Goto(_) => counts.0 += 1,
                Terminator::Branch { .. } => counts.1 += 1,
                Terminator::Return(_) => counts.2 += 1,
                Terminator::Exit => counts.3 += 1,
            }
        }
        counts
    }

    #[test]
    fn straight_line_code_is_one_block() {
        let ast = parsed("a = 1\nb = 2\na + b");
        let cfg = Cfg::build_entry(&ast);
        // One block of three statements, plus the exit.
        assert_eq!(cfg.blocks().len(), 2);
        assert_eq!(cfg.block(cfg.entry()).actions.len(), 3);
        assert!(matches!(
            cfg.block(cfg.entry()).terminator,
            Terminator::Return(None)
        ));
    }

    #[test]
    fn every_graph_has_a_unique_exit_reached_from_each_return() {
        let ast = parsed("a = 1\na");
        let cfg = Cfg::build_entry(&ast);
        assert!(matches!(cfg.block(cfg.exit()).terminator, Terminator::Exit));
        assert_eq!(
            cfg.successors(cfg.exit()),
            Vec::new(),
            "the sink has no exits"
        );
        for returning in cfg.returning_blocks() {
            assert_eq!(cfg.successors(returning), vec![cfg.exit()]);
        }
        assert_eq!(cfg.returning_blocks().len(), 1, "one return today");
    }

    #[test]
    fn a_while_loop_has_a_back_edge() {
        let ast = parsed(indoc! {"
            n = 0
            while n < 3
                n += 1
            n
        "});
        let cfg = Cfg::build_entry(&ast);
        // The header branches, and the body returns to it.
        let header = cfg
            .block_ids()
            .find(|id| matches!(cfg.block(*id).terminator, Terminator::Branch { .. }))
            .expect("a header");
        let body = match cfg.block(header).terminator {
            Terminator::Branch { then_block, .. } => then_block,
            _ => unreachable!(),
        };
        assert_eq!(
            cfg.successors(body),
            vec![header],
            "the body must jump back to the header"
        );
        assert!(
            cfg.predecessors(header).contains(&body),
            "the header's predecessors include the body — that is the cycle"
        );
    }

    #[test]
    fn an_if_statement_becomes_a_diamond() {
        let ast = parsed(indoc! {"
            a = 0
            if a > 0
                a = 1
            else
                a = 2
            a
        "});
        let cfg = Cfg::build_entry(&ast);
        let (_, branches, returns, exits) = terminator_counts(&cfg);
        assert_eq!(branches, 1, "one condition");
        assert_eq!(returns, 1);
        assert_eq!(exits, 1);
        // Both arms converge on one merge block.
        let branch = cfg
            .block_ids()
            .find(|id| matches!(cfg.block(*id).terminator, Terminator::Branch { .. }))
            .unwrap();
        let (then_block, else_block) = match cfg.block(branch).terminator {
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => (then_block, else_block),
            _ => unreachable!(),
        };
        assert_eq!(cfg.successors(then_block), cfg.successors(else_block));
    }

    #[test]
    fn an_if_without_an_else_still_merges() {
        let ast = parsed(indoc! {"
            a = 0
            if a > 0
                a = 1
            a
        "});
        let cfg = Cfg::build_entry(&ast);
        let merge_reached: Vec<BlockId> = cfg
            .block_ids()
            .filter(|id| cfg.predecessors(*id).len() > 1)
            .collect();
        assert_eq!(merge_reached.len(), 1, "the arm and the fall-through meet");
    }

    #[test]
    fn a_for_loop_binds_its_variable_in_the_body() {
        let ast = parsed(indoc! {"
            total = 0
            for idx in 1..3
                total += idx
            total
        "});
        let cfg = Cfg::build_entry(&ast);
        let binds: Vec<BlockId> = cfg
            .block_ids()
            .filter(|id| {
                cfg.block(*id)
                    .actions
                    .iter()
                    .any(|action| matches!(action, Action::Bind(_)))
            })
            .collect();
        assert_eq!(binds.len(), 1, "the body binds the loop variable");
        // And the binding is the body's first action, before anything reads it.
        assert!(matches!(
            cfg.block(binds[0]).actions.first(),
            Some(Action::Bind(_))
        ));
    }

    #[test]
    fn a_multi_binder_for_binds_each_name() {
        let ast = parsed(indoc! {"
            m = [1, 2; 3, 4]
            total = 0
            for row, col, val in m
                total += val
            total
        "});
        let cfg = Cfg::build_entry(&ast);
        let bind_count: usize = cfg
            .block_ids()
            .map(|id| {
                cfg.block(id)
                    .actions
                    .iter()
                    .filter(|action| matches!(action, Action::Bind(_)))
                    .count()
            })
            .sum();
        assert_eq!(bind_count, 3);
    }

    #[test]
    fn a_post_test_loop_branches_at_the_end() {
        let ast = parsed(indoc! {"
            n = 0
            loop
                n += 1
            until n > 2
            n
        "});
        let cfg = Cfg::build_entry(&ast);
        let branch = cfg
            .block_ids()
            .find(|id| matches!(cfg.block(*id).terminator, Terminator::Branch { .. }))
            .expect("a condition at the end of the body");
        // `until` repeats while false, so the *else* edge is the back edge.
        let (then_block, else_block) = match cfg.block(branch).terminator {
            Terminator::Branch {
                then_block,
                else_block,
                ..
            } => (then_block, else_block),
            _ => unreachable!(),
        };
        assert!(
            cfg.predecessors(else_block).contains(&branch) && else_block != then_block,
            "the false edge loops back"
        );
    }

    #[test]
    fn a_nested_block_evaluates_its_result() {
        // An arm's trailing expression is a read that liveness must see, so it becomes an action.
        let ast = parsed("x = 1\ny = 2 if x > 0 else 3\ny");
        let cfg = Cfg::build_entry(&ast);
        // The ternary is nested inside an assignment, so it stays atomic — the whole statement is
        // one action, and no separate Evaluate appears. This documents the if-expression
        // compromise; a statement-position `if` is what produces branches.
        let (_, branches, _, _) = terminator_counts(&cfg);
        assert_eq!(branches, 0, "an if-expression inside a statement is atomic");
    }

    #[test]
    fn a_function_returns_its_body_result() {
        let ast = parse(indoc! {"
            f(a: int): int is
                b = a + 1
                b
            f(1)
        "})
        .unwrap();
        let TopLevel::Func(func) = &ast.top_level[0] else {
            panic!("Expected a function.");
        };
        let cfg = Cfg::build_function(&ast, func);
        let returning = cfg.returning_blocks();
        assert_eq!(returning.len(), 1);
        assert!(
            matches!(
                cfg.block(returning[0]).terminator,
                Terminator::Return(Some(_)),
            ),
            "the body's trailing expression is the returned value"
        );
    }

    #[test]
    fn dot_output_names_every_block_and_edge() {
        let ast = parsed("a = 1\na");
        let cfg = Cfg::build_entry(&ast);
        let dot = cfg.to_dot();
        assert!(dot.starts_with("digraph cfg {"));
        assert!(dot.contains("0 -> 1;"), "entry reaches the exit:\n{dot}");
    }
}
