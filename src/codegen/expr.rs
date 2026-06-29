// SPDX-License-Identifier: MIT

use super::*;

use inkwell::IntPredicate;
use inkwell::values::BasicMetadataValueEnum;

use crate::parser::{ArrayElementType, BinOp, BuiltinMember, ExprId, Node, RangeKind, UnOp};

// Classification of one 2D dimension selector, derived by `classify_2d_selector`.
#[derive(Clone, Copy)]
struct DimSlice {
    start: usize,
    len: usize,
}

enum DimSel {
    Scalar,
    Slice(DimSlice),
}

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
            Node::Str(_) | Node::FStr { .. } => todo!("string codegen — step 5"),
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
            Node::Call { callee, args } => {
                if self.ast.names[callee.value()] == "identity" {
                    return Ok(Some(self.gen_identity(expr_id)?));
                }
                return self.gen_call(*callee, args);
            }
            Node::If { .. } => return self.gen_if(expr_id),
            Node::Range { .. } => self.gen_range(expr_id)?,
            Node::Membership { .. } => self.gen_membership(expr_id)?,
            Node::ArrayLiteral { .. } => self.gen_array_literal(expr_id)?,
            Node::Index { .. } => self.gen_index(expr_id)?,
            Node::Member { .. } => self.gen_builtin_member(expr_id)?,
            Node::Comprehension { .. } => self.gen_comprehension(expr_id)?,
            Node::MatrixLiteral { .. } => self.gen_matrix_literal(expr_id)?,
            Node::Index2D { .. } => self.gen_index2d(expr_id)?,
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
    // Compound-assign helpers
    // -------------------------------------------------------------------------

    // Apply a compound-assignment operator to a pre-loaded element value and an evaluated RHS.
    // Used by `gen_indexed_assign` and `gen_indexed_assign2d` to avoid evaluating the index
    // expression twice.
    pub(super) fn apply_compound_op(
        &mut self,
        op: BinOp,
        current: BasicValueEnum<'ctx>,
        elem_type: ArrayElementType,
        rhs_id: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let rhs = self.expect_value(rhs_id)?;
        let rhs_type = self.ast.type_of(rhs_id);
        let elem_pinp_type = PinpType::from(elem_type);
        // Determine the result type by the standard bin_type rules.
        let use_float = matches!(op, BinOp::Div)
            || elem_pinp_type == PinpType::Float
            || rhs_type == PinpType::Float;
        Ok(if use_float {
            let left = self
                .promote(current, elem_pinp_type, PinpType::Float)
                .into_float_value();
            let right = self
                .promote(rhs, rhs_type, PinpType::Float)
                .into_float_value();
            match op {
                BinOp::Add => self
                    .builder
                    .build_float_add(left, right, "fadd")
                    .map_err(err)?
                    .into(),
                BinOp::Sub => self
                    .builder
                    .build_float_sub(left, right, "fsub")
                    .map_err(err)?
                    .into(),
                BinOp::Mul => self
                    .builder
                    .build_float_mul(left, right, "fmul")
                    .map_err(err)?
                    .into(),
                BinOp::Div => self
                    .builder
                    .build_float_div(left, right, "fdiv")
                    .map_err(err)?
                    .into(),
                BinOp::Pow => {
                    let pow_fn = self.pow_intrinsic();
                    let call = self
                        .builder
                        .build_call(pow_fn, &[left.into(), right.into()], "pow")
                        .map_err(err)?;
                    basic_value(call.try_as_basic_value())
                }
                _ => unreachable!("compound op {op:?} does not produce Float"),
            }
        } else {
            let left = self
                .promote(current, elem_pinp_type, PinpType::Int)
                .into_int_value();
            let right = self.promote(rhs, rhs_type, PinpType::Int).into_int_value();
            match op {
                BinOp::Add => self
                    .builder
                    .build_int_add(left, right, "add")
                    .map_err(err)?
                    .into(),
                BinOp::Sub => self
                    .builder
                    .build_int_sub(left, right, "sub")
                    .map_err(err)?
                    .into(),
                BinOp::Mul => self
                    .builder
                    .build_int_mul(left, right, "mul")
                    .map_err(err)?
                    .into(),
                BinOp::IntDiv => self
                    .builder
                    .build_int_signed_div(left, right, "sdiv")
                    .map_err(err)?
                    .into(),
                BinOp::Mod => self
                    .builder
                    .build_int_signed_rem(left, right, "srem")
                    .map_err(err)?
                    .into(),
                BinOp::Pow => {
                    // Int ^ Int: use float pow then truncate.
                    let lf = self
                        .builder
                        .build_signed_int_to_float(left, self.float_type(), "powi_l")
                        .map_err(err)?;
                    let rf = self
                        .builder
                        .build_signed_int_to_float(right, self.float_type(), "powi_r")
                        .map_err(err)?;
                    let pow_fn = self.pow_intrinsic();
                    let call = self
                        .builder
                        .build_call(pow_fn, &[lf.into(), rf.into()], "pow")
                        .map_err(err)?;
                    let result_f = basic_value(call.try_as_basic_value()).into_float_value();
                    self.builder
                        .build_float_to_signed_int(result_f, self.int_type(), "powi")
                        .map_err(err)?
                        .into()
                }
                _ => unreachable!("compound int op {op:?}"),
            }
        })
    }

    // Array helpers
    // -------------------------------------------------------------------------

    // The LLVM element type for an `ArrayElementType`.
    pub(super) fn element_llvm_type(
        &self,
        elem: ArrayElementType,
    ) -> inkwell::types::BasicTypeEnum<'ctx> {
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
    // `select(i < 0, i + len, i)` — folds away at compile time for literal indices.
    fn normalize_index(
        &mut self,
        index: inkwell::values::IntValue<'ctx>,
        len: inkwell::values::IntValue<'ctx>,
    ) -> Result<inkwell::values::IntValue<'ctx>, String> {
        let zero = self.int_type().const_zero();
        let is_neg = self
            .builder
            .build_int_compare(IntPredicate::SLT, index, zero, "is_neg")
            .map_err(err)?;
        let adjusted = self
            .builder
            .build_int_add(index, len, "adj_idx")
            .map_err(err)?;
        Ok(self
            .builder
            .build_select(is_neg, adjusted, index, "eff_idx")
            .map_err(err)?
            .into_int_value())
    }

    pub(super) fn bounds_check_and_gep(
        &mut self,
        elem: ArrayElementType,
        base: inkwell::values::PointerValue<'ctx>,
        index: inkwell::values::IntValue<'ctx>,
        length: usize,
    ) -> Result<inkwell::values::PointerValue<'ctx>, String> {
        let n = self.int_type().const_int(length as u64, false);
        let index = self.normalize_index(index, n)?;
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

    // `[r0; r1; …]` — allocate `rows * cols * elem_size` bytes in row-major order, store each
    // promoted element at flat offset `r * col_count + c`.
    fn gen_matrix_literal(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let PinpType::Matrix(elem_type, row_count, col_count) = self.ast.type_of(expr_id) else {
            unreachable!("MatrixLiteral has Matrix type")
        };
        let Node::MatrixLiteral { rows } = self.ast.node(expr_id).clone() else {
            unreachable!("gen_matrix_literal on wrong node")
        };
        let total = row_count * col_count;
        let byte_count = self
            .int_type()
            .const_int(total as u64 * Self::element_byte_size(elem_type), false);
        let matrix_ptr = self.build_pinp_alloc(byte_count)?;
        let elem_pinp_type = PinpType::from(elem_type);
        for (row_idx, row) in rows.iter().enumerate() {
            for (col_idx, elem_id) in row.iter().enumerate() {
                let raw = self.expect_value(*elem_id)?;
                let promoted = self.promote(raw, self.ast.type_of(*elem_id), elem_pinp_type);
                let flat = self
                    .int_type()
                    .const_int((row_idx * col_count + col_idx) as u64, false);
                let ptr = self.build_element_ptr(elem_type, matrix_ptr, flat)?;
                self.builder.build_store(ptr, promoted).map_err(err)?;
            }
        }
        Ok(matrix_ptr.into())
    }

    // `identity(n, type)` — allocate n×n elements; zero the block with memset, then emit n diagonal
    // stores of 1/1.0. O(n) stores instead of the previous O(n²) — LLVM sees n compile-time
    // constants rather than n² mixed constant/zero values.
    fn gen_identity(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let PinpType::Matrix(elem_type, n, _) = self.ast.type_of(expr_id) else {
            unreachable!("identity call has Matrix type")
        };
        let byte_count = self.int_type().const_int(
            n as u64 * n as u64 * Self::element_byte_size(elem_type),
            false,
        );
        let matrix_ptr = self.build_pinp_alloc(byte_count)?;
        // Zero every byte — diagonal elements will be overwritten with 1/1.0 below.
        let memset_fn = self.declare_memset();
        let zero_byte = self.context.i32_type().const_zero();
        self.builder
            .build_call(
                memset_fn,
                &[matrix_ptr.into(), zero_byte.into(), byte_count.into()],
                "memset",
            )
            .map_err(err)?;
        // Store 1 or 1.0 at each diagonal position i*n + i.
        let one: BasicValueEnum = match elem_type {
            ArrayElementType::Int => self.int_type().const_int(1, false).into(),
            ArrayElementType::Float => self.float_type().const_float(1.0).into(),
            ArrayElementType::Bool => unreachable!("identity() rejects Bool element type at sema"),
        };
        for i in 0..n {
            let flat = self.int_type().const_int((i * n + i) as u64, false);
            let ptr = self.build_element_ptr(elem_type, matrix_ptr, flat)?;
            self.builder.build_store(ptr, one).map_err(err)?;
        }
        Ok(matrix_ptr.into())
    }

    // Get or declare libc's `memset(ptr, i32 value, i64 size) -> ptr` external function.
    fn declare_memset(&self) -> inkwell::values::FunctionValue<'ctx> {
        if let Some(f) = self.module.get_function("memset") {
            return f;
        }
        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let fn_type = ptr_type.fn_type(
            &[
                ptr_type.into(),
                self.context.i32_type().into(),
                self.int_type().into(),
            ],
            false,
        );
        self.module.add_function("memset", fn_type, None)
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

    // -------------------------------------------------------------------------
    // 2D indexing and slicing
    // -------------------------------------------------------------------------

    // `mat[row_sel, col_sel]` — dispatch to scalar read, row slice, col slice, or submatrix
    // based on the compile-time shape of each selector.
    fn gen_index2d(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let Node::Index2D { matrix, row, col } = self.ast.node(expr_id) else {
            unreachable!("gen_index2d on wrong node")
        };
        let (matrix_id, row_id, col_id) = (*matrix, *row, *col);
        let PinpType::Matrix(elem_type, rows, cols) = self.ast.type_of(matrix_id) else {
            unreachable!("Index2D matrix has Matrix type")
        };
        let matrix_ptr = self.expect_value(matrix_id)?.into_pointer_value();
        let row_sel = self.classify_2d_selector(row_id, rows);
        let col_sel = self.classify_2d_selector(col_id, cols);
        match (row_sel, col_sel) {
            (DimSel::Scalar, DimSel::Scalar) => {
                self.gen_index2d_elem(elem_type, matrix_ptr, row_id, col_id, rows, cols)
            }
            (DimSel::Scalar, DimSel::Slice(col_slice)) => {
                self.gen_index2d_row_slice(elem_type, matrix_ptr, row_id, rows, cols, col_slice)
            }
            (DimSel::Slice(row_slice), DimSel::Scalar) => {
                self.gen_index2d_col_slice(elem_type, matrix_ptr, col_id, cols, row_slice)
            }
            (DimSel::Slice(row_slice), DimSel::Slice(col_slice)) => {
                self.gen_index2d_submatrix(elem_type, matrix_ptr, cols, row_slice, col_slice)
            }
        }
    }

    // Classify one dimension selector as scalar (runtime-checked Int) or compile-time slice.
    // `FullExtent` (`:`) becomes `Slice { start: 0, len: dimension }`.
    fn classify_2d_selector(&self, sel_id: ExprId, dimension: usize) -> DimSel {
        match self.ast.node(sel_id) {
            Node::FullExtent => DimSel::Slice(DimSlice {
                start: 0,
                len: dimension,
            }),
            Node::Range {
                start, stop, kind, ..
            } => {
                let start_val = match self.ast.node(*start) {
                    Node::Int(val) => *val as usize,
                    _ => unreachable!("slice bound is always a literal int"),
                };
                let stop_val = match self.ast.node(*stop) {
                    Node::Int(val) => *val as usize,
                    _ => unreachable!("slice bound is always a literal int"),
                };
                let len = match kind {
                    RangeKind::Inclusive => stop_val - start_val + 1,
                    RangeKind::UpExclusive => stop_val - start_val,
                    RangeKind::DownExclusive => {
                        unreachable!("sema rejects step ranges in 2D slice position")
                    }
                };
                DimSel::Slice(DimSlice {
                    start: start_val,
                    len,
                })
            }
            _ => DimSel::Scalar,
        }
    }

    // Bounds-check both dimensions and return a pointer to element `[row, col]`. Shared by the
    // read path (`gen_index2d_elem`) and the write path (`gen_indexed_assign2d` in stmt.rs).
    pub(super) fn bounds_check_and_gep_2d(
        &mut self,
        elem_type: ArrayElementType,
        matrix_ptr: PointerValue<'ctx>,
        row_val: inkwell::values::IntValue<'ctx>,
        col_val: inkwell::values::IntValue<'ctx>,
        rows: usize,
        cols: usize,
    ) -> Result<PointerValue<'ctx>, String> {
        let func = self.current_function();
        let zero = self.int_type().const_zero();
        let rows_val = self.int_type().const_int(rows as u64, false);
        let row_val = self.normalize_index(row_val, rows_val)?;
        let row_lo = self
            .builder
            .build_int_compare(IntPredicate::SLT, row_val, zero, "row_lo")
            .map_err(err)?;
        let row_hi = self
            .builder
            .build_int_compare(IntPredicate::SGE, row_val, rows_val, "row_hi")
            .map_err(err)?;
        let row_oob = self
            .builder
            .build_or(row_lo, row_hi, "row_oob")
            .map_err(err)?;
        let row_err = self.context.append_basic_block(func, "mat_row_err");
        let row_ok = self.context.append_basic_block(func, "mat_row_ok");
        self.builder
            .build_conditional_branch(row_oob, row_err, row_ok)
            .map_err(err)?;
        self.builder.position_at_end(row_err);
        self.gen_runtime_error_call("Array index out of bounds.")?;
        self.builder.position_at_end(row_ok);
        let cols_val = self.int_type().const_int(cols as u64, false);
        let col_val = self.normalize_index(col_val, cols_val)?;
        let col_lo = self
            .builder
            .build_int_compare(IntPredicate::SLT, col_val, zero, "col_lo")
            .map_err(err)?;
        let col_hi = self
            .builder
            .build_int_compare(IntPredicate::SGE, col_val, cols_val, "col_hi")
            .map_err(err)?;
        let col_oob = self
            .builder
            .build_or(col_lo, col_hi, "col_oob")
            .map_err(err)?;
        let col_err = self.context.append_basic_block(func, "mat_col_err");
        let col_ok = self.context.append_basic_block(func, "mat_col_ok");
        self.builder
            .build_conditional_branch(col_oob, col_err, col_ok)
            .map_err(err)?;
        self.builder.position_at_end(col_err);
        self.gen_runtime_error_call("Array index out of bounds.")?;
        self.builder.position_at_end(col_ok);
        let row_scaled = self
            .builder
            .build_int_mul(row_val, cols_val, "row_scaled")
            .map_err(err)?;
        let flat = self
            .builder
            .build_int_add(row_scaled, col_val, "flat_idx")
            .map_err(err)?;
        self.build_element_ptr(elem_type, matrix_ptr, flat)
    }

    // `mat[r, c]` — scalar element read with runtime bounds checks on both dimensions.
    fn gen_index2d_elem(
        &mut self,
        elem_type: ArrayElementType,
        matrix_ptr: PointerValue<'ctx>,
        row_id: ExprId,
        col_id: ExprId,
        rows: usize,
        cols: usize,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let row_val = self.as_int(row_id)?;
        let col_val = self.as_int(col_id)?;
        let elem_ptr =
            self.bounds_check_and_gep_2d(elem_type, matrix_ptr, row_val, col_val, rows, cols)?;
        self.builder
            .build_load(self.element_llvm_type(elem_type), elem_ptr, "load_elem")
            .map_err(err)
    }

    // `mat[r, c1..c1+cl]` — runtime-check row, copy `cl` elements from that row.
    fn gen_index2d_row_slice(
        &mut self,
        elem_type: ArrayElementType,
        matrix_ptr: PointerValue<'ctx>,
        row_id: ExprId,
        rows: usize,
        cols: usize,
        col_slice: DimSlice,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let row_val = self.as_int(row_id)?;
        let func = self.current_function();
        let rows_val = self.int_type().const_int(rows as u64, false);
        let zero = self.int_type().const_zero();
        let row_lo = self
            .builder
            .build_int_compare(IntPredicate::SLT, row_val, zero, "row_lo")
            .map_err(err)?;
        let row_hi = self
            .builder
            .build_int_compare(IntPredicate::SGE, row_val, rows_val, "row_hi")
            .map_err(err)?;
        let row_oob = self
            .builder
            .build_or(row_lo, row_hi, "row_oob")
            .map_err(err)?;
        let err_bb = self.context.append_basic_block(func, "rslice_err");
        let ok_bb = self.context.append_basic_block(func, "rslice_ok");
        self.builder
            .build_conditional_branch(row_oob, err_bb, ok_bb)
            .map_err(err)?;
        self.builder.position_at_end(err_bb);
        self.gen_runtime_error_call("Array index out of bounds.")?;
        self.builder.position_at_end(ok_bb);

        let byte_count = self.int_type().const_int(
            col_slice.len as u64 * Self::element_byte_size(elem_type),
            false,
        );
        let dst_ptr = self.build_pinp_alloc(byte_count)?;
        let cols_val = self.int_type().const_int(cols as u64, false);
        let c1_val = self.int_type().const_int(col_slice.start as u64, false);
        let cl_val = self.int_type().const_int(col_slice.len as u64, false);
        let row_base = self
            .builder
            .build_int_mul(row_val, cols_val, "row_base")
            .map_err(err)?;

        let counter = self.alloca_at_entry(PinpType::Int)?;
        self.builder.build_store(counter, zero).map_err(err)?;
        let header = self.context.append_basic_block(func, "rslice_hdr");
        let body = self.context.append_basic_block(func, "rslice_body");
        let exit = self.context.append_basic_block(func, "rslice_exit");
        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(header);
        let k = self.load_counter(counter)?;
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, k, cl_val, "rslice_cond")
            .map_err(err)?;
        self.builder
            .build_conditional_branch(cond, body, exit)
            .map_err(err)?;

        self.builder.position_at_end(body);
        let src_col = self
            .builder
            .build_int_add(k, c1_val, "src_col")
            .map_err(err)?;
        let src_flat = self
            .builder
            .build_int_add(row_base, src_col, "src_flat")
            .map_err(err)?;
        let src_ptr = self.build_element_ptr(elem_type, matrix_ptr, src_flat)?;
        let val = self
            .builder
            .build_load(self.element_llvm_type(elem_type), src_ptr, "elem")
            .map_err(err)?;
        let dst_ep = self.build_element_ptr(elem_type, dst_ptr, k)?;
        self.builder.build_store(dst_ep, val).map_err(err)?;
        let one = self.int_type().const_int(1, false);
        let next_k = self.builder.build_int_add(k, one, "next_k").map_err(err)?;
        self.builder.build_store(counter, next_k).map_err(err)?;
        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(exit);
        Ok(dst_ptr.into())
    }

    // `mat[r1..r1+rl, c]` — runtime-check col, copy `rl` elements from that column.
    fn gen_index2d_col_slice(
        &mut self,
        elem_type: ArrayElementType,
        matrix_ptr: PointerValue<'ctx>,
        col_id: ExprId,
        cols: usize,
        row_slice: DimSlice,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let col_val = self.as_int(col_id)?;
        let func = self.current_function();
        let cols_val = self.int_type().const_int(cols as u64, false);
        let zero = self.int_type().const_zero();
        let col_lo = self
            .builder
            .build_int_compare(IntPredicate::SLT, col_val, zero, "col_lo")
            .map_err(err)?;
        let col_hi = self
            .builder
            .build_int_compare(IntPredicate::SGE, col_val, cols_val, "col_hi")
            .map_err(err)?;
        let col_oob = self
            .builder
            .build_or(col_lo, col_hi, "col_oob")
            .map_err(err)?;
        let err_bb = self.context.append_basic_block(func, "cslice_err");
        let ok_bb = self.context.append_basic_block(func, "cslice_ok");
        self.builder
            .build_conditional_branch(col_oob, err_bb, ok_bb)
            .map_err(err)?;
        self.builder.position_at_end(err_bb);
        self.gen_runtime_error_call("Array index out of bounds.")?;
        self.builder.position_at_end(ok_bb);

        let byte_count = self.int_type().const_int(
            row_slice.len as u64 * Self::element_byte_size(elem_type),
            false,
        );
        let dst_ptr = self.build_pinp_alloc(byte_count)?;
        let r1_val = self.int_type().const_int(row_slice.start as u64, false);
        let rl_val = self.int_type().const_int(row_slice.len as u64, false);

        let counter = self.alloca_at_entry(PinpType::Int)?;
        self.builder.build_store(counter, zero).map_err(err)?;
        let header = self.context.append_basic_block(func, "cslice_hdr");
        let body = self.context.append_basic_block(func, "cslice_body");
        let exit = self.context.append_basic_block(func, "cslice_exit");
        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(header);
        let k = self.load_counter(counter)?;
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, k, rl_val, "cslice_cond")
            .map_err(err)?;
        self.builder
            .build_conditional_branch(cond, body, exit)
            .map_err(err)?;

        self.builder.position_at_end(body);
        let src_row = self
            .builder
            .build_int_add(k, r1_val, "src_row")
            .map_err(err)?;
        let src_row_off = self
            .builder
            .build_int_mul(src_row, cols_val, "src_row_off")
            .map_err(err)?;
        let src_flat = self
            .builder
            .build_int_add(src_row_off, col_val, "src_flat")
            .map_err(err)?;
        let src_ptr = self.build_element_ptr(elem_type, matrix_ptr, src_flat)?;
        let val = self
            .builder
            .build_load(self.element_llvm_type(elem_type), src_ptr, "elem")
            .map_err(err)?;
        let dst_ep = self.build_element_ptr(elem_type, dst_ptr, k)?;
        self.builder.build_store(dst_ep, val).map_err(err)?;
        let one = self.int_type().const_int(1, false);
        let next_k = self.builder.build_int_add(k, one, "next_k").map_err(err)?;
        self.builder.build_store(counter, next_k).map_err(err)?;
        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(exit);
        Ok(dst_ptr.into())
    }

    // `mat[r1..r1+rl, c1..c1+cl]` — both dimensions sema-verified; copy `rl×cl` elements
    // into a new matrix using a single flat counter `k in 0..rl*cl`.
    fn gen_index2d_submatrix(
        &mut self,
        elem_type: ArrayElementType,
        matrix_ptr: PointerValue<'ctx>,
        cols: usize,
        row_slice: DimSlice,
        col_slice: DimSlice,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let total = row_slice.len * col_slice.len;
        let byte_count = self
            .int_type()
            .const_int(total as u64 * Self::element_byte_size(elem_type), false);
        let dst_ptr = self.build_pinp_alloc(byte_count)?;
        let cols_val = self.int_type().const_int(cols as u64, false);
        let cl_val = self.int_type().const_int(col_slice.len as u64, false);
        let r1_val = self.int_type().const_int(row_slice.start as u64, false);
        let c1_val = self.int_type().const_int(col_slice.start as u64, false);
        let total_val = self.int_type().const_int(total as u64, false);
        let zero = self.int_type().const_zero();

        let counter = self.alloca_at_entry(PinpType::Int)?;
        self.builder.build_store(counter, zero).map_err(err)?;
        let func = self.current_function();
        let header = self.context.append_basic_block(func, "sub_hdr");
        let body = self.context.append_basic_block(func, "sub_body");
        let exit = self.context.append_basic_block(func, "sub_exit");
        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(header);
        let k = self.load_counter(counter)?;
        let cond = self
            .builder
            .build_int_compare(IntPredicate::SLT, k, total_val, "sub_cond")
            .map_err(err)?;
        self.builder
            .build_conditional_branch(cond, body, exit)
            .map_err(err)?;

        self.builder.position_at_end(body);
        let sub_row = self
            .builder
            .build_int_signed_div(k, cl_val, "sub_row")
            .map_err(err)?;
        let sub_col = self
            .builder
            .build_int_signed_rem(k, cl_val, "sub_col")
            .map_err(err)?;
        let src_row = self
            .builder
            .build_int_add(sub_row, r1_val, "src_row")
            .map_err(err)?;
        let src_col = self
            .builder
            .build_int_add(sub_col, c1_val, "src_col")
            .map_err(err)?;
        let src_row_off = self
            .builder
            .build_int_mul(src_row, cols_val, "src_row_off")
            .map_err(err)?;
        let src_flat = self
            .builder
            .build_int_add(src_row_off, src_col, "src_flat")
            .map_err(err)?;
        let src_ptr = self.build_element_ptr(elem_type, matrix_ptr, src_flat)?;
        let val = self
            .builder
            .build_load(self.element_llvm_type(elem_type), src_ptr, "elem")
            .map_err(err)?;
        let dst_ep = self.build_element_ptr(elem_type, dst_ptr, k)?;
        self.builder.build_store(dst_ep, val).map_err(err)?;
        let one = self.int_type().const_int(1, false);
        let next_k = self.builder.build_int_add(k, one, "next_k").map_err(err)?;
        self.builder.build_store(counter, next_k).map_err(err)?;
        self.builder
            .build_unconditional_branch(header)
            .map_err(err)?;

        self.builder.position_at_end(exit);
        Ok(dst_ptr.into())
    }

    // Compiler built-in pseudo-members like .len, .ndim, etc
    fn gen_builtin_member(&self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let Node::Member {
            object: object_id, ..
        } = self.ast.node(expr_id)
        else {
            unreachable!("gen_member on wrong node")
        };
        let object_id = *object_id;
        // Sema resolved the member name to a BuiltinMember variant; we match the enum here so
        // the compiler enforces exhaustiveness — a new variant that compiles in sema but is
        // unhandled in codegen becomes a compile error, not a silent runtime `unreachable!`.
        let builtin = self
            .ast
            .builtin_member_of(expr_id)
            .expect("Sema must resolve every Node::Member to a BuiltinMember");
        let const_val: u64 = match (builtin, self.ast.type_of(object_id)) {
            (BuiltinMember::Len, PinpType::Array(_, n)) => n as u64,
            (BuiltinMember::Len, PinpType::Matrix(_, rows, cols)) => (rows * cols) as u64,
            (BuiltinMember::Ndim, PinpType::Array(..)) => 1,
            (BuiltinMember::Ndim, PinpType::Matrix(..)) => 2,
            (BuiltinMember::Rows, PinpType::Matrix(_, rows, _)) => rows as u64,
            (BuiltinMember::Cols, PinpType::Matrix(_, _, cols)) => cols as u64,
            _ => unreachable!("Sema rejected invalid (member, type) combination"),
        };
        Ok(self.int_type().const_int(const_val, false).into())
    }

    // `[element for var[:type] in source]` — allocate array, loop over the range, store each
    // element. The range must have literal bounds (enforced by sema), so the loop bound is a
    // compile-time constant..0
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
            PinpType::Str => todo!("string codegen — step 5"),
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
