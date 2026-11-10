// SPDX-License-Identifier: MIT

//! Values: constants, variables, operators, conversions, calls, and ranges.

use cranelift_codegen::ir::condcodes::{FloatCC, IntCC};
use cranelift_codegen::ir::immediates::Ieee64;

use super::*;
use crate::ir::{Constant, List, RangeFieldKind};
use crate::parser::{BinOp, RangeKind, UnOp};

impl FunctionEmitter<'_, '_> {
    /// The words of the value `node_id` computes.
    pub(super) fn emit_value(&mut self, node_id: NodeId) -> Words {
        let function = self.function;
        let pinp_type = self.type_of(node_id);
        match function.arena.operations[node_id] {
            Operation::Constant(constant_id) => {
                self.emit_constant(self.program.tables.constants.get(constant_id))
            }
            Operation::ReadLocal(local_id) => {
                let mut read = Words::none();
                for (index, &variable) in self.local_variables(local_id).to_vec().iter().enumerate()
                {
                    read.values[index] = self.builder.use_var(variable);
                }
                read.count = words(pinp_type).len();
                read
            }
            Operation::ReadGlobal(global_id) => {
                let address = self.global_address(global_id);
                self.load_words(pinp_type, address)
            }
            Operation::Unary { op, operand } => {
                let operand = self.emit_value(operand).single();
                let ins = self.builder.ins();
                Words::one(match (op, pinp_type) {
                    (UnOp::Neg, PinpType::Float) => ins.fneg(operand),
                    (UnOp::Neg, _) => ins.ineg(operand),
                    (UnOp::Not, _) => ins.bxor_imm_s(operand, 1),
                })
            }
            Operation::Binary { op, lhs, rhs } => self.emit_binary(op, lhs, rhs, pinp_type),
            Operation::IntegerDivide {
                remainder,
                checked,
                lhs,
                rhs,
            } => self.emit_integer_divide(remainder, checked, lhs, rhs),
            Operation::Convert { operand } => {
                let from = self.type_of(operand);
                let operand = self.emit_value(operand);
                self.emit_convert(operand, from, pinp_type)
            }
            Operation::RangeField { range, field } => {
                let range = self.emit_value(range);
                let index = match field {
                    RangeFieldKind::Start => 0,
                    RangeFieldKind::Stop => 1,
                    RangeFieldKind::Step => 2,
                    RangeFieldKind::Inclusive => 3,
                };
                Words::one(range.values[index])
            }
            Operation::Select {
                cond,
                if_true,
                if_false,
            } => {
                let cond = self.emit_value(cond).single();
                let if_true = self.emit_value(if_true);
                let if_false = self.emit_value(if_false);
                let mut selected = if_true;
                for index in 0..if_true.count {
                    selected.values[index] = self.builder.ins().select(
                        cond,
                        if_true.values[index],
                        if_false.values[index],
                    );
                }
                selected
            }
            Operation::Call { callee, args } => {
                let args = self.emit_arguments(args);
                let func_id = self.program.functions[callee];
                self.call_function(func_id, &args)
            }
            Operation::RuntimeCall { function, args } => self.emit_runtime_call(function, args),
            Operation::Range {
                kind,
                start,
                stop,
                step,
            } => self.emit_range(kind, start, stop, step),
            Operation::Membership { value, range } => self.emit_membership(value, range),
            Operation::ArrayNew { elements } => self.emit_array_new(elements, pinp_type),
            Operation::ArrayAllocate { zeroed } => {
                Words::one(self.emit_array_allocate(pinp_type, zeroed))
            }
            Operation::ElementGet {
                array,
                indices,
                checked,
            } => self.emit_element_get(array, indices, checked, pinp_type),
            Operation::Slice { source, selectors } => self.emit_slice(source, selectors, pinp_type),
            Operation::ElementAddress { .. } => {
                unimplemented!("`ElementAddress` is not produced yet")
            }
            Operation::Load { .. } => unimplemented!("`Load` is not produced yet"),
            operation => unreachable!("{operation:?} is not a value"),
        }
    }

    /// Every word of every argument, in order.
    pub(super) fn emit_arguments(&mut self, args: List<NodeId>) -> Vec<Value> {
        let function = self.function;
        let mut values = Vec::new();
        for &arg in function.arena.items(args) {
            values.extend_from_slice(self.emit_value(arg).as_slice());
        }
        values
    }

