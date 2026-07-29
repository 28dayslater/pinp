// SPDX-License-Identifier: MIT

//! String lowering: literals, f-string interpolation, concatenation, the six comparisons, `.len`,
//! and the `str(x)`/`meminfo()` built-ins.
//!
//! A string value in flight is a `PinpStr` — the runtime's 16-byte descriptor, carried through IR as
//! `{ i64, i64 }` (see [`CodeGen::str_type`]). Results come back from the runtime by value in that
//! shape; operands that the runtime only *reads* are passed by pointer, so a value being inspected
//! is first spilled to a stack slot.
//!
//! `str` is pinp's first type with deterministic freeing: heap storage is released the moment its
//! last use is over, with no garbage collector and no reference counts. The Ownership section below
//! states the rule the whole file follows. (Arrays are still never freed — future work.)

use super::*;

use inkwell::IntPredicate;
use inkwell::types::FunctionType;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};

use crate::parser::{BinOp, ExprId, FStrSegment, Node, Stmt, StrId, TopLevel};

/// One operand of a join, paired with whether the join is responsible for freeing it.
#[derive(Clone, Copy)]
struct StrPart<'ctx> {
    value: BasicValueEnum<'ctx>,
    owned: bool,
}

impl<'ctx> StrPart<'ctx> {
    /// A part the join must free once it has copied the content out.
    fn owned(value: BasicValueEnum<'ctx>) -> Self {
        StrPart { value, owned: true }
    }

