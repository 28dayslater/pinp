// SPDX-License-Identifier: MIT

//! Strings and the other runtime calls. A string is its 16-byte `PinpStr` descriptor, held as two
//! words and passed by value; the runtime takes descriptors by address, so they are first stored
//! in a stack slot. Strings are never freed on this backend yet.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::I32;
use cranelift_codegen::ir::{StackSlotData, StackSlotKind};

use super::value::int_condition;
use super::*;
use crate::ir::{List, RuntimeFunction};
use crate::parser::{BinOp, StrId};

/// Bytes in a `PinpStr`.
const DESCRIPTOR: u32 = 16;

impl FunctionEmitter<'_, '_> {
    pub(super) fn emit_string_literal(&mut self, str_id: StrId) -> Words {
        let content = self.program.string_literals[str_id.value()].as_bytes();
        let text = self.text_address(content);
        self.string_from_text(text)
    }

    /// A new string copied from the NUL-terminated text at `text`.
    fn string_from_text(&mut self, text: Value) -> Words {
        self.call_runtime("pinp_str_from_cstr", &[I64], &[I64, I64], &[text])
    }

    /// `operand`, of type `from`, rendered as a string; a string is left as it is.
    pub(super) fn emit_to_string(&mut self, operand: Words, from: PinpType) -> Words {
        match from {
            PinpType::Str => operand,
            PinpType::Int => {
                self.call_runtime("pinp_str_from_int", &[I64], &[I64, I64], operand.as_slice())
            }
            PinpType::Float => self.call_runtime(
                "pinp_str_from_float",
                &[F64],
                &[I64, I64],
                operand.as_slice(),
            ),
            PinpType::Bool => {
                let true_text = self.text_address(b"true");
                let false_text = self.text_address(b"false");
                let text = self
                    .builder
                    .ins()
                    .select(operand.single(), true_text, false_text);
                self.string_from_text(text)
            }
            other => unreachable!("{other:?} does not render as a string"),
        }
    }

    pub(super) fn emit_runtime_call(
        &mut self,
        function: RuntimeFunction,
        args: List<NodeId>,
    ) -> Words {
        let args = self.emit_arguments(args);
        match function {
            RuntimeFunction::StrConcat => {
                let count = args.len() / 2;
                let parts = match count {
                    0 => self.builder.ins().iconst(I64, 0),
                    _ => self.spill(&args),
                };
                let count = self.builder.ins().iconst(I64, count as i64);
                self.call_runtime(
                    "pinp_str_concat_n",
                    &[I64, I64],
                    &[I64, I64],
                    &[parts, count],
                )
            }
            RuntimeFunction::StrCopy => {
                let string = self.spill(&args);
                let one = self.builder.ins().iconst(I64, 1);
                self.call_runtime(
                    "pinp_str_concat_n",
                    &[I64, I64],
                    &[I64, I64],
                    &[string, one],
                )
            }
            RuntimeFunction::StrFree => {
                let string = self.spill(&args);
                self.call_runtime("pinp_str_free", &[I64], &[], &[string])
            }
            RuntimeFunction::StrLength => {
                let string = self.spill(&args);
                self.call_runtime("pinp_str_len", &[I64], &[I64], &[string])
            }
            RuntimeFunction::MemInfo => self.call_runtime("pinp_meminfo", &[], &[], &[]),
        }
    }

    /// A string comparison through the runtime: `==` and `!=` ask whether the contents are equal,
    /// the orderings compare a three-way answer with zero.
    pub(super) fn emit_string_comparison(&mut self, op: BinOp, lhs: NodeId, rhs: NodeId) -> Words {
        let left = self.emit_value(lhs);
        let right = self.emit_value(rhs);
        let left = self.spill(left.as_slice());
        let right = self.spill(right.as_slice());
        let (name, condition) = match op {
            BinOp::Eq => ("pinp_str_eq", IntCC::NotEqual),
            BinOp::Ne => ("pinp_str_eq", IntCC::Equal),
            _ => ("pinp_str_cmp", int_condition(op)),
        };
        let answer = self
            .call_runtime(name, &[I64, I64], &[I32], &[left, right])
            .single();
        Words::one(self.builder.ins().icmp_imm_s(condition, answer, 0))
    }

    /// Stores descriptors, two words each, side by side in a new stack slot; returns its address.
    fn spill(&mut self, descriptor_words: &[Value]) -> Value {
        let size = DESCRIPTOR * (descriptor_words.len() as u32 / 2);
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size,
            3,
        ));
        for (index, &word) in descriptor_words.iter().enumerate() {
            self.builder
                .ins()
                .stack_store(I64, word, slot, (index * 8) as i32);
        }
        self.builder.ins().stack_addr(I64, slot, 0)
    }
}
