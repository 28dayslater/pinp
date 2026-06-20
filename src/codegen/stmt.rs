// SPDX-License-Identifier: MIT

use super::*;

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::IntValue;

use crate::parser::{ExprId, Node, RangeKind, Stmt};

impl<'ctx, 'ast> CodeGen<'ctx, 'ast> {
    // -------------------------------------------------------------------------
    // Statements and control flow
    // -------------------------------------------------------------------------

    /// Emits a statement, returning the value it yields (an `Expr`'s value, or an
    /// assignment's right-hand side) — used to pick up the program's result.
    pub(super) fn gen_stmt(&mut self, stmt: &Stmt) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        match stmt {
            Stmt::Expr(expr_id) => self.gen_expr(*expr_id),
            Stmt::Assign {
                target_lists,
                values,
            } => {
                // Evaluate every value first (so `a, b = b, a` swaps), then store to each target
                // group. The SSA values are the temporaries; no extra slots needed.
                let mut evaluated = Vec::with_capacity(values.len());
                for value in values {
                    evaluated.push((self.expect_value(*value)?, self.ast.type_of(*value)));
                }
                for group in target_lists {
                    for (place, (value, value_type)) in group.iter().zip(&evaluated) {
                        let (pointer, slot_type) = self.resolve_place(*place, *value_type)?;
                        let stored = self.promote(*value, *value_type, slot_type);
                        self.builder.build_store(pointer, stored).map_err(err)?;
                    }
                }
                // The program result of a trailing assignment is the last value bound.
                Ok(evaluated.last().map(|(value, _)| *value))
            }
            Stmt::While { cond, body } => {
                let function = self.current_function();
                let header = self.context.append_basic_block(function, "while_header");
                let body_bb = self.context.append_basic_block(function, "while_body");
                let exit = self.context.append_basic_block(function, "while_exit");
                self.builder
                    .build_unconditional_branch(header)
                    .map_err(err)?;

                self.builder.position_at_end(header);
                let cond_val = self.expect_value(*cond)?.into_int_value();
                self.builder
                    .build_conditional_branch(cond_val, body_bb, exit)
                    .map_err(err)?;

                self.builder.position_at_end(body_bb);
                self.gen_block(body)?; // run for effect
                self.builder
                    .build_unconditional_branch(header)
                    .map_err(err)?; // back-edge

                self.builder.position_at_end(exit);
                Ok(None)
            }
            Stmt::Loop { body, cond, until } => {
                let function = self.current_function();
                let body_bb = self.context.append_basic_block(function, "loop_body");
                let exit = self.context.append_basic_block(function, "loop_exit");
                self.builder
                    .build_unconditional_branch(body_bb)
                    .map_err(err)?;

                self.builder.position_at_end(body_bb);
                self.gen_block(body)?;
                // Post-test: repeat while `cond` holds; `until` flips the sense (repeat while false).
                let cond_val = self.expect_value(*cond)?.into_int_value();
                let (true_bb, false_bb) = if *until {
                    (exit, body_bb)
                } else {
                    (body_bb, exit)
                };
                self.builder
                    .build_conditional_branch(cond_val, true_bb, false_bb)
                    .map_err(err)?;

                self.builder.position_at_end(exit);
                Ok(None)
            }
            Stmt::For { var, range, body } => {
                let range = self.expect_value(*range)?;
                let (start, stop, step, inclusive) = self.extract_range(range)?;
                let function = self.current_function();

                // The counter lives in an entry-block slot so its alloca is not re-run each pass.
                let counter = self.alloca_at_entry(PinpType::Int)?;
                self.builder.build_store(counter, start).map_err(err)?;
                let zero = self.int_type().const_zero();
                let going_up = self
                    .builder
                    .build_int_compare(IntPredicate::SGT, step, zero, "going_up")
                    .map_err(err)?;

                let header = self.context.append_basic_block(function, "for_header");
                let body_bb = self.context.append_basic_block(function, "for_body");
                let exit = self.context.append_basic_block(function, "for_exit");
                self.builder
                    .build_unconditional_branch(header)
                    .map_err(err)?;

                // Header: continue while the counter has not passed `stop`, in the step's direction.
                // The step is non-zero (a zero step traps when the range is built), so the counter
                // always advances and the loop terminates.
                self.builder.position_at_end(header);
                let current = self.load_counter(counter)?;
                let cond = self.range_continue(current, stop, inclusive, going_up)?;
                self.builder
                    .build_conditional_branch(cond, body_bb, exit)
                    .map_err(err)?;

                // Body: the loop variable reads the counter slot; the latch advances it by the step.
                self.builder.position_at_end(body_bb);
                self.push_scope();
                self.locals
                    .last_mut()
                    .unwrap()
                    .insert(*var, (counter, PinpType::Int));
                self.gen_block(body)?;
                self.pop_scope();
                let current = self.load_counter(counter)?;
                let next = self
                    .builder
                    .build_int_add(current, step, "next")
                    .map_err(err)?;
                self.builder.build_store(counter, next).map_err(err)?;
                self.builder
                    .build_unconditional_branch(header)
                    .map_err(err)?;

                self.builder.position_at_end(exit);
                Ok(None)
            }
        }
    }

    /// Emits a block's statements then its optional result, each in a fresh local scope so any
    /// name introduced here does not escape. Returns the result value (if the block ends in one).
    fn gen_block(
        &mut self,
        block: &crate::parser::Block,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        self.push_scope();
        for stmt in &block.stmts {
            self.gen_stmt(stmt)?;
        }
        let result = match block.result {
            Some(expr_id) => self.gen_expr(expr_id)?,
            None => None,
        };
        self.pop_scope();
        Ok(result)
    }

    /// Resolves an assignable place to its pointer, creating a function-local
    /// alloca on first assignment.
    fn resolve_place(
        &mut self,
        place: Place,
        rhs_type: PinpType,
    ) -> Result<(PointerValue<'ctx>, PinpType), String> {
        match place {
            Place::Global(sym) => Ok(self.globals[&sym]),
            Place::Local(sym) => {
                // Mutate the nearest enclosing local if there is one.
                if let Some(slot) = self.find_local(sym) {
                    return Ok(slot);
                }
                // At the top level a bare name not held in any local frame is a module global.
                if !self.in_function
                    && let Some(slot) = self.globals.get(&sym)
                {
                    return Ok(*slot);
                }
                // Otherwise introduce a fresh body-local, its slot alloca'd in the entry block.
                let slot = self.alloca_at_entry(rhs_type)?;
                self.locals
                    .last_mut()
                    .unwrap()
                    .insert(sym, (slot, rhs_type));
                Ok((slot, rhs_type))
            }
        }
    }

    // Lower an `if`/`elif`/`else` (and the one-line ternary, which is a single-arm `If`) as a chain
    // of diamonds joined at a merge block. When the node has a value (an `else` is present and every
    // branch yields one), a `phi` in the merge selects the taken branch's value; otherwise the `if`
    // runs purely for effect and yields nothing.
    pub(super) fn gen_if(
        &mut self,
        expr_id: ExprId,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let result_type = self.ast.type_of(expr_id);
        let Node::If { arms, else_block } = self.ast.node(expr_id) else {
            unreachable!("gen_if on a non-If node")
        };
        let function = self.current_function();
        let merge_bb = self.context.append_basic_block(function, "if_merge");

        // Value + originating block for each branch, for the merge `phi` (only when typed).
        let mut incomings: Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)> = Vec::new();

        for arm in arms {
            let cond = self.expect_value(arm.cond)?.into_int_value(); // i1
            let then_bb = self.context.append_basic_block(function, "if_then");
            let next_bb = self.context.append_basic_block(function, "if_next");
            self.builder
                .build_conditional_branch(cond, then_bb, next_bb)
                .map_err(err)?;

            self.builder.position_at_end(then_bb);
            let value = self.gen_block(&arm.body)?;
            self.record_branch(&arm.body, value, result_type, &mut incomings)?;
            self.builder
                .build_unconditional_branch(merge_bb)
                .map_err(err)?;

            // Subsequent arms test in the fall-through block.
            self.builder.position_at_end(next_bb);
        }

        // The trailing `else` (if any) occupies the final fall-through block; a missing `else`
        // leaves it empty and the node is `Void`.
        if let Some(else_block) = else_block {
            let value = self.gen_block(else_block)?;
            self.record_branch(else_block, value, result_type, &mut incomings)?;
        }
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(err)?;

        self.builder.position_at_end(merge_bb);
        if result_type == PinpType::Void {
            return Ok(None);
        }
        let phi = self
            .builder
            .build_phi(self.basic_type(result_type), "if")
            .map_err(err)?;
        for (value, block) in &incomings {
            phi.add_incoming(&[(value, *block)]);
        }
        Ok(Some(phi.as_basic_value()))
    }

    // Record a branch's value (promoted to the `if`'s result type) and the block it flows from, for
    // the merge `phi`. A no-op when the `if` is `Void`.
    fn record_branch(
        &self,
        body: &crate::parser::Block,
        value: Option<BasicValueEnum<'ctx>>,
        result_type: PinpType,
        incomings: &mut Vec<(BasicValueEnum<'ctx>, BasicBlock<'ctx>)>,
    ) -> Result<(), String> {
        if result_type == PinpType::Void {
            return Ok(());
        }
        let result = body
            .result
            .expect("a typed if branch ends in an expression");
        let value = value.expect("a typed if branch yields a value");
        let value = self.promote(value, self.ast.type_of(result), result_type);
        let end = self.builder.get_insert_block().expect("an active block");
        incomings.push((value, end));
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Ranges and the runtime-error trap
    // -------------------------------------------------------------------------

    // Build a range value `{start, stop, step, inclusive}`, defaulting an omitted step to ±1. A
    // zero step never advances, so building such a range traps — every range thereafter has a
    // non-zero step, which the `for`/`in` lowerings rely on.
    pub(super) fn gen_range(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let Node::Range {
            start,
            stop,
            step,
            kind,
        } = self.ast.node(expr_id)
        else {
            unreachable!("gen_range on a non-Range node")
        };
        let (start, stop, step, kind) = (*start, *stop, *step, *kind);
        let start_value = self.as_int(start)?;
        let stop_value = self.as_int(stop)?;
        let step_value = match step {
            Some(step_id) => self.as_int(step_id)?,
            None => self.default_step(kind, start_value, stop_value)?,
        };
        self.guard_zero_step(step_value)?;
        let inclusive = self
            .bool_type()
            .const_int((kind == RangeKind::Inclusive) as u64, false);
        self.build_range(start_value, stop_value, step_value, inclusive)
    }

    // Raise a runtime error if `step` is zero. A literal zero is already a sema error, so this only
    // fires for a variable step that is zero at run time — a range that could never advance.
    fn guard_zero_step(&self, step: IntValue<'ctx>) -> Result<(), String> {
        let zero = self.int_type().const_zero();
        let is_zero = self
            .builder
            .build_int_compare(IntPredicate::EQ, step, zero, "zero_step")
            .map_err(err)?;
        let function = self.current_function();
        let error_bb = self.context.append_basic_block(function, "range_zero_step");
        let ok_bb = self.context.append_basic_block(function, "range_ok");
        self.builder
            .build_conditional_branch(is_zero, error_bb, ok_bb)
            .map_err(err)?;

        self.builder.position_at_end(error_bb);
        self.raise_runtime_error(RUNTIME_ERROR_ZERO_STEP)?;

        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    // The module global holding the runtime-error code (`0` = none), read back by [`PinpJit::run`].
    // It has external linkage so the JIT exposes it for lookup; declared on demand and reused.
    pub(super) fn runtime_error_global(&self) -> PointerValue<'ctx> {
        if let Some(global) = self.module.get_global(RUNTIME_ERROR_SYMBOL) {
            return global.as_pointer_value();
        }
        let global = self
            .module
            .add_global(self.int_type(), None, RUNTIME_ERROR_SYMBOL);
        global.set_initializer(&self.int_type().const_zero());
        global.as_pointer_value()
    }

    // Records `code` in the runtime-error global and returns from the current function with a
    // throwaway value: pinp has no unwinding, so execution stops by falling back out to the host,
    // which sees the recorded code and turns it into an `Err` instead of using the result.
    fn raise_runtime_error(&self, code: i64) -> Result<(), String> {
        let global = self.runtime_error_global();
        self.builder
            .build_store(global, self.int_type().const_int(code as u64, true))
            .map_err(err)?;
        let function = self.current_function();
        match function.get_type().get_return_type() {
            None => self.builder.build_return(None).map_err(err)?,
            Some(return_type) => {
                let throwaway = self.llvm_zero(return_type);
                self.builder.build_return(Some(&throwaway)).map_err(err)?
            }
        };
        Ok(())
    }

    // The zero of an LLVM scalar return type — the throwaway value returned when a trap fires.
    fn llvm_zero(&self, basic_type: BasicTypeEnum<'ctx>) -> BasicValueEnum<'ctx> {
        match basic_type {
            BasicTypeEnum::IntType(int_type) => int_type.const_zero().into(),
            BasicTypeEnum::FloatType(float_type) => float_type.const_zero().into(),
            other => unreachable!("Unexpected function return type {other:?}."),
        }
    }

    // The step for an `start..stop` with no `:step`: +1 / -1 for the exclusive forms, and for the
    // inclusive form the direction is taken from the bounds (up when `stop >= start`).
    fn default_step(
        &self,
        kind: RangeKind,
        start: IntValue<'ctx>,
        stop: IntValue<'ctx>,
    ) -> Result<IntValue<'ctx>, String> {
        let one = self.int_type().const_int(1, true);
        let neg_one = self.int_type().const_int((-1i64) as u64, true);
        Ok(match kind {
            RangeKind::UpExclusive => one,
            RangeKind::DownExclusive => neg_one,
            RangeKind::Inclusive => {
                let up = self
                    .builder
                    .build_int_compare(IntPredicate::SGE, stop, start, "rng_up")
                    .map_err(err)?;
                self.builder
                    .build_select(up, one, neg_one, "rng_step")
                    .map_err(err)?
                    .into_int_value()
            }
        })
    }

    // Load the loop counter from its stack slot.
    fn load_counter(&self, counter: PointerValue<'ctx>) -> Result<IntValue<'ctx>, String> {
        Ok(self
            .builder
            .build_load(self.int_type(), counter, "idx")
            .map_err(err)?
            .into_int_value())
    }

    // The `for` continue test: ascending wants `counter <= stop` (inclusive) or `< stop`; descending
    // the mirror. `going_up` picks the side, so the two directions share one header.
    pub(super) fn range_continue(
        &self,
        current: IntValue<'ctx>,
        stop: IntValue<'ctx>,
        inclusive: IntValue<'ctx>,
        going_up: IntValue<'ctx>,
    ) -> Result<IntValue<'ctx>, String> {
        let le = self
            .builder
            .build_int_compare(IntPredicate::SLE, current, stop, "le")
            .map_err(err)?;
        let lt = self
            .builder
            .build_int_compare(IntPredicate::SLT, current, stop, "lt")
            .map_err(err)?;
        let ge = self
            .builder
            .build_int_compare(IntPredicate::SGE, current, stop, "ge")
            .map_err(err)?;
        let gt = self
            .builder
            .build_int_compare(IntPredicate::SGT, current, stop, "gt")
            .map_err(err)?;
        let up = self
            .builder
            .build_select(inclusive, le, lt, "up")
            .map_err(err)?
            .into_int_value();
        let down = self
            .builder
            .build_select(inclusive, ge, gt, "down")
            .map_err(err)?
            .into_int_value();
        Ok(self
            .builder
            .build_select(going_up, up, down, "for_cond")
            .map_err(err)?
            .into_int_value())
    }
}
