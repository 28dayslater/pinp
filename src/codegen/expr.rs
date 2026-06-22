// SPDX-License-Identifier: MIT

use super::*;

use inkwell::IntPredicate;
use inkwell::values::BasicMetadataValueEnum;

use crate::parser::{ArrayElementType, BinOp, ExprId, Node, RangeKind, UnOp};

impl<'ctx, 'ast> CodeGen<'ctx, 'ast> {
    // -------------------------------------------------------------------------
    // Expression lowering
    // -------------------------------------------------------------------------

    /// Lowers an expression to its LLVM value, or `None` for a void expression.
    pub(super) fn gen_expr(
        &mut self,
        expr_id: ExprId,
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let node = self.ast.node(expr_id);
        let value = match node {
            Node::Int(int_value) => self.int_type().const_int(*int_value as u64, true).into(),
            Node::Float(float_value) => self.float_type().const_float(*float_value).into(),
            Node::Bool(bool_value) => self.bool_type().const_int(*bool_value as u64, false).into(),
            Node::Var(sym_id) => self.load_var(*sym_id, false)?,
            Node::Global(sym_id) => self.load_var(*sym_id, true)?,
            Node::Unary {
                op: UnOp::Neg,
                operand,
            } => {
                let operand = *operand;
                // The result type is `Int` for a bool/int operand, `Float` for a float one; promote
                // the operand up to it (a `Bool` operand widens to `Int`) before negating.
                let result_type = self.ast.type_of(expr_id);
                let value = self.expect_value(operand)?;
                let value = self.promote(value, self.ast.type_of(operand), result_type);
                if result_type == PinpType::Int {
                    self.builder
                        .build_int_neg(value.into_int_value(), "neg")
                        .map_err(err)?
                        .into()
                } else {
                    self.builder
                        .build_float_neg(value.into_float_value(), "neg")
                        .map_err(err)?
                        .into()
                }
            }
            Node::Unary {
                op: UnOp::Not,
                operand,
            } => {
                let value = self.expect_value(*operand)?.into_int_value(); // i1
                self.builder.build_not(value, "not").map_err(err)?.into()
            }
            Node::Bin { op, lhs, rhs } => self.gen_bin(expr_id, *op, *lhs, *rhs)?,
            Node::Call { callee, args } => return self.gen_call(*callee, args),
            Node::If { .. } => return self.gen_if(expr_id),
            Node::Range { .. } => self.gen_range(expr_id)?,
            Node::Membership { .. } => self.gen_membership(expr_id)?,
            Node::ArrayLiteral { .. } => self.gen_array_literal(expr_id)?,
            Node::Index { .. } => self.gen_index(expr_id)?,
            Node::Member { .. } => self.gen_member(expr_id)?,
            Node::Comprehension { .. } => self.gen_comprehension(expr_id)?,
            Node::MatrixLiteral { .. } => todo!("MatrixLiteral codegen — step 13"),
            Node::Index2D { .. } => todo!("Index2D codegen — step 15"),
            Node::FullExtent => {
                unreachable!("FullExtent is resolved by sema, never reaches codegen")
            }
        };
        Ok(Some(value))
    }

    // `value in range` as closed-form arithmetic: on-step, in-bounds (direction-aware), and a
    // non-zero step (an empty range is never a member). No branches, no runtime calls.
    fn gen_membership(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let Node::Membership { value, range } = self.ast.node(expr_id) else {
            unreachable!("gen_membership on a non-Membership node")
        };
        let (value, range) = (*value, *range);
        let value = self.as_int(value)?;
        let range = self.expect_value(range)?;
        let (start, stop, step, inclusive) = self.extract_range(range)?;

        let zero = self.int_type().const_zero();
        // `(value - start)` must be a multiple of step. The step is non-zero (a zero step traps when
        // the range is built), so the `srem` is well-defined.
        let diff = self
            .builder
            .build_int_sub(value, start, "diff")
            .map_err(err)?;
        let remainder = self
            .builder
            .build_int_signed_rem(diff, step, "rem")
            .map_err(err)?;
        let on_step = self
            .builder
            .build_int_compare(IntPredicate::EQ, remainder, zero, "on_step")
            .map_err(err)?;

        let going_up = self
            .builder
            .build_int_compare(IntPredicate::SGT, step, zero, "going_up")
            .map_err(err)?;
        let within_stop = self.range_continue(value, stop, inclusive, going_up)?;
        // `range_continue` covers the stop side; membership also needs the start side.
        let ge_start = self
            .builder
            .build_int_compare(IntPredicate::SGE, value, start, "ge_start")
            .map_err(err)?;
        let le_start = self
            .builder
            .build_int_compare(IntPredicate::SLE, value, start, "le_start")
            .map_err(err)?;
        let from_start = self
            .builder
            .build_select(going_up, ge_start, le_start, "from_start")
            .map_err(err)?
            .into_int_value();

        let member = self
            .builder
            .build_and(on_step, within_stop, "member")
            .map_err(err)?;
        let member = self
            .builder
            .build_and(member, from_start, "member")
            .map_err(err)?;
        Ok(member.into())
    }