    /// A part belonging to someone else — a binding's own descriptor — which the join only reads.
    fn borrowed(value: BasicValueEnum<'ctx>) -> Self {
        StrPart {
            value,
            owned: false,
        }
    }
}

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

    /// `pinp_str_free(*mut PinpStr)` — releases heap storage and resets the descriptor to empty, so
    /// freeing a slot twice is harmless and an inline string costs nothing.
    fn declare_str_free(&self) -> FunctionValue<'ctx> {
        self.declare_runtime_function("pinp_str_free", || {
            self.context
                .void_type()
                .fn_type(&[self.ptr_type().into()], false)
        })
    }

    /// Writes a `PinpStr` value to a stack slot so it can be passed to the runtime by pointer. The
    /// slot is allocated in the entry block, so a spill inside a loop body does not grow the stack.
    fn spill_str(&self, value: BasicValueEnum<'ctx>) -> Result<PointerValue<'ctx>, String> {
        let slot = self.alloca_at_entry(PinpType::Str)?;
        self.builder.build_store(slot, value).map_err(err)?;
        Ok(slot)
    }

    // -------------------------------------------------------------------------
    // Ownership
    // -------------------------------------------------------------------------
    //
    // Every string value in flight is either *owned* — freshly produced, and the consumer's job to
    // release — or *borrowed*, which means it is a binding's own descriptor, read out of its slot.
    // The two are told apart structurally: only reading a variable borrows. Where a consumer needs
    // ownership but holds a borrow it takes a copy, since without reference counting two owners of
    // one heap buffer would free it twice.

    /// True when lowering `expr_id` yields a string the consumer must release. Reading a variable
    /// hands back the binding's own descriptor and so borrows it; everything else builds a new one.
    pub(super) fn owns_str_result(&self, expr_id: ExprId) -> bool {
        !matches!(self.ast.node(expr_id), Node::Var(_) | Node::Global(_))
    }

    /// Copies a string so the result is owned outright. A one-part join is exactly that copy — and
    /// it needs no extra runtime entry point.
    pub(super) fn copy_str(
        &self,
        value: BasicValueEnum<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        self.build_concat(&[StrPart::borrowed(value)])
    }

    /// Emits `expr_id` as a string the caller owns: as-is when it already produces one, otherwise a
    /// copy of the borrowed binding.
    pub(super) fn gen_owned_str(
        &mut self,
        expr_id: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let value = self.expect_value(expr_id)?;
        self.own_str(value, self.owns_str_result(expr_id))
    }

    /// Turns an already-evaluated string value into an owned one, copying only if it is borrowed.
    pub(super) fn own_str(
        &self,
        value: BasicValueEnum<'ctx>,
        owned: bool,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if owned {
            Ok(value)
        } else {
            self.copy_str(value)
        }
    }

    /// Releases the storage behind a string value. The value has to be spilled first — the runtime
    /// frees through a pointer, so that it can reset the descriptor it just released.
    pub(super) fn free_str_value(&self, value: BasicValueEnum<'ctx>) -> Result<(), String> {
        let slot = self.spill_str(value)?;
        self.free_str_slot(slot)
    }

    /// Releases the string a slot holds and leaves the slot holding the empty string.
    pub(super) fn free_str_slot(&self, slot: PointerValue<'ctx>) -> Result<(), String> {
        self.builder
            .build_call(self.declare_str_free(), &[slot.into()], "")
            .map_err(err)?;
        Ok(())
    }

    /// Frees every string bound in the innermost scope frame — emitted where that scope ends: a
    /// block's close, one turn of a loop body, or a function's return.
    ///
    /// Slots are visited in symbol order so the emitted IR does not depend on hash iteration order.
    pub(super) fn free_scope_strings(&self) -> Result<(), String> {
        let mut bound: Vec<(SymId, PointerValue<'ctx>)> = self
            .locals
            .last()
            .expect("a scope frame")
            .iter()
            .filter(|(_, (_, slot_type))| *slot_type == PinpType::Str)
            .map(|(sym_id, (slot, _))| (*sym_id, *slot))
            .collect();
        bound.sort_by_key(|(sym_id, _)| sym_id.value());
        for (_, slot) in bound {
            self.free_str_slot(slot)?;
        }
        Ok(())
    }

    /// Whether the value a statement yields is owned by whoever receives it. An expression
    /// statement passes its expression's ownership along; an assignment's value has just been
    /// handed to a binding's slot, so what it yields is that binding's — a borrow.
    pub(super) fn stmt_result_owns_str(&self, item: Option<&TopLevel>) -> bool {
        match item {
            Some(TopLevel::Stmt(Stmt::Expr(expr_id))) => self.owns_str_result(*expr_id),
            _ => false,
        }
    }

    /// Frees the string a statement produced when nothing receives it — an expression evaluated
    /// purely for its effect, in the middle of a block.
    pub(super) fn free_discarded_str(
        &mut self,
        stmt: &Stmt,
        value: Option<BasicValueEnum<'ctx>>,
    ) -> Result<(), String> {
        // An assignment's value lives on in the slot it was stored into; only a bare expression's
        // value is genuinely dropped here.
        let Stmt::Expr(expr_id) = stmt else {
            return Ok(());
        };
        if self.ast.type_of(*expr_id) == PinpType::Str && self.owns_str_result(*expr_id) {
            self.free_str_value(value.expect("a string expression yields a value"))?;
        }
        Ok(())
    }

    /// Frees every string held in a module global — emitted at the end of the entry function, which
    /// is where a global's lifetime ends.
    pub(super) fn free_global_strings(&self) -> Result<(), String> {
        let mut bound: Vec<(SymId, PointerValue<'ctx>)> = self
            .globals
            .iter()
            .filter(|(_, (_, slot_type))| *slot_type == PinpType::Str)
            .map(|(sym_id, (slot, _))| (*sym_id, *slot))
            .collect();
        bound.sort_by_key(|(sym_id, _)| sym_id.value());
        for (_, slot) in bound {
            self.free_str_slot(slot)?;
        }
        Ok(())
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
    /// A value that is *already* a string is handed back as-is rather than copied, so the result
    /// carries its operand's ownership: callers pair it with a [`StrPart`] built accordingly.
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

    /// Lowers the `str(x)` built-in. Converting a scalar produces a fresh string; converting a
    /// string yields an owned one too, so that a call result is owned whatever was passed in.
    pub(super) fn gen_str_conversion(
        &mut self,
        arg: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        if self.ast.type_of(arg) == PinpType::Str {
            return self.gen_owned_str(arg);
        }
        let value = self.expect_value(arg)?;
        self.str_from_value(value, self.ast.type_of(arg))
    }

    /// Joins already-evaluated parts with a single `pinp_str_concat_n` — one allocation for the
    /// whole chain rather than one per pair. The parts are gathered into a stack array, which is
    /// what the runtime's by-pointer signature expects.
    ///
    /// The join copies out of every part, so each owned part is dead as soon as it returns and is
    /// freed here. Their array slots double as the spill slots that free needs.
    fn build_concat(&self, parts: &[StrPart<'ctx>]) -> Result<BasicValueEnum<'ctx>, String> {
        let array_type = self.str_type().array_type(parts.len() as u32);
        let base = self.alloca_at_entry_type(array_type)?;
        let mut slots = Vec::with_capacity(parts.len());
        for (index, part) in parts.iter().enumerate() {
            let offset = self.int_type().const_int(index as u64, false);
            // SAFETY (LLVM's, not Rust's): the index is below the array's compile-time length.
            let slot = unsafe {
                self.builder
                    .build_gep(self.str_type(), base, &[offset], "part")
                    .map_err(err)?
            };
            self.builder.build_store(slot, part.value).map_err(err)?;
            slots.push(slot);
        }
        let count = self.int_type().const_int(parts.len() as u64, false);
        let joined =
            self.call_for_str(self.declare_str_concat_n(), &[base.into(), count.into()])?;
        for (part, slot) in parts.iter().zip(slots) {
            if part.owned {
                self.free_str_slot(slot)?;
            }
        }
        Ok(joined)
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
            let part_type = self.ast.type_of(part);
            let value = self.expect_value(part)?;
            // A scalar operand is converted, and the conversion is a fresh string this join owns; a
            // string operand is owned only if the expression that produced it was.
            let owned = part_type != PinpType::Str || self.owns_str_result(part);
            let value = self.str_from_value(value, part_type)?;
            values.push(StrPart { value, owned });
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
                    StrPart::owned(self.build_str_constant(self.ast.string_literal(*str_id))?)
                }
                FStrSegment::Interp(place) => {
                    let (value, value_type) = self.load_place(*place)?;
                    let rendered = self.str_from_value(value, value_type)?;
                    // A hole naming a string interpolates the binding's own descriptor; any other
                    // type was rendered into a fresh string this join owns.
                    match value_type {
                        PinpType::Str => StrPart::borrowed(rendered),
                        _ => StrPart::owned(rendered),
                    }
                }
            };
            parts.push(part);
        }
        // A lone `{s}` hole still goes through the join, which is what copies the borrowed binding
        // into a result the caller can own.
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
        let length = basic_value(call.try_as_basic_value());
        // Measuring a temporary is the last thing anyone does with it.
        if self.owns_str_result(object_id) {
            self.free_str_slot(object)?;
        }
        Ok(length)
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
        let relation = self
            .builder
            .build_int_compare(
                predicate,
                answer,
                self.context.i32_type().const_zero(),
                "rel",
            )
            .map_err(err)?;
        // The comparison read both operands and nothing else will.
        for (operand, slot) in [(lhs, left), (rhs, right)] {
            if self.owns_str_result(operand) {
                self.free_str_slot(slot)?;
            }
        }
        Ok(relation.into())
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
