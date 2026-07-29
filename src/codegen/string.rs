// SPDX-License-Identifier: MIT

//! String lowering: literals, f-string interpolation, concatenation, the six comparisons, `.len`,
//! and the `str(x)`/`meminfo()` built-ins.
//!
//! A string value in flight is a `PinpStr` — the runtime's 16-byte descriptor, carried through IR as
//! `{ i64, i64 }` (see [`CodeGen::str_type`]). Results come back from the runtime by value in that
//! shape; operands that the runtime only *reads* are passed by pointer, so a value being inspected
//! is first spilled to a stack slot.
//!
//! Nothing here frees anything. A produced `PinpStr` that owns heap storage stays alive for the rest
//! of the run, exactly as an array's allocation does today; deterministic freeing is the next step.

use super::*;

use inkwell::IntPredicate;
use inkwell::types::FunctionType;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};

use crate::parser::{BinOp, ExprId, FStrSegment, Node, StrId};

impl<'ctx, 'ast> CodeGen<'ctx, 'ast> {
    // -------------------------------------------------------------------------
    // The runtime surface
    // -------------------------------------------------------------------------

    /// Declares a runtime entry point on first use, then hands back the same declaration. The JIT
    /// resolves these against the process's own symbols, the same path that finds `pinp_alloc`.
    fn declare_runtime_function(
        &self,
        name: &str,
        signature: impl FnOnce() -> FunctionType<'ctx>,
    ) -> FunctionValue<'ctx> {
        match self.module.get_function(name) {
            Some(declared) => declared,
            None => self.module.add_function(name, signature(), None),
        }
    }

    /// `pinp_str_from_cstr(ptr) -> PinpStr` — builds a string from a NUL-terminated constant.
    fn declare_str_from_cstr(&self) -> FunctionValue<'ctx> {
        self.declare_runtime_function("pinp_str_from_cstr", || {
            self.str_type().fn_type(&[self.ptr_type().into()], false)
        })
    }

    /// `pinp_str_concat_n(*const PinpStr, count) -> PinpStr` — the whole chain in one allocation.
    fn declare_str_concat_n(&self) -> FunctionValue<'ctx> {
        self.declare_runtime_function("pinp_str_concat_n", || {
            self.str_type()
                .fn_type(&[self.ptr_type().into(), self.int_type().into()], false)
        })
    }

    /// `pinp_str_len(*const PinpStr) -> i64` — the byte length.
    fn declare_str_len(&self) -> FunctionValue<'ctx> {
        self.declare_runtime_function("pinp_str_len", || {
            self.int_type().fn_type(&[self.ptr_type().into()], false)
        })
    }

    /// `pinp_str_eq(*const PinpStr, *const PinpStr) -> i32` — non-zero when equal.
    fn declare_str_eq(&self) -> FunctionValue<'ctx> {
        self.declare_runtime_function("pinp_str_eq", || {
            self.context
                .i32_type()
                .fn_type(&[self.ptr_type().into(), self.ptr_type().into()], false)
        })
    }

    /// `pinp_str_cmp(*const PinpStr, *const PinpStr) -> i32` — `memcmp`-style three-way.
    fn declare_str_cmp(&self) -> FunctionValue<'ctx> {
        self.declare_runtime_function("pinp_str_cmp", || {
            self.context
                .i32_type()
                .fn_type(&[self.ptr_type().into(), self.ptr_type().into()], false)
        })
    }

    /// `pinp_str_from_int(i64) -> PinpStr`.
    fn declare_str_from_int(&self) -> FunctionValue<'ctx> {
        self.declare_runtime_function("pinp_str_from_int", || {
            self.str_type().fn_type(&[self.int_type().into()], false)
        })
    }

    /// `pinp_str_from_float(f64) -> PinpStr`.
    fn declare_str_from_float(&self) -> FunctionValue<'ctx> {
        self.declare_runtime_function("pinp_str_from_float", || {
            self.str_type().fn_type(&[self.float_type().into()], false)
        })
    }

    /// Calls a runtime entry point that yields a `PinpStr` by value.
    fn call_for_str(
        &self,
        function: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let call = self
            .builder
            .build_call(function, args, "str")
            .map_err(err)?;
        Ok(basic_value(call.try_as_basic_value()))
    }

    /// Writes a `PinpStr` value to a stack slot so it can be passed to the runtime by pointer. The
    /// slot is allocated in the entry block, so a spill inside a loop body does not grow the stack.
    fn spill_str(&self, value: BasicValueEnum<'ctx>) -> Result<PointerValue<'ctx>, String> {
        let slot = self.alloca_at_entry(PinpType::Str)?;
        self.builder.build_store(slot, value).map_err(err)?;
        Ok(slot)
    }

    // -------------------------------------------------------------------------
    // Producing string values
    // -------------------------------------------------------------------------

    /// Builds a `PinpStr` from fixed text: a private NUL-terminated module constant handed to
    /// `pinp_str_from_cstr`.
    fn build_str_constant(&self, content: &str) -> Result<BasicValueEnum<'ctx>, String> {
        let text = self
            .builder
            .build_global_string_ptr(content, "str_lit")
            .map_err(err)?
            .as_pointer_value();
        self.call_for_str(self.declare_str_from_cstr(), &[text.into()])
    }

    /// Lowers a `str` literal.
    pub(super) fn gen_str_literal(&self, str_id: StrId) -> Result<BasicValueEnum<'ctx>, String> {
        self.build_str_constant(self.ast.string_literal(str_id))
    }

    /// Renders an already-evaluated value as a `PinpStr` — the `str(...)` conversion, applied both
    /// explicitly and implicitly (a scalar concatenated onto a string, or interpolated into an
    /// f-string).
    ///
    /// A value that is *already* a string is handed back as-is, so the result aliases it rather than
    /// copying. That is invisible while nothing is freed; the freeing step has to treat such a
    /// result as borrowed, not owned.
    fn str_from_value(
        &self,
        value: BasicValueEnum<'ctx>,
        value_type: PinpType,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        match value_type {
            PinpType::Str => Ok(value),
            PinpType::Int => self.call_for_str(self.declare_str_from_int(), &[value.into()]),
            PinpType::Float => self.call_for_str(self.declare_str_from_float(), &[value.into()]),
            // A bool has only two renderings, so it needs no runtime formatter — just a choice
            // between two constants.
            PinpType::Bool => {
                let true_text = self
                    .builder
                    .build_global_string_ptr("true", "true_lit")
                    .map_err(err)?
                    .as_pointer_value();
                let false_text = self
                    .builder
                    .build_global_string_ptr("false", "false_lit")
                    .map_err(err)?
                    .as_pointer_value();
                let text = self
                    .builder
                    .build_select(value.into_int_value(), true_text, false_text, "bool_text")
                    .map_err(err)?;
                self.call_for_str(self.declare_str_from_cstr(), &[text.into()])
            }
            other => unreachable!("Sema rejects `{other:?}` where a string is expected"),
        }
    }

    /// Lowers the `str(x)` built-in.
    pub(super) fn gen_str_conversion(
        &mut self,
        arg: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let value = self.expect_value(arg)?;
        self.str_from_value(value, self.ast.type_of(arg))
    }

    /// Joins already-evaluated parts with a single `pinp_str_concat_n` — one allocation for the
    /// whole chain rather than one per pair. The parts are gathered into a stack array, which is
    /// what the runtime's by-pointer signature expects.
    fn build_concat(&self, parts: &[BasicValueEnum<'ctx>]) -> Result<BasicValueEnum<'ctx>, String> {
        let array_type = self.str_type().array_type(parts.len() as u32);
        let base = self.alloca_at_entry_type(array_type)?;
        for (index, part) in parts.iter().enumerate() {
            let offset = self.int_type().const_int(index as u64, false);
            // SAFETY (LLVM's, not Rust's): the index is below the array's compile-time length.
            let slot = unsafe {
                self.builder
                    .build_gep(self.str_type(), base, &[offset], "part")
                    .map_err(err)?
            };
            self.builder.build_store(slot, *part).map_err(err)?;
        }
        let count = self.int_type().const_int(parts.len() as u64, false);
        self.call_for_str(self.declare_str_concat_n(), &[base.into(), count.into()])
    }

    /// Lowers a `str` concatenation. The whole left spine of `+` is flattened first, so
    /// `a + b + c + d` becomes one N-way join instead of three pairwise ones.
    pub(super) fn gen_str_concat(
        &mut self,
        expr_id: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut parts = Vec::new();
        self.collect_str_parts(expr_id, &mut parts);
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            let value = self.expect_value(part)?;
            values.push(self.str_from_value(value, self.ast.type_of(part))?);
        }
        self.build_concat(&values)
    }

    /// Collects the operands of a chain of string `+` in evaluation order. Only the left spine is
    /// walked: a parenthesised concatenation on the right is one part, lowered by its own join.
    fn collect_str_parts(&self, expr_id: ExprId, parts: &mut Vec<ExprId>) {
        if self.ast.type_of(expr_id) == PinpType::Str
            && let Node::Bin {
                op: BinOp::Add,
                lhs,
                rhs,
            } = self.ast.node(expr_id)
        {
            self.collect_str_parts(*lhs, parts);
            parts.push(*rhs);
        } else {
            parts.push(expr_id);
        }
    }

    /// Lowers an f-string: each segment becomes a `PinpStr`, then one join produces the result.
    pub(super) fn gen_fstring(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        let Node::FStr { segments } = self.ast.node(expr_id) else {
            unreachable!("gen_fstring on a non-FStr node")
        };
        let segments: &'ast [FStrSegment] = segments;

        // An f-string of nothing but fixed text is just that text; `f''` has no segments at all.
        match segments {
            [] => return self.build_str_constant(""),
            [FStrSegment::Literal(str_id)] => return self.gen_str_literal(*str_id),
            _ => {}
        }

        let mut parts = Vec::with_capacity(segments.len());
        for segment in segments {
            let part = match segment {
                FStrSegment::Literal(str_id) => {
                    self.build_str_constant(self.ast.string_literal(*str_id))?
                }
                FStrSegment::Interp(place) => {
                    let (value, value_type) = self.load_place(*place)?;
                    self.str_from_value(value, value_type)?
                }
            };
            parts.push(part);
        }
        // A lone `{s}` hole still goes through the join: it copies, where handing the binding's own
        // descriptor back would alias it (see `str_from_value`).
        self.build_concat(&parts)
    }

    // -------------------------------------------------------------------------
    // Reading string values
    // -------------------------------------------------------------------------

    /// Lowers `.len` on a string to `pinp_str_len`, the one built-in member that is not a
    /// compile-time constant — an array knows its length from its type, a string only at run time.
    pub(super) fn gen_str_len(
        &mut self,
        object_id: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let value = self.expect_value(object_id)?;
        let object = self.spill_str(value)?;
        let call = self
            .builder
            .build_call(self.declare_str_len(), &[object.into()], "str_len")
            .map_err(err)?;
        Ok(basic_value(call.try_as_basic_value()))
    }

    /// Lowers a comparison whose operands are strings (sema has already required both to be). The
    /// runtime answers with an `i32` — a truth value for `==`/`!=`, a three-way sign for the
    /// orderings — which the same integer compare against zero turns into the `i1` pinp wants.
    pub(super) fn gen_str_compare(
        &mut self,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let left_value = self.expect_value(lhs)?;
        let right_value = self.expect_value(rhs)?;
        let left = self.spill_str(left_value)?;
        let right = self.spill_str(right_value)?;

        let (function, predicate) = match op {
            // `pinp_str_eq` reports equality directly, so `==` is "answer is not zero".
            BinOp::Eq => (self.declare_str_eq(), IntPredicate::NE),
            BinOp::Ne => (self.declare_str_eq(), IntPredicate::EQ),
            BinOp::Lt => (self.declare_str_cmp(), IntPredicate::SLT),
            BinOp::Gt => (self.declare_str_cmp(), IntPredicate::SGT),
            BinOp::Le => (self.declare_str_cmp(), IntPredicate::SLE),
            BinOp::Ge => (self.declare_str_cmp(), IntPredicate::SGE),
            other => unreachable!("`{other:?}` is not a string comparison"),
        };
        let call = self
            .builder
            .build_call(function, &[left.into(), right.into()], "str_rel")
            .map_err(err)?;
        let answer = basic_value(call.try_as_basic_value()).into_int_value();
        Ok(self
            .builder
            .build_int_compare(
                predicate,
                answer,
                self.context.i32_type().const_zero(),
                "rel",
            )
            .map_err(err)?
            .into())
    }

    /// Lowers the `meminfo()` diagnostic: the runtime prints mimalloc's statistics to stderr, and
    /// the call itself evaluates to nothing.
    pub(super) fn gen_meminfo(&self) -> Result<(), String> {
        let meminfo = self.declare_runtime_function("pinp_meminfo", || {
            self.context.void_type().fn_type(&[], false)
        });
        self.builder.build_call(meminfo, &[], "").map_err(err)?;
        Ok(())
    }
}