    // Loads the current value of a variable.
    fn load_var(&self, sym_id: SymId, global: bool) -> Result<BasicValueEnum<'ctx>, String> {
        // `::name` always reads a global; a bare name reads the nearest enclosing local, falling
        // back to a global only at the top level (where top-level vars are module globals).
        let (pointer, value_type) = if global {
            self.globals[&sym_id]
        } else if let Some(slot) = self.find_local(sym_id) {
            slot
        } else {
            self.globals[&sym_id]
        };
        self.builder
            .build_load(self.basic_type(value_type), pointer, "load")
            .map_err(err)
    }

    // Lowers a call, promoting each argument to its parameter type.
    fn gen_call(
        &mut self,
        callee: SymId,
        args: &'ast [ExprId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let (function, param_types, return_type) = self.functions[&callee].clone();
        let mut arg_values: Vec<BasicMetadataValueEnum> = Vec::with_capacity(args.len());
        for (arg, param_type) in args.iter().zip(&param_types) {
            let value = self.expect_value(*arg)?;
            let value = self.promote(value, self.ast.type_of(*arg), *param_type);
            arg_values.push(value.into());
        }
        let call = self
            .builder
            .build_call(function, &arg_values, "call")
            .map_err(err)?;
        Ok(match return_type {
            PinpType::Void => None,
            _ => Some(basic_value(call.try_as_basic_value())),
        })
    }

    // Lowers a binary operation: arithmetic (int or float), comparison, or logical.
    fn gen_bin(
        &mut self,
        expr_id: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let result_type = self.ast.type_of(expr_id);
        let value = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul if result_type == PinpType::Float => {
                let left = self.as_float(lhs)?;
                let right = self.as_float(rhs)?;
                match op {
                    BinOp::Add => self.builder.build_float_add(left, right, "fadd"),
                    BinOp::Sub => self.builder.build_float_sub(left, right, "fsub"),
                    _ => self.builder.build_float_mul(left, right, "fmul"),
                }
                .map_err(err)?
                .into()
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let left = self.as_int(lhs)?;
                let right = self.as_int(rhs)?;
                match op {
                    BinOp::Add => self.builder.build_int_add(left, right, "add"),
                    BinOp::Sub => self.builder.build_int_sub(left, right, "sub"),
                    _ => self.builder.build_int_mul(left, right, "mul"),
                }
                .map_err(err)?
                .into()
            }
            BinOp::Div => {
                let left = self.as_float(lhs)?;
                let right = self.as_float(rhs)?;
                self.builder
                    .build_float_div(left, right, "fdiv")
                    .map_err(err)?
                    .into()
            }
            BinOp::IntDiv => {
                let left = self.as_int(lhs)?;
                let right = self.as_int(rhs)?;
                self.builder
                    .build_int_signed_div(left, right, "sdiv")
                    .map_err(err)?
                    .into()
            }
            BinOp::Mod => {
                let left = self.as_int(lhs)?;
                let right = self.as_int(rhs)?;
                self.builder
                    .build_int_signed_rem(left, right, "srem")
                    .map_err(err)?
                    .into()
            }
            BinOp::Pow => {
                let left = self.as_float(lhs)?;
                let right = self.as_float(rhs)?;
                let pow = self.pow_intrinsic();
                let call = self
                    .builder
                    .build_call(pow, &[left.into(), right.into()], "pow")
                    .map_err(err)?;
                let result = basic_value(call.try_as_basic_value()).into_float_value();
                if result_type == PinpType::Int {
                    self.builder
                        .build_float_to_signed_int(result, self.int_type(), "powi")
                        .map_err(err)?
                        .into()
                } else {
                    result.into()
                }
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                self.gen_compare(op, lhs, rhs)?
            }
            // `xor` is eager — both operands always decide the result.
            BinOp::Xor => {
                let left = self.expect_value(lhs)?.into_int_value(); // i1
                let right = self.expect_value(rhs)?.into_int_value();
                self.builder
                    .build_xor(left, right, "xor")
                    .map_err(err)?
                    .into()
            }
            BinOp::And | BinOp::Or => self.gen_short_circuit(op, lhs, rhs)?,
        };
        Ok(value)
    }

    // A comparison yields `i1`. Operands compare at their common type: float (with an ordered
    // predicate) if either side is float, otherwise int (signed predicate) — `Bool` operands
    // widen to int via `as_int`/`as_float`.
    fn gen_compare(
        &mut self,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        use inkwell::{FloatPredicate as FloatPred, IntPredicate as IntPred};
        let float =
            self.ast.type_of(lhs) == PinpType::Float || self.ast.type_of(rhs) == PinpType::Float;
        let value = if float {
            let left = self.as_float(lhs)?;
            let right = self.as_float(rhs)?;
            let pred = match op {
                BinOp::Eq => FloatPred::OEQ,
                BinOp::Ne => FloatPred::ONE,
                BinOp::Lt => FloatPred::OLT,
                BinOp::Gt => FloatPred::OGT,
                BinOp::Le => FloatPred::OLE,
                _ => FloatPred::OGE,
            };
            self.builder
                .build_float_compare(pred, left, right, "fcmp")
                .map_err(err)?
        } else {
            let left = self.as_int(lhs)?;
            let right = self.as_int(rhs)?;
            let pred = match op {
                BinOp::Eq => IntPred::EQ,
                BinOp::Ne => IntPred::NE,
                BinOp::Lt => IntPred::SLT,
                BinOp::Gt => IntPred::SGT,
                BinOp::Le => IntPred::SLE,
                _ => IntPred::SGE,
            };
            self.builder
                .build_int_compare(pred, left, right, "icmp")
                .map_err(err)?
        };
        Ok(value.into())
    }

    // Short-circuit `and`/`or` over `i1` operands — the codebase's first intra-expression
    // branching. Evaluate the left operand; for `and` skip the right when it is false, for `or`
    // skip it when it is true; a `phi` in the merge block selects the short-circuit constant or
    // the right operand's value.
    fn gen_short_circuit(
        &mut self,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let lhs_val = self.expect_value(lhs)?.into_int_value(); // i1
        let entry_bb = self.builder.get_insert_block().expect("an active block");
        let function = entry_bb
            .get_parent()
            .expect("a block has a parent function");
        let rhs_bb = self.context.append_basic_block(function, "sc_rhs");
        let merge_bb = self.context.append_basic_block(function, "sc_merge");

        // `and`: branch to rhs when lhs is true; `or`: branch to rhs when lhs is false.
        let (then_bb, else_bb) = match op {
            BinOp::And => (rhs_bb, merge_bb),
            _ => (merge_bb, rhs_bb),
        };
        self.builder
            .build_conditional_branch(lhs_val, then_bb, else_bb)
            .map_err(err)?;

        self.builder.position_at_end(rhs_bb);
        let rhs_val = self.expect_value(rhs)?.into_int_value();
        // The right operand may itself have opened blocks; the phi's incoming edge is wherever we
        // ended up, not necessarily `rhs_bb`.
        let rhs_end_bb = self.builder.get_insert_block().expect("an active block");
        self.builder
            .build_unconditional_branch(merge_bb)
            .map_err(err)?;

        self.builder.position_at_end(merge_bb);
        let phi = self
            .builder
            .build_phi(self.bool_type(), "sc")
            .map_err(err)?;
        // On the short-circuit edge the result is `false` for `and`, `true` for `or`.
        let short = self.bool_type().const_int((op == BinOp::Or) as u64, false);
        phi.add_incoming(&[(&short, entry_bb), (&rhs_val, rhs_end_bb)]);
        Ok(phi.as_basic_value())
    }

    /// `llvm.pow.f64`, declared lazily on first use.
    fn pow_intrinsic(&self) -> FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("llvm.pow.f64") {
            return f;
        }
        let f64t = self.float_type();
        let fn_type = f64t.fn_type(&[f64t.into(), f64t.into()], false);
        self.module.add_function("llvm.pow.f64", fn_type, None)
    }

    // -------------------------------------------------------------------------
    // Value coercions
    // -------------------------------------------------------------------------

    /// Evaluates `expr_id`, requiring it to produce a value (errors on a void expression).
    pub(super) fn expect_value(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        self.gen_expr(expr_id)?
            .ok_or_else(|| "Expected a value but the expression is void.".to_string())
    }

    /// Evaluates `expr_id` as an `i64`, widening a `Bool` (`i1`) operand with a zero-extend.
    pub(super) fn as_int(
        &mut self,
        expr_id: ExprId,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let value = self.expect_value(expr_id)?.into_int_value();
        if self.ast.type_of(expr_id) == PinpType::Bool {
            self.builder
                .build_int_z_extend(value, self.int_type(), "bwiden")
                .map_err(err)
        } else {
            Ok(value)
        }
    }

    /// Evaluates `expr_id` as a float, inserting a widening conversion (`Bool`/`Int -> Float`) if needed.
    fn as_float(&mut self, expr_id: ExprId) -> Result<inkwell::values::FloatValue<'ctx>, String> {
        let value = self.expect_value(expr_id)?;
        match self.ast.type_of(expr_id) {
            PinpType::Int => self
                .builder
                .build_signed_int_to_float(value.into_int_value(), self.float_type(), "promote")
                .map_err(err),
            PinpType::Bool => self
                .builder
                .build_unsigned_int_to_float(value.into_int_value(), self.float_type(), "promote")
                .map_err(err),
            _ => Ok(value.into_float_value()),
        }
    }

    /// Widens a value up the `Bool -> Int -> Float` lattice when the target type requires it.
    pub(super) fn promote(
        &self,
        value: BasicValueEnum<'ctx>,
        from: PinpType,
        to: PinpType,
    ) -> BasicValueEnum<'ctx> {
        match (from, to) {
            (PinpType::Bool, PinpType::Int) => self
                .builder
                .build_int_z_extend(value.into_int_value(), self.int_type(), "bwiden")
                .expect("bool-to-int widening")
                .into(),
            (PinpType::Bool, PinpType::Float) => self
                .builder
                .build_unsigned_int_to_float(value.into_int_value(), self.float_type(), "promote")
                .expect("bool-to-float promotion")
                .into(),
            (PinpType::Int, PinpType::Float) => self
                .builder
                .build_signed_int_to_float(value.into_int_value(), self.float_type(), "promote")
                .expect("int-to-float promotion")
                .into(),
            _ => value,
        }
    }

    // -------------------------------------------------------------------------
    // Array helpers
    // -------------------------------------------------------------------------

    // The LLVM element type for an `ArrayElementType`.
    fn element_llvm_type(&self, elem: ArrayElementType) -> inkwell::types::BasicTypeEnum<'ctx> {
        match elem {
            ArrayElementType::Bool => self.bool_type().into(),
            ArrayElementType::Int => self.int_type().into(),
            ArrayElementType::Float => self.float_type().into(),
        }
    }

    // Size in bytes of one array element on the target (64-bit x86_64 assumed).
    fn element_byte_size(elem: ArrayElementType) -> u64 {
        match elem {
            ArrayElementType::Bool => 1,
            ArrayElementType::Int => 8,
            ArrayElementType::Float => 8,
        }
    }

    // Get or declare the `pinp_alloc(i64 size) -> ptr` external function.
    fn declare_pinp_alloc(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("pinp_alloc") {
            return f;
        }
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let fn_type = ptr_type.fn_type(&[self.int_type().into()], false);
        self.module.add_function("pinp_alloc", fn_type, None)
    }

    // Call `pinp_alloc(byte_count)` and return the resulting pointer.
    fn build_pinp_alloc(
        &self,
        byte_count: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let alloc_fn = self.declare_pinp_alloc();
        let call = self
            .builder
            .build_call(alloc_fn, &[byte_count.into()], "alloc")
            .map_err(err)?;
        Ok(basic_value(call.try_as_basic_value()).into_pointer_value())
    }

    // Build a GEP pointer to element `index` in an array starting at `base`.
    pub(super) fn build_element_ptr(
        &self,
        elem: ArrayElementType,
        base: inkwell::values::PointerValue<'ctx>,
        index: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let elem_type = self.element_llvm_type(elem);
        unsafe {
            self.builder
                .build_gep(elem_type, base, &[index], "elem_ptr")
                .map_err(err)
        }
    }

    // Emit a bounds check for `index` into an array of `length` elements. Emits a runtime error
    // call + unreachable on the out-of-bounds path; on the ok path returns a GEP (LLVM
    // `getelementptr`) — the address of element `index` within the heap buffer, computed by
    // scaling `index` by the element byte size and adding it to the base pointer.
    pub(super) fn bounds_check_and_gep(
        &mut self,
        elem: ArrayElementType,
        base: inkwell::values::PointerValue<'ctx>,
        index: inkwell::values::IntValue<'ctx>,
        length: usize,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let n = self.int_type().const_int(length as u64, false);
        let zero = self.int_type().const_zero();
        let too_low = self
            .builder
            .build_int_compare(IntPredicate::SLT, index, zero, "too_low")
            .map_err(err)?;
        let too_high = self
            .builder
            .build_int_compare(IntPredicate::SGE, index, n, "too_high")
            .map_err(err)?;
        let oob = self
            .builder
            .build_or(too_low, too_high, "oob")
            .map_err(err)?;
        let function = self.current_function();
        let err_bb = self.context.append_basic_block(function, "arr_oob");
        let ok_bb = self.context.append_basic_block(function, "arr_ok");
        self.builder
            .build_conditional_branch(oob, err_bb, ok_bb)
            .map_err(err)?;
        self.builder.position_at_end(err_bb);
        self.gen_runtime_error_call("Array index out of bounds.")?;
        self.builder.position_at_end(ok_bb);
        self.build_element_ptr(elem, base, index)
    }

    // -------------------------------------------------------------------------
    // Array expression emitters
    // -------------------------------------------------------------------------

    // `[e0, e1, …]` — allocate `n * elem_size` bytes, store each promoted element.
    // A single-element list whose sole element is a `Range` node is range-init syntax
    // (`[start..stop[:step]]`); that case is routed to `gen_array_from_range`.
    fn gen_array_literal(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let PinpType::Array(elem_type, n) = self.ast.type_of(expr_id) else {
            unreachable!("ArrayLiteral has Array type")
        };
        let Node::ArrayLiteral { elements } = self.ast.node(expr_id).clone() else {
            unreachable!("gen_array_literal on wrong node")
        };
        if elements.len() == 1 && matches!(self.ast.node(elements[0]), Node::Range { .. }) {
            return self.gen_array_from_range(elements[0], elem_type, n);
        }
        let byte_count = self
            .int_type()
            .const_int(n as u64 * Self::element_byte_size(elem_type), false);
        let array_ptr = self.build_pinp_alloc(byte_count)?;
        let elem_pinp_type = PinpType::from(elem_type);
        for (i, elem_id) in elements.iter().enumerate() {
            let raw = self.expect_value(*elem_id)?;
            let promoted = self.promote(raw, self.ast.type_of(*elem_id), elem_pinp_type);
            let index = self.int_type().const_int(i as u64, false);
            let ptr = self.build_element_ptr(elem_type, array_ptr, index)?;
            self.builder.build_store(ptr, promoted).map_err(err)?;
        }
        Ok(array_ptr.into())
    }

    // `[start..stop[:step]]` — allocate `n` elements and fill with the integer sequence via a
    // counted loop. `n` is already computed at sema time; the loop bound is a compile-time constant.
    // Range-init always produces `Int` elements (ranges are integer sequences).
    fn gen_array_from_range(
        &mut self,
        range_id: ExprId,
        elem_type: ArrayElementType,
        n: usize,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let byte_count = self
            .int_type()
            .const_int(n as u64 * Self::element_byte_size(elem_type), false);
        let array_ptr = self.build_pinp_alloc(byte_count)?;

        // Generate the range value — this gives us the (start, step) pair as LLVM values.
        let range_val = self.gen_range(range_id)?;
        let (start, _stop, step, _inclusive) = self.extract_range(range_val)?;

        // Counter slot: current range value. GEP index slot: current write position.
        let counter = self.alloca_at_entry(PinpType::Int)?;
        let gep_slot = self.alloca_at_entry(PinpType::Int)?;
        self.builder.build_store(counter, start).map_err(err)?;
        self.builder
            .build_store(gep_slot, self.int_type().const_zero())
            .map_err(err)?;

        let function = self.current_function();
        let header_bb = self.context.append_basic_block(function, "rinit_header");
        let body_bb = self.context.append_basic_block(function, "rinit_body");
        let exit_bb = self.context.append_basic_block(function, "rinit_exit");

        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(err)?;

        // Header: iterate `n` times.
        self.builder.position_at_end(header_bb);
        let gep_idx = self.load_counter(gep_slot)?;
        let n_val = self.int_type().const_int(n as u64, false);
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, gep_idx, n_val, "rinit_cond")
            .map_err(err)?;
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .map_err(err)?;

        // Body: store the current counter value and advance both counters.
        self.builder.position_at_end(body_bb);
        let cur_val = self.load_counter(counter)?;
        let gep_idx_body = self.load_counter(gep_slot)?;
        let elem_ptr = self.build_element_ptr(elem_type, array_ptr, gep_idx_body)?;
        let promoted = self.promote(cur_val.into(), PinpType::Int, PinpType::from(elem_type));
        self.builder.build_store(elem_ptr, promoted).map_err(err)?;
        let next_counter = self
            .builder
            .build_int_add(cur_val, step, "next_counter")
            .map_err(err)?;
        self.builder
            .build_store(counter, next_counter)
            .map_err(err)?;
        let one = self.int_type().const_int(1, false);
        let next_gep = self
            .builder
            .build_int_add(gep_idx_body, one, "next_gep")
            .map_err(err)?;
        self.builder.build_store(gep_slot, next_gep).map_err(err)?;
        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(err)?;

        self.builder.position_at_end(exit_bb);
        Ok(array_ptr.into())
    }

    // `array[index]` — scalar index: bounds-check then load one element.
    // `array[start..stop]` — slice index: allocate a new array and copy the sub-range.
    fn gen_index(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let Node::Index {
            array: array_id,
            index: index_id,
        } = self.ast.node(expr_id)
        else {
            unreachable!("gen_index on wrong node")
        };
        let (array_id, index_id) = (*array_id, *index_id);
        if matches!(self.ast.node(index_id), Node::Range { .. }) {
            return self.gen_slice(expr_id, array_id, index_id);
        }
        let PinpType::Array(elem_type, n) = self.ast.type_of(array_id) else {
            unreachable!("Index object has Array type")
        };
        let array_ptr = self.expect_value(array_id)?.into_pointer_value();
        let index = self.as_int(index_id)?;
        let elem_ptr = self.bounds_check_and_gep(elem_type, array_ptr, index, n)?;
        self.builder
            .build_load(self.element_llvm_type(elem_type), elem_ptr, "load_elem")
            .map_err(err)
    }

    // `array[start..stop]` — allocate a new `count`-element array and copy elements
    // `src[start .. start+count]` into it. Both bounds are literal integers (sema-enforced), so
    // the copy count and start offset fold to constants. LLVM's loop-idiom pass replaces a
    // fixed-count copy loop with a `memcpy` at `-O1`+.
    fn gen_slice(
        &mut self,
        expr_id: ExprId,
        array_id: ExprId,
        range_id: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let PinpType::Array(elem_type, count) = self.ast.type_of(expr_id) else {
            unreachable!("slice result has Array type")
        };
        let (start_lit, _, _) = self.read_slice_range(range_id);
        let array_ptr = self.expect_value(array_id)?.into_pointer_value();
        let start_offset = self.int_type().const_int(start_lit as u64, false);
        let byte_count = self
            .int_type()
            .const_int(count as u64 * Self::element_byte_size(elem_type), false);
        let dst_ptr = self.build_pinp_alloc(byte_count)?;

        let n_val = self.int_type().const_int(count as u64, false);
        let counter = self.alloca_at_entry(PinpType::Int)?;
        self.builder
            .build_store(counter, self.int_type().const_zero())
            .map_err(err)?;

        let function = self.current_function();
        let header = self.context.append_basic_block(function, "slice_hdr");
        let body = self.context.append_basic_block(function, "slice_body");
        let exit = self.context.append_basic_block(function, "slice_exit");

        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(header);
        let idx = self.load_counter(counter)?;
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, idx, n_val, "slice_cond")
            .map_err(err)?;
        self.builder
            .build_conditional_branch(cond, body, exit)
            .map_err(err)?;

        self.builder.position_at_end(body);
        let src_idx = self
            .builder
            .build_int_add(idx, start_offset, "src_idx")
            .map_err(err)?;
        let src_ptr = self.build_element_ptr(elem_type, array_ptr, src_idx)?;
        let val = self
            .builder
            .build_load(self.element_llvm_type(elem_type), src_ptr, "elem")
            .map_err(err)?;
        let dst_ep = self.build_element_ptr(elem_type, dst_ptr, idx)?;
        self.builder.build_store(dst_ep, val).map_err(err)?;
        let one = self.int_type().const_int(1, false);
        let next = self.builder.build_int_add(idx, one, "next").map_err(err)?;
        self.builder.build_store(counter, next).map_err(err)?;
        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(exit);
        Ok(dst_ptr.into())
    }

    // Extract the literal (start, stop, kind) from a slice Range node. Sema guarantees both bounds
    // are integer literals and the kind is Inclusive or UpExclusive.
    pub(super) fn read_slice_range(&self, range_id: ExprId) -> (i64, i64, RangeKind) {
        let (start_id, stop_id, kind) = match self.ast.node(range_id) {
            Node::Range {
                start, stop, kind, ..
            } => (*start, *stop, *kind),
            _ => unreachable!(),
        };
        let start_val = match self.ast.node(start_id) {
            Node::Int(val) => *val,
            _ => unreachable!("slice start is always a literal int"),
        };
        let stop_val = match self.ast.node(stop_id) {
            Node::Int(val) => *val,
            _ => unreachable!("slice stop is always a literal int"),
        };
        (start_val, stop_val, kind)
    }

    // `array.len` — returns the compile-time-known length as an `i64` constant.
    fn gen_member(&self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let Node::Member {
            object: object_id, ..
        } = self.ast.node(expr_id)
        else {
            unreachable!("gen_member on wrong node")
        };
        let object_id = *object_id;
        let PinpType::Array(_, n) = self.ast.type_of(object_id) else {
            unreachable!("Member object has Array type")
        };
        // Sema already validated the member name is `len`.
        Ok(self.int_type().const_int(n as u64, false).into())
    }

    // `[element for var[:type] in source]` — allocate array, loop over the range, store each
    // element. The range must have literal bounds (enforced by sema), so the loop bound is a
    // compile-time constant.
    fn gen_comprehension(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let Node::Comprehension {
            element,
            var,
            var_type,
            source,
        } = self.ast.node(expr_id)
        else {
            unreachable!("gen_comprehension on wrong node")
        };
        let (element, var, var_type, source) = (*element, *var, *var_type, *source);
        let PinpType::Array(elem_type, n) = self.ast.type_of(expr_id) else {
            unreachable!("Comprehension has Array type")
        };

        // Allocate the result array.
        let byte_count = self
            .int_type()
            .const_int(n as u64 * Self::element_byte_size(elem_type), false);
        let array_ptr = self.build_pinp_alloc(byte_count)?;

        // Generate the range (fires guard_zero_step if needed).
        let range_val = self.gen_range(source)?;
        let (start, _stop, step, _inclusive) = self.extract_range(range_val)?;

        // Allocate the range variable slot and the GEP index counter.
        let range_var_slot = self.alloca_at_entry(var_type)?;
        let gep_counter = self.alloca_at_entry(PinpType::Int)?;

        // Initialise: range variable ← start (promoted if Float), GEP counter ← 0.
        let init_var_val: BasicValueEnum = match var_type {
            PinpType::Float => self
                .builder
                .build_signed_int_to_float(start, self.float_type(), "init_float")
                .map_err(err)?
                .into(),
            _ => start.into(),
        };
        self.builder
            .build_store(range_var_slot, init_var_val)
            .map_err(err)?;
        self.builder
            .build_store(gep_counter, self.int_type().const_zero())
            .map_err(err)?;

        // Expose `var` in the local scope so the element expression can read it.
        self.push_scope();
        self.locals
            .last_mut()
            .unwrap()
            .insert(var, (range_var_slot, var_type));

        let function = self.current_function();
        let header_bb = self.context.append_basic_block(function, "compr_header");
        let body_bb = self.context.append_basic_block(function, "compr_body");
        let exit_bb = self.context.append_basic_block(function, "compr_exit");

        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(err)?;

        // Header: loop while GEP counter < n.
        self.builder.position_at_end(header_bb);
        let gep_idx = self.load_counter(gep_counter)?;
        let n_val = self.int_type().const_int(n as u64, false);
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, gep_idx, n_val, "compr_cond")
            .map_err(err)?;
        self.builder
            .build_conditional_branch(cond, body_bb, exit_bb)
            .map_err(err)?;

        // Body: evaluate element, GEP, store; then advance both counters.
        self.builder.position_at_end(body_bb);
        let raw = self.expect_value(element)?;
        let promoted = self.promote(raw, self.ast.type_of(element), PinpType::from(elem_type));
        let gep_idx_body = self.load_counter(gep_counter)?;
        let elem_ptr = self.build_element_ptr(elem_type, array_ptr, gep_idx_body)?;
        self.builder.build_store(elem_ptr, promoted).map_err(err)?;

        // Advance GEP counter.
        let one = self.int_type().const_int(1, false);
        let next_gep = self
            .builder
            .build_int_add(gep_idx_body, one, "next_gep")
            .map_err(err)?;
        self.builder
            .build_store(gep_counter, next_gep)
            .map_err(err)?;

        // Advance range variable.
        match var_type {
            PinpType::Float => {
                let cur_float = self
                    .builder
                    .build_load(self.float_type(), range_var_slot, "cur_float")
                    .map_err(err)?
                    .into_float_value();
                let float_step = self
                    .builder
                    .build_signed_int_to_float(step, self.float_type(), "float_step")
                    .map_err(err)?;
                let next_float = self
                    .builder
                    .build_float_add(cur_float, float_step, "next_float")
                    .map_err(err)?;
                self.builder
                    .build_store(range_var_slot, next_float)
                    .map_err(err)?;
            }
            _ => {
                let cur_int = self
                    .builder
                    .build_load(self.int_type(), range_var_slot, "cur_int")
                    .map_err(err)?
                    .into_int_value();
                let next_int = self
                    .builder
                    .build_int_add(cur_int, step, "next_int")
                    .map_err(err)?;
                self.builder
                    .build_store(range_var_slot, next_int)
                    .map_err(err)?;
            }
        }

        self.builder
            .build_unconditional_branch(header_bb)
            .map_err(err)?;

        // Exit: clean up scope and return the allocated pointer.
        self.builder.position_at_end(exit_bb);
        self.pop_scope();

        Ok(array_ptr.into())
    }

    pub(super) fn zero(&self, pinp_type: PinpType) -> BasicValueEnum<'ctx> {
        match pinp_type {
            PinpType::Bool => self.bool_type().const_zero().into(),
            PinpType::Int => self.int_type().const_zero().into(),
            PinpType::Float => self.float_type().const_zero().into(),
            PinpType::Void => unreachable!("Void has no zero value."),
            PinpType::Range => self.range_type().const_zero().into(),
            // Null pointer is the zero for array/matrix slots (must be overwritten before use).
            PinpType::Array(_, _) | PinpType::Matrix(_, _, _) => self
                .context
                .ptr_type(AddressSpace::default())
                .const_null()
                .into(),
        }
    }
}