    fn emit_constant(&mut self, constant: Constant) -> Words {
        let ins = self.builder.ins();
        match constant {
            Constant::Bool(value) => Words::one(ins.iconst(I8, value as i64)),
            Constant::Int(value) => Words::one(ins.iconst(I64, value)),
            Constant::Float(bits) => Words::one(ins.f64const(Ieee64::with_bits(bits))),
            Constant::Str(str_id) => self.emit_string_literal(str_id),
        }
    }

    fn emit_binary(&mut self, op: BinOp, lhs: NodeId, rhs: NodeId, result_type: PinpType) -> Words {
        let operand_type = self.type_of(lhs);
        if operand_type == PinpType::Str {
            return self.emit_string_comparison(op, lhs, rhs);
        }
        let left = self.emit_value(lhs).single();
        let right = self.emit_value(rhs).single();
        let float = operand_type == PinpType::Float;
        if op == BinOp::Pow {
            return Words::one(self.emit_power(left, right, result_type));
        }
        let ins = self.builder.ins();
        Words::one(match op {
            BinOp::Add if float => ins.fadd(left, right),
            BinOp::Sub if float => ins.fsub(left, right),
            BinOp::Mul if float => ins.fmul(left, right),
            BinOp::Add => ins.iadd(left, right),
            BinOp::Sub => ins.isub(left, right),
            BinOp::Mul => ins.imul(left, right),
            BinOp::Div => ins.fdiv(left, right),
            BinOp::Xor => ins.bxor(left, right),
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge if float => {
                ins.fcmp(float_condition(op), left, right)
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                ins.icmp(int_condition(op), left, right)
            }
            BinOp::Pow | BinOp::And | BinOp::Or | BinOp::IntDiv | BinOp::Mod => {
                unreachable!("{op:?} is not a plain binary operation")
            }
        })
    }

    /// `left ^ right` through libm's `pow`. An `int` power converts saturating: past the range of
    /// `int` it is `i64::MAX` or `i64::MIN`.
    fn emit_power(&mut self, left: Value, right: Value, result_type: PinpType) -> Value {
        let integer = result_type == PinpType::Int;
        let (left, right) = match integer {
            true => (
                self.builder.ins().fcvt_from_sint(F64, left),
                self.builder.ins().fcvt_from_sint(F64, right),
            ),
            false => (left, right),
        };
        let power = self
            .call_runtime("pow", &[F64, F64], &[F64], &[left, right])
            .single();
        match integer {
            true => self.builder.ins().fcvt_to_sint_sat(I64, power),
            false => power,
        }
    }

