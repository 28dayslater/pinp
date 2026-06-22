// SPDX-License-Identifier: MIT

use super::*;

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::values::IntValue;

use crate::parser::{ArrayElementType, ExprId, Node, RangeKind, Stmt};

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
            Stmt::IndexedAssign {
                target,
                index,
                value,
            } => {
                // Look up the array variable's alloca slot and confirm it is an array.
                let (slot_ptr, array_pinp_type) = match *target {
                    Place::Local(sym_id) => {
                        if let Some(slot) = self.find_local(sym_id) {
                            slot
                        } else {
                            self.globals[&sym_id]
                        }
                    }
                    Place::Global(sym_id) => self.globals[&sym_id],
                };
                let PinpType::Array(elem_type, n) = array_pinp_type else {
                    unreachable!("IndexedAssign target is always an array")
                };
                // The slot holds the heap pointer; load it.
                let array_ptr = self
                    .builder
                    .build_load(
                        self.context.ptr_type(AddressSpace::default()),
                        slot_ptr,
                        "arr_ptr",
                    )
                    .map_err(err)?
                    .into_pointer_value();
                // Dispatch to slice fill when the index is a range.
                if matches!(self.ast.node(*index), Node::Range { .. }) {
                    return self.gen_slice_assign(elem_type, array_ptr, *index, *value);
                }
                // Bounds-check and locate the element.
                let index_val = self.as_int(*index)?;
                let elem_ptr = self.bounds_check_and_gep(elem_type, array_ptr, index_val, n)?;
                // Evaluate, promote to element type, and store.
                let val = self.expect_value(*value)?;
                let val = self.promote(val, self.ast.type_of(*value), PinpType::from(elem_type));
                self.builder.build_store(elem_ptr, val).map_err(err)?;
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
        self.gen_runtime_error_call("Range step cannot be zero.")?;

        self.builder.position_at_end(ok_bb);
        Ok(())
    }

    // Calls the native `pinp_runtime_error` with `message` and emits `unreachable`. This longjmps
    // back to `pinp_run` in the host, so it truly never returns — hence the unreachable terminator.
    pub(super) fn gen_runtime_error_call(&self, message: &str) -> Result<(), String> {
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let fn_type = self.context.void_type().fn_type(&[ptr_type.into()], false);
        let error_fn = self
            .module
            .get_function("pinp_runtime_error")
            .unwrap_or_else(|| {
                self.module
                    .add_function("pinp_runtime_error", fn_type, None)
            });
        let msg_ptr = self
            .builder
            .build_global_string_ptr(message, "err_msg")
            .map_err(err)?
            .as_pointer_value();
        self.builder
            .build_call(error_fn, &[msg_ptr.into()], "")
            .map_err(err)?;
        self.builder.build_unreachable().map_err(err)?;
        Ok(())
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
    pub(super) fn load_counter(
        &self,
        counter: PointerValue<'ctx>,
    ) -> Result<IntValue<'ctx>, String> {
        Ok(self
            .builder
            .build_load(self.int_type(), counter, "idx")
            .map_err(err)?
            .into_int_value())
    }

    // `array[start..stop] = scalar` — store `value` into every element of the slice. The bounds are
    // literal integers (sema-enforced), so the loop count and start offset are compile-time constants.
    fn gen_slice_assign(
        &mut self,
        elem_type: ArrayElementType,
        array_ptr: PointerValue<'ctx>,
        range_id: ExprId,
        value_id: ExprId,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let (start_lit, stop_lit, kind) = self.read_slice_range(range_id);
        let count = match kind {
            RangeKind::Inclusive => (stop_lit - start_lit + 1) as usize,
            RangeKind::UpExclusive => (stop_lit - start_lit) as usize,
            _ => unreachable!(),
        };
        let start_offset = self.int_type().const_int(start_lit as u64, false);
        let n_val = self.int_type().const_int(count as u64, false);

        // Evaluate the scalar once; promote to the element type.
        let raw_val = self.expect_value(value_id)?;
        let elem_pinp_type = PinpType::from(elem_type);
        let stored_val = self.promote(raw_val, self.ast.type_of(value_id), elem_pinp_type);

        let counter = self.alloca_at_entry(PinpType::Int)?;
        self.builder
            .build_store(counter, self.int_type().const_zero())
            .map_err(err)?;

        let function = self.current_function();
        let header = self.context.append_basic_block(function, "sla_hdr");
        let body = self.context.append_basic_block(function, "sla_body");
        let exit = self.context.append_basic_block(function, "sla_exit");

        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(header);
        let idx = self.load_counter(counter)?;
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, idx, n_val, "sla_cond")
            .map_err(err)?;
        self.builder
            .build_conditional_branch(cond, body, exit)
            .map_err(err)?;

        self.builder.position_at_end(body);
        let dst_idx = self
            .builder
            .build_int_add(idx, start_offset, "dst_idx")
            .map_err(err)?;
        let dst_ptr = self.build_element_ptr(elem_type, array_ptr, dst_idx)?;
        self.builder.build_store(dst_ptr, stored_val).map_err(err)?;
        let one = self.int_type().const_int(1, false);
        let next = self.builder.build_int_add(idx, one, "next").map_err(err)?;
        self.builder.build_store(counter, next).map_err(err)?;
        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(exit);
        Ok(None)
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
