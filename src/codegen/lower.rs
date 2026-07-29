// SPDX-License-Identifier: MIT

//! AST → LLVM IR lowering: the heart of the codegen backend.
//!
//! *Lowering* is the compiler term for rewriting a program into a lower-level form — here the typed
//! [`ProgramAst`] into LLVM IR, one step "down" toward the machine. Sema has already resolved names
//! and inferred every type, so lowering carries no analysis: it is a mechanical walk that emits, for
//! each construct, the IR that computes it. LLVM then optimises that IR and machine-codes it.
//!
//! This file holds the `CodeGen` state and its construction, the small scope and LLVM-type helpers
//! the rest of the backend builds on, and the top-level driver (`generate`) that emits the module's
//! globals, functions, and entry point. The per-node emitters live beside it in `stmt.rs`
//! (statements and control flow) and `expr.rs` (expressions and value coercions).

use super::*;

use inkwell::module::Linkage;
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FloatType, IntType, StructType,
};
use inkwell::values::IntValue;

use crate::parser::{Stmt, TopLevel};

impl<'ctx, 'ast> CodeGen<'ctx, 'ast> {
    // -------------------------------------------------------------------------
    // Construction and scopes
    // -------------------------------------------------------------------------

    /// Creates an empty code generator lowering `ast` into a module in `context`.
    pub(super) fn new(context: &'ctx Context, ast: &'ast ProgramAst<'ast>) -> Self {
        CodeGen {
            context,
            module: context.create_module("pinp"),
            builder: context.create_builder(),
            ast,
            functions: FxHashMap::default(),
            globals: FxHashMap::default(),
            locals: Vec::new(),
            fn_base: 0,
            in_function: false,
        }
    }

    /// Push a fresh local scope frame.
    pub(super) fn push_scope(&mut self) {
        self.locals.push(FxHashMap::default());
    }

    /// Pop the innermost local scope frame.
    pub(super) fn pop_scope(&mut self) {
        self.locals.pop();
    }