    /// `div` or `mod`; while `checked`, a zero divisor and the one overflowing quotient,
    /// `i64::MIN div -1`, are runtime errors — for `mod` too, as in Rust.
    fn emit_integer_divide(
        &mut self,
        remainder: bool,
        checked: bool,
        lhs: NodeId,
        rhs: NodeId,
    ) -> Words {
        let left = self.emit_value(lhs).single();
        let right = self.emit_value(rhs).single();
        if checked {
            let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, right, 0);
            self.emit_check(zero, "Division by zero.");
            let minimum = self.builder.ins().icmp_imm_s(IntCC::Equal, left, i64::MIN);
            let minus_one = self.builder.ins().icmp_imm_s(IntCC::Equal, right, -1);
            let overflow = self.builder.ins().band(minimum, minus_one);
            self.emit_check(overflow, "Division overflow.");
        }
        let ins = self.builder.ins();
        Words::one(match remainder {
            true => ins.srem(left, right),
            false => ins.sdiv(left, right),
        })
    }

    /// An implicit promotion — `bool` to `int` or `float`, `int` to `float` — or a rendering as
    /// `str`.
    fn emit_convert(&mut self, operand: Words, from: PinpType, to: PinpType) -> Words {
        if to == PinpType::Str {
            return self.emit_to_string(operand, from);
        }
        let value = operand.single();
        let ins = self.builder.ins();
        Words::one(match (from, to) {
            (PinpType::Bool, PinpType::Int) => ins.uextend(I64, value),
            (PinpType::Bool, PinpType::Float) => {
                let widened = ins.uextend(I64, value);
                self.builder.ins().fcvt_from_sint(F64, widened)
            }
            (PinpType::Int, PinpType::Float) => ins.fcvt_from_sint(F64, value),
            _ => unreachable!("{from:?} does not convert to {to:?}"),
        })
    }

    /// A range value. Without a step, an exclusive range steps toward its kind's direction and an
    /// inclusive one toward its stop; an explicit step of zero is a runtime error.
    fn emit_range(
        &mut self,
        kind: RangeKind,
        start: NodeId,
        stop: NodeId,
        step: Option<NodeId>,
    ) -> Words {
        let start = self.emit_value(start).single();
        let stop = self.emit_value(stop).single();
        let step = match step {
            Some(step) => {
                let step = self.emit_value(step).single();
                let zero = self.builder.ins().icmp_imm_s(IntCC::Equal, step, 0);
                self.emit_check(zero, "Range step cannot be zero.");
                step
            }
            None => match kind {
                RangeKind::UpExclusive => self.builder.ins().iconst(I64, 1),
                RangeKind::DownExclusive => self.builder.ins().iconst(I64, -1),
                RangeKind::Inclusive => {
                    let upward =
                        self.builder
                            .ins()
                            .icmp(IntCC::SignedGreaterThanOrEqual, stop, start);
                    let one = self.builder.ins().iconst(I64, 1);
                    let minus_one = self.builder.ins().iconst(I64, -1);
                    self.builder.ins().select(upward, one, minus_one)
                }
            },
        };
        let inclusive = self
            .builder
            .ins()
            .iconst(I8, (kind == RangeKind::Inclusive) as i64);
        Words::new(&[start, stop, step, inclusive])
    }

    /// `value in range` as closed-form arithmetic: on a step from the start, not before the start,
    /// and not past the stop, each side taken in the step's direction.
    fn emit_membership(&mut self, value: NodeId, range: NodeId) -> Words {
        let value = self.emit_value(value).single();
        let range = self.emit_value(range);
        let [start, stop, step, inclusive] = range.values;
        let offset = self.builder.ins().isub(value, start);
        let remainder = self.builder.ins().srem(offset, step);
        let on_step = self.builder.ins().icmp_imm_s(IntCC::Equal, remainder, 0);
        let upward = self
            .builder
            .ins()
            .icmp_imm_s(IntCC::SignedGreaterThan, step, 0);
        let up_to_stop = self.compare_either(
            IntCC::SignedLessThanOrEqual,
            IntCC::SignedLessThan,
            inclusive,
            value,
            stop,
        );
        let down_to_stop = self.compare_either(
            IntCC::SignedGreaterThanOrEqual,
            IntCC::SignedGreaterThan,
            inclusive,
            value,
            stop,
        );
        let within_stop = self.builder.ins().select(upward, up_to_stop, down_to_stop);
        let from_start = self.compare_either(
            IntCC::SignedGreaterThanOrEqual,
            IntCC::SignedLessThanOrEqual,
            upward,
            value,
            start,
        );
        let member = self.builder.ins().band(on_step, within_stop);
        Words::one(self.builder.ins().band(member, from_start))
    }

    /// `left when_true right` where `choose` holds, `left otherwise right` elsewhere.
    fn compare_either(
        &mut self,
        when_true: IntCC,
        otherwise: IntCC,
        choose: Value,
        left: Value,
        right: Value,
    ) -> Value {
        let first = self.builder.ins().icmp(when_true, left, right);
        let second = self.builder.ins().icmp(otherwise, left, right);
        self.builder.ins().select(choose, first, second)
    }
}

/// Floats compare ordered: any comparison with NaN is false, `!=` included.
fn float_condition(op: BinOp) -> FloatCC {
    match op {
        BinOp::Eq => FloatCC::Equal,
        BinOp::Ne => FloatCC::OrderedNotEqual,
        BinOp::Lt => FloatCC::LessThan,
        BinOp::Gt => FloatCC::GreaterThan,
        BinOp::Le => FloatCC::LessThanOrEqual,
        _ => FloatCC::GreaterThanOrEqual,
    }
}

pub(super) fn int_condition(op: BinOp) -> IntCC {
    match op {
        BinOp::Eq => IntCC::Equal,
        BinOp::Ne => IntCC::NotEqual,
        BinOp::Lt => IntCC::SignedLessThan,
        BinOp::Gt => IntCC::SignedGreaterThan,
        BinOp::Le => IntCC::SignedLessThanOrEqual,
        _ => IntCC::SignedGreaterThanOrEqual,
    }
}
