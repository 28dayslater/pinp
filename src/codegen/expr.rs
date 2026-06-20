// SPDX-License-Identifier: MIT

use super::*;

use inkwell::IntPredicate;
use inkwell::values::BasicMetadataValueEnum;

use crate::parser::{BinOp, ExprId, Node, UnOp};

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

    pub(super) fn zero(&self, pinp_type: PinpType) -> BasicValueEnum<'ctx> {
        match pinp_type {
            PinpType::Bool => self.bool_type().const_zero().into(),
            PinpType::Int => self.int_type().const_zero().into(),
            PinpType::Float => self.float_type().const_zero().into(),
            PinpType::Void => unreachable!("Void has no zero value."),
            PinpType::Range => self.range_type().const_zero().into(),
        }
    }
}