    /// The function currently being emitted into — the parent of the active basic block. Used to
    /// append the new blocks that `if`/`while`/`loop` need.
    pub(super) fn current_function(&self) -> FunctionValue<'ctx> {
        self.builder
            .get_insert_block()
            .expect("an active block")
            .get_parent()
            .expect("a block has a parent function")
    }

    /// The slot+type of the nearest enclosing local binding of `sym`, searched innermost-first down
    /// to the current function base (never the global frame).
    pub(super) fn find_local(&self, sym: SymId) -> Option<(PointerValue<'ctx>, PinpType)> {
        self.locals[self.fn_base..]
            .iter()
            .rev()
            .find_map(|frame| frame.get(&sym).copied())
    }

    /// The slot+type a name resolves to: `::name` is always a global, while a bare name is the
    /// nearest enclosing local, falling back to a global at the top level (where top-level bindings
    /// *are* module globals).
    pub(super) fn var_slot(&self, sym_id: SymId, global: bool) -> (PointerValue<'ctx>, PinpType) {
        if global {
            self.globals[&sym_id]
        } else if let Some(slot) = self.find_local(sym_id) {
            slot
        } else {
            self.globals[&sym_id]
        }
    }

    /// Allocates a slot in the current function's entry block (not at the live insert point), so the
    /// alloca dominates every use and is not re-run — growing the stack — inside a loop body.
    pub(super) fn alloca_at_entry(
        &self,
        pinp_type: PinpType,
    ) -> Result<PointerValue<'ctx>, String> {
        let slot = self.alloca_at_entry_type(self.basic_type(pinp_type))?;
        // A string slot is freed before it is overwritten and again when its scope ends, so it must
        // start out as a valid descriptor rather than whatever the stack held. All-zero is the empty
        // inline string, which frees to nothing.
        if pinp_type == PinpType::Str {
            self.store_at_entry(slot, self.zero(PinpType::Str))?;
        }
        Ok(slot)
    }

    /// Stores an initial value into a slot right where the slot was allocated, so the store runs
    /// once on entry rather than every time execution reaches the code that uses it.
    pub(super) fn store_at_entry(
        &self,
        slot: PointerValue<'ctx>,
        value: BasicValueEnum<'ctx>,
    ) -> Result<(), String> {
        let current = self.builder.get_insert_block().expect("an active block");
        let alloca = slot
            .as_instruction()
            .expect("a slot is an alloca instruction");
        match alloca.get_next_instruction() {
            Some(next) => self.builder.position_before(&next),
            None => self.builder.position_at_end(
                alloca
                    .get_parent()
                    .expect("an instruction belongs to a block"),
            ),
        }
        self.builder.build_store(slot, value).map_err(err)?;
        self.builder.position_at_end(current);
        Ok(())
    }

    /// [`alloca_at_entry`](Self::alloca_at_entry) for an LLVM type with no pinp type of its own —
    /// the array of `PinpStr`s a concatenation gathers its parts into, for instance.
    pub(super) fn alloca_at_entry_type(
        &self,
        llvm_type: impl BasicType<'ctx>,
    ) -> Result<PointerValue<'ctx>, String> {
        let current = self.builder.get_insert_block().expect("an active block");
        let entry = current
            .get_parent()
            .expect("a block has a parent function")
            .get_first_basic_block()
            .expect("a function has an entry block");
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let slot = self.builder.build_alloca(llvm_type, "local").map_err(err)?;
        self.builder.position_at_end(current);
        Ok(slot)
    }

    // -------------------------------------------------------------------------
    // LLVM types
    // -------------------------------------------------------------------------

    /// The LLVM `i64` backing pinp `Int`.
    pub(super) fn int_type(&self) -> IntType<'ctx> {
        self.context.i64_type()
    }

    /// The LLVM `f64` backing pinp `Float`.
    pub(super) fn float_type(&self) -> FloatType<'ctx> {
        self.context.f64_type()
    }

    /// The LLVM `i1` backing pinp `Bool`.
    pub(super) fn bool_type(&self) -> IntType<'ctx> {
        self.context.bool_type() // i1
    }

    /// The LLVM opaque pointer, used for every address the backend passes around.
    pub(super) fn ptr_type(&self) -> inkwell::types::PointerType<'ctx> {
        self.context.ptr_type(AddressSpace::default())
    }

    /// The LLVM type of a `PinpStr`: two INTEGER eightbytes, matching the runtime's 16-byte wire
    /// struct. The shape is load-bearing, not cosmetic — `{ i64, i64 }` is what the SysV ABI
    /// returns in `rax:rdx`, whereas the equally 16-byte `[16 x i8]` classifies as MEMORY and would
    /// be returned through a hidden pointer the runtime does not expect.
    pub(super) fn str_type(&self) -> StructType<'ctx> {
        let int = self.int_type().into();
        self.context.struct_type(&[int, int], false)
    }

    // A range value is the aggregate `{ start, stop, step, inclusive }`: three `i64`s and an `i1`.
    // The operator's direction/inclusivity is baked into these fields at construction, so iteration
    // and membership read a uniform shape.
    pub(super) fn range_type(&self) -> StructType<'ctx> {
        let int = self.int_type().into();
        let bool = self.bool_type().into();
        self.context.struct_type(&[int, int, int, bool], false)
    }

    /// The LLVM type backing a storable pinp value type.
    pub(super) fn basic_type(&self, pinp_type: PinpType) -> BasicTypeEnum<'ctx> {
        match pinp_type {
            PinpType::Bool => self.bool_type().into(),
            PinpType::Int => self.int_type().into(),
            PinpType::Float => self.float_type().into(),
            PinpType::Str => self.str_type().into(),
            PinpType::Void => unreachable!("Void is not a storable value type."),
            PinpType::Range => self.range_type().into(),
            // Array and matrix variables hold a heap pointer (opaque ptr, 64-bit on all targets).
            PinpType::Array(_, _) | PinpType::Matrix(_, _, _) => {
                self.context.ptr_type(AddressSpace::default()).into()
            }
        }
    }

    /// Builds a range value from its already-evaluated fields.
    pub(super) fn build_range(
        &self,
        start: IntValue<'ctx>,
        stop: IntValue<'ctx>,
        step: IntValue<'ctx>,
        inclusive: IntValue<'ctx>,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let mut range = self.range_type().get_undef();
        for (index, field) in [start, stop, step, inclusive].into_iter().enumerate() {
            range = self
                .builder
                .build_insert_value(range, field, index as u32, "range")
                .map_err(err)?
                .into_struct_value();
        }
        Ok(range.into())
    }

    /// Pulls the four fields `(start, stop, step, inclusive)` out of a range value.
    pub(super) fn extract_range(
        &self,
        range: BasicValueEnum<'ctx>,
    ) -> Result<
        (
            IntValue<'ctx>,
            IntValue<'ctx>,
            IntValue<'ctx>,
            IntValue<'ctx>,
        ),
        String,
    > {
        let range = range.into_struct_value();
        let field = |index| -> Result<IntValue<'ctx>, String> {
            Ok(self
                .builder
                .build_extract_value(range, index, "field")
                .map_err(err)?
                .into_int_value())
        };
        Ok((field(0)?, field(1)?, field(2)?, field(3)?))
    }

    // -------------------------------------------------------------------------
    // Top-level driver
    // -------------------------------------------------------------------------

    /// Emits the whole program and returns the type of its final expression.
    pub(super) fn generate(&mut self) -> Result<PinpType, String> {
        self.declare_globals();
        self.declare_functions();
        for item in &self.ast.top_level {
            if let TopLevel::Func(func) = item {
                self.gen_function(func)?;
            }
        }
        self.gen_entry()
    }

    /// Declares a module global for every symbol assigned at the top level.
    fn declare_globals(&mut self) {
        for item in &self.ast.top_level {
            if let TopLevel::Stmt(Stmt::Assign {
                target_lists,
                values,
            }) = item
            {
                for group in target_lists {
                    for (place, value) in group.iter().zip(values) {
                        let sym_id = place_sym(*place);
                        if self.globals.contains_key(&sym_id) {
                            continue;
                        }
                        let value_type = self.ast.type_of(*value);
                        let name = self.ast.names[sym_id.value()];
                        let global =
                            self.module
                                .add_global(self.basic_type(value_type), None, name);
                        global.set_linkage(Linkage::Internal);
                        global.set_initializer(&self.zero(value_type));
                        self.globals
                            .insert(sym_id, (global.as_pointer_value(), value_type));
                    }
                }
            }
        }
    }

    /// Declares the LLVM signature of every pinp function (so calls resolve
    /// regardless of textual order).
    fn declare_functions(&mut self) {
        for item in &self.ast.top_level {
            let TopLevel::Func(func) = item else { continue };
            let param_types: Vec<PinpType> =
                func.params.iter().map(|param| param.param_type).collect();
            let llvm_params: Vec<BasicMetadataTypeEnum> = param_types
                .iter()
                .map(|param_type| self.basic_type(*param_type).into())
                .collect();
            let fn_type = match func.return_type {
                PinpType::Void => self.context.void_type().fn_type(&llvm_params, false),
                value_type => self.basic_type(value_type).fn_type(&llvm_params, false),
            };
            let name = self.ast.names[func.name.value()];
            let function = self.module.add_function(name, fn_type, None);
            self.functions
                .insert(func.name, (function, param_types, func.return_type));
        }
    }

    /// Lowers one pinp function into its LLVM function body.
    fn gen_function(&mut self, func: &crate::parser::FuncDef) -> Result<(), String> {
        let (function, _, _) = self.functions[&func.name];
        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        self.locals.clear();
        self.locals.push(FxHashMap::default()); // function base frame
        self.fn_base = 0;
        self.in_function = true;
        // Give each parameter an alloca so reads/writes are uniform with locals.
        for (i, param) in func.params.iter().enumerate() {
            let slot = self
                .builder
                .build_alloca(self.basic_type(param.param_type), "param")
                .map_err(err)?;
            let incoming = function.get_nth_param(i as u32).expect("parameter count");
            self.builder.build_store(slot, incoming).map_err(err)?;
            self.locals[0].insert(param.name, (slot, param.param_type));
        }

        for stmt in &func.body.stmts {
            let value = self.gen_stmt(stmt)?;
            self.free_discarded_str(stmt, value)?;
        }

        let result = func
            .body
            .result
            .expect("a function body always ends in a result expression");
        if func.return_type == PinpType::Void {
            self.gen_expr(result)?; // side effects only
            self.free_scope_strings()?;
            self.builder.build_return(None).map_err(err)?;
        } else {
            // A returned string is moved out: it is owned before the frame's own strings are
            // released, so returning a local's value hands back a copy rather than a freed one.
            let value = match func.return_type {
                PinpType::Str => self.gen_owned_str(result)?,
                _ => self.expect_value(result)?,
            };
            let value = self.promote(value, self.ast.type_of(result), func.return_type);
            self.free_scope_strings()?;
            self.builder.build_return(Some(&value)).map_err(err)?;
        }
        self.in_function = false;
        Ok(())
    }

    /// Emits the `ENTRY` function from the top-level statements. The entry always has the uniform
    /// signature `void __pinp_main(ptr result_ptr)`: the caller (pinp_run) passes an 8-byte buffer
    /// for the scalar result; the entry writes into it when non-void, then returns void. This lets
    /// the C trampoline hold the setjmp frame and call any program with a single function pointer
    /// type, regardless of the program's result type.
    fn gen_entry(&mut self) -> Result<PinpType, String> {
        let result_type = match self.ast.top_level.last() {
            Some(TopLevel::Stmt(Stmt::Expr(e))) => self.ast.type_of(*e),
            Some(TopLevel::Stmt(Stmt::Assign { values, .. })) => {
                self.ast.type_of(*values.last().unwrap())
            }
            _ => PinpType::Void,
        };
        if result_type == PinpType::Range {
            return Err("A program cannot evaluate to a range.".into());
        }

        let ptr_type = self.context.ptr_type(AddressSpace::default());
        let fn_type = self.context.void_type().fn_type(&[ptr_type.into()], false);
        let entry_fn = self.module.add_function(ENTRY, fn_type, None);
        let block = self.context.append_basic_block(entry_fn, "entry");
        self.builder.position_at_end(block);
        self.locals.clear();
        self.locals.push(FxHashMap::default()); // entry base frame, for control-flow body-locals
        self.fn_base = 0;
        self.in_function = false;

        let last = self.ast.top_level.len().wrapping_sub(1);
        let mut result = None;
        for (i, item) in self.ast.top_level.iter().enumerate() {
            if let TopLevel::Stmt(stmt) = item {
                let value = self.gen_stmt(stmt)?;
                if i == last {
                    result = value;
                } else {
                    self.free_discarded_str(stmt, value)?;
                }
            }
        }

        if result_type != PinpType::Void {
            let result_value = result.expect("A non-void program must yield a value.");
            // The program's value is moved out to the host, which frees it — so it must be owned
            // before the top-level bindings and globals are released just below.
            let result_value = match result_type {
                PinpType::Str => {
                    let owned = self.stmt_result_owns_str(self.ast.top_level.last());
                    self.own_str(result_value, owned)?
                }
                _ => result_value,
            };
            let result_ptr = entry_fn
                .get_first_param()
                .expect("result pointer parameter")
                .into_pointer_value();
            self.builder
                .build_store(result_ptr, result_value)
                .map_err(err)?;
        }
        self.free_scope_strings()?;
        self.free_global_strings()?;
        self.builder.build_return(None).map_err(err)?;
        Ok(result_type)
    }

    /// Consumes the generator, yielding the finished LLVM module.
    pub(super) fn into_module(self) -> Module<'ctx> {
        self.module
    }
}
