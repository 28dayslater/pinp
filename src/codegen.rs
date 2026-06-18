// SPDX-License-Identifier: MIT

//! LLVM ORCv2 JIT backend (gated behind the `llvm` feature).
//!
//! [`Jit`] is a thin safe wrapper over the ORCv2 LLJIT C API (reached through
//! `inkwell::llvm_sys`, no extra dependency); inkwell 0.9 ships no ORC bindings.
//! [`CodeGen`] lowers a parsed [`Ast`] into an LLVM module, and [`PinpJit`] ties
//! the two together: source string in, executed, [`PinpValue`] out. This is the
//! harness pinp's runtime tests are written against.

use std::ffi::{CStr, CString};

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::llvm_sys::error::{LLVMDisposeErrorMessage, LLVMErrorRef, LLVMGetErrorMessage};
use inkwell::llvm_sys::orc2::lljit::{
    LLVMOrcCreateLLJIT, LLVMOrcDisposeLLJIT, LLVMOrcLLJITAddLLVMIRModule,
    LLVMOrcLLJITGetMainJITDylib, LLVMOrcLLJITLookup, LLVMOrcLLJITRef,
};
use inkwell::llvm_sys::orc2::{
    LLVMOrcCreateNewThreadSafeContext, LLVMOrcCreateNewThreadSafeModule,
    LLVMOrcDisposeThreadSafeContext, LLVMOrcExecutorAddress,
};
use inkwell::module::{Linkage, Module};
use inkwell::targets::{InitializationConfig, Target};
use inkwell::types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FloatType, IntType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue, ValueKind,
};
use rustc_hash::FxHashMap;

use crate::parser::{
    Ast, BinOp, ExprId, Node, PinpType, Place, Stmt, SymId, TopLevel, UnOp, parse,
};
use crate::sema::analyze;

/// Name of the synthetic entry function [`CodeGen`] emits for the top-level program.
const ENTRY: &str = "__pinp_main";

// ---------------------------------------------------------------------------
// ORCv2 JIT wrapper
// ---------------------------------------------------------------------------

/// A minimal ORCv2 LLJIT instance. Owns the underlying `LLVMOrcLLJITRef` and
/// disposes it on drop.
///
/// The inkwell [`Context`] a module was built in must outlive the `Jit` it was
/// added to: ORC frees the module on dispose, which must precede its context.
pub struct Jit {
    jit: LLVMOrcLLJITRef,
}

impl Jit {
    /// Creates a JIT for the host machine.
    pub fn new() -> Result<Self, String> {
        Target::initialize_native(&InitializationConfig::default())
            .map_err(|error| format!("Failed to initialize the native target: {error}."))?;

        let mut jit: LLVMOrcLLJITRef = std::ptr::null_mut();
        // SAFETY: a null builder asks ORC for a default LLJIT; on success it writes
        // the instance into `jit`, otherwise it returns a non-null error.
        unsafe { check_error(LLVMOrcCreateLLJIT(&mut jit, std::ptr::null_mut()))? };
        Ok(Jit { jit })
    }

    /// Adds an inkwell module to the JIT, transferring ownership of the module to
    /// ORC. The module's [`Context`] must outlive this `Jit` (see the type docs).
    pub fn add_module(&self, module: Module) -> Result<(), String> {
        let module_ref = module.as_mut_ptr();
        // ORC takes ownership of the raw module below, so release inkwell's RAII
        // handle without disposing it.
        std::mem::forget(module);

        // SAFETY: `module_ref` is a valid, now inkwell-unowned module. The throwaway
        // ThreadSafeContext only guards ORC's (single-threaded) locking; the module
        // keeps its real context. `add` consumes the ThreadSafeModule.
        unsafe {
            let thread_safe_context = LLVMOrcCreateNewThreadSafeContext();
            let thread_safe_module =
                LLVMOrcCreateNewThreadSafeModule(module_ref, thread_safe_context);
            LLVMOrcDisposeThreadSafeContext(thread_safe_context);

            let main_dylib = LLVMOrcLLJITGetMainJITDylib(self.jit);
            check_error(LLVMOrcLLJITAddLLVMIRModule(
                self.jit,
                main_dylib,
                thread_safe_module,
            ))
        }
    }

    /// Looks up a JITed symbol and returns it as the function-pointer type `FPT`.
    ///
    /// # Safety
    ///
    /// `FPT` must be a function-pointer type whose signature matches the symbol's
    /// actual ABI, or calling it is undefined behaviour.
    pub unsafe fn lookup<FPT: Copy>(&self, name: &str) -> Result<FPT, String> {
        let symbol = CString::new(name).map_err(|error| error.to_string())?;
        let mut address: LLVMOrcExecutorAddress = 0;
        // SAFETY: the caller guarantees `FPT` matches the symbol's ABI.
        unsafe {
            check_error(LLVMOrcLLJITLookup(self.jit, &mut address, symbol.as_ptr()))?;
            // `address` is the symbol's runtime address; reinterpret it as `FPT`.
            Ok(std::mem::transmute_copy::<LLVMOrcExecutorAddress, FPT>(
                &address,
            ))
        }
    }
}

impl Drop for Jit {
    fn drop(&mut self) {
        // SAFETY: `self.jit` was created in `new` and is disposed exactly once.
        unsafe {
            LLVMOrcDisposeLLJIT(self.jit);
        }
    }
}

/// Turns an `LLVMErrorRef` into a `Result`: null is success, otherwise the error's
/// message is extracted and the error consumed.
unsafe fn check_error(error: LLVMErrorRef) -> Result<(), String> {
    if error.is_null() {
        return Ok(());
    }
    // SAFETY: `error` is non-null here, so the caller's LLVM error pointer is live.
    unsafe {
        let message = LLVMGetErrorMessage(error); // consumes `error`, returns owned C string
        let owned = CStr::from_ptr(message).to_string_lossy().into_owned();
        LLVMDisposeErrorMessage(message);
        Err(owned)
    }
}

// ---------------------------------------------------------------------------
// AST -> LLVM IR
// ---------------------------------------------------------------------------

/// Lowers a parsed [`Ast`] into an LLVM module. Every pinp function becomes an
/// LLVM function, top-level globals become module globals, and the top-level
/// statements are emitted into an `ENTRY` function whose return value is the
/// program's final expression.
struct CodeGen<'ctx, 'ast> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    ast: &'ast Ast<'ast>,
    functions: FxHashMap<SymId, (FunctionValue<'ctx>, Vec<PinpType>, PinpType)>,
    globals: FxHashMap<SymId, (PointerValue<'ctx>, PinpType)>,
    // A stack of local scope frames mirroring sema: a function (or the entry) pushes a base frame,
    // and every control-flow body pushes another. Bare-name resolution searches `locals[fn_base..]`
    // outward, so an assignment mutates the nearest enclosing local while a name new to all frames
    // becomes a non-escaping body-local (its slot is still alloca'd in the entry block).
    locals: Vec<FxHashMap<SymId, (PointerValue<'ctx>, PinpType)>>,
    fn_base: usize,
    in_function: bool,
}

impl<'ctx, 'ast> CodeGen<'ctx, 'ast> {
    fn new(context: &'ctx Context, ast: &'ast Ast<'ast>) -> Self {
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

    fn push_scope(&mut self) {
        self.locals.push(FxHashMap::default());
    }

    fn pop_scope(&mut self) {
        self.locals.pop();
    }

    /// The function currently being emitted into — the parent of the active basic block. Used to
    /// append the new blocks that `if`/`while`/`loop` need.
    fn current_function(&self) -> FunctionValue<'ctx> {
        self.builder
            .get_insert_block()
            .expect("an active block")
            .get_parent()
            .expect("a block has a parent function")
    }

    /// The slot+type of the nearest enclosing local binding of `sym`, searched innermost-first down
    /// to the current function base (never the global frame).
    fn find_local(&self, sym: SymId) -> Option<(PointerValue<'ctx>, PinpType)> {
        self.locals[self.fn_base..]
            .iter()
            .rev()
            .find_map(|frame| frame.get(&sym).copied())
    }

    /// Allocates a slot in the current function's entry block (not at the live insert point), so the
    /// alloca dominates every use and is not re-run — growing the stack — inside a loop body.
    fn alloca_at_entry(&self, pinp_type: PinpType) -> Result<PointerValue<'ctx>, String> {
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
        let slot = self
            .builder
            .build_alloca(self.basic_type(pinp_type), "local")
            .map_err(err)?;
        self.builder.position_at_end(current);
        Ok(slot)
    }

    fn int_type(&self) -> IntType<'ctx> {
        self.context.i64_type()
    }

    fn float_type(&self) -> FloatType<'ctx> {
        self.context.f64_type()
    }

    fn bool_type(&self) -> IntType<'ctx> {
        self.context.bool_type() // i1
    }

    fn basic_type(&self, pinp_type: PinpType) -> BasicTypeEnum<'ctx> {
        match pinp_type {
            PinpType::Bool => self.bool_type().into(),
            PinpType::Int => self.int_type().into(),
            PinpType::Float => self.float_type().into(),
            PinpType::Void => unreachable!("Void is not a storable value type."),
        }
    }

    /// Emits the whole program and returns the type of its final expression.
    fn generate(&mut self) -> Result<PinpType, String> {
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
            self.gen_stmt(stmt)?;
        }

        let result = func
            .body
            .result
            .expect("a function body always ends in a result expression");
        if func.return_type == PinpType::Void {
            self.gen_expr(result)?; // side effects only
            self.builder.build_return(None).map_err(err)?;
        } else {
            let value = self.expect_value(result)?;
            let value = self.promote(value, self.ast.type_of(result), func.return_type);
            self.builder.build_return(Some(&value)).map_err(err)?;
        }
        self.in_function = false;
        Ok(())
    }

    /// Emits the `ENTRY` function from the top-level statements; its return value
    /// is the program's final expression (or void if it ends in a definition).
    fn gen_entry(&mut self) -> Result<PinpType, String> {
        let result_type = match self.ast.top_level.last() {
            Some(TopLevel::Stmt(Stmt::Expr(e))) => self.ast.type_of(*e),
            Some(TopLevel::Stmt(Stmt::Assign { values, .. })) => {
                self.ast.type_of(*values.last().unwrap())
            }
            _ => PinpType::Void,
        };

        let fn_type = match result_type {
            PinpType::Void => self.context.void_type().fn_type(&[], false),
            ty => self.basic_type(ty).fn_type(&[], false),
        };
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
                }
            }
        }

        match result_type {
            PinpType::Void => self.builder.build_return(None).map_err(err)?,
            _ => {
                let value = result.expect("A non-void program must yield a value.");
                self.builder.build_return(Some(&value)).map_err(err)?
            }
        };
        Ok(result_type)
    }

    /// Emits a statement, returning the value it yields (an `Expr`'s value, or an
    /// assignment's right-hand side) — used to pick up the program's result.
    fn gen_stmt(&mut self, stmt: &Stmt) -> Result<Option<BasicValueEnum<'ctx>>, String> {
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

    fn gen_expr(&mut self, expr_id: ExprId) -> Result<Option<BasicValueEnum<'ctx>>, String> {
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
        };
        Ok(Some(value))
    }

    // Lower an `if`/`elif`/`else` (and the one-line ternary, which is a single-arm `If`) as a chain
    // of diamonds joined at a merge block. When the node has a value (an `else` is present and every
    // branch yields one), a `phi` in the merge selects the taken branch's value; otherwise the `if`
    // runs purely for effect and yields nothing.
    fn gen_if(&mut self, expr_id: ExprId) -> Result<Option<BasicValueEnum<'ctx>>, String> {
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

    /// Evaluates `expr_id`, requiring it to produce a value (errors on a void expression).
    fn expect_value(&mut self, expr_id: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        self.gen_expr(expr_id)?
            .ok_or_else(|| "Expected a value but the expression is void.".to_string())
    }

    /// Evaluates `expr_id` as an `i64`, widening a `Bool` (`i1`) operand with a zero-extend.
    fn as_int(&mut self, expr_id: ExprId) -> Result<inkwell::values::IntValue<'ctx>, String> {
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
    fn promote(
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

    fn zero(&self, pinp_type: PinpType) -> BasicValueEnum<'ctx> {
        match pinp_type {
            PinpType::Bool => self.bool_type().const_zero().into(),
            PinpType::Int => self.int_type().const_zero().into(),
            PinpType::Float => self.float_type().const_zero().into(),
            PinpType::Void => unreachable!("Void has no zero value."),
        }
    }

    fn into_module(self) -> Module<'ctx> {
        self.module
    }
}

fn place_sym(place: Place) -> SymId {
    match place {
        Place::Local(sym_id) | Place::Global(sym_id) => sym_id,
    }
}

/// Extracts the value of a value-returning call (already filtered against void).
fn basic_value(kind: ValueKind) -> BasicValueEnum {
    match kind {
        ValueKind::Basic(value) => value,
        ValueKind::Instruction(_) => unreachable!("A value-returning call yields a basic value."),
    }
}

fn err(e: inkwell::builder::BuilderError) -> String {
    format!("IR builder error: {e}.")
}

// ---------------------------------------------------------------------------
// PinpJit: source string -> value
// ---------------------------------------------------------------------------

/// The value a pinp program evaluates to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PinpValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Void,
}

/// Compiles a pinp source string and JIT-executes it, the test harness for the
/// runtime. Holds the JIT and the context it borrows (context dropped last).
pub struct PinpJit {
    jit: Jit,
    result_type: PinpType,
    _context: Context,
}

impl PinpJit {
    /// Parses, type-checks, and JIT-compiles `src`, ready to [`run`](Self::run).
    pub fn new(src: &str) -> Result<Self, String> {
        let mut ast = parse(src).map_err(|error| format!("{error:?}"))?;
        analyze(&mut ast).map_err(|error| format!("{error:?}"))?;
        let context = Context::create();

        let mut codegen = CodeGen::new(&context, &ast);
        let result_type = codegen.generate()?;
        let module = codegen.into_module();

        let jit = Jit::new()?;
        jit.add_module(module)?;
        Ok(PinpJit {
            jit,
            result_type,
            _context: context,
        })
    }

    /// Executes the program and returns its result.
    pub fn run(&self) -> Result<PinpValue, String> {
        // SAFETY: the entry function's ABI matches the looked-up signature, chosen
        // from the program's statically inferred result type.
        unsafe {
            Ok(match self.result_type {
                PinpType::Bool => {
                    // The entry returns `i1`; read it through `u8` and mask the low bit, since an
                    // `i1` return is not guaranteed to zero-extend its upper bits.
                    let f: extern "C" fn() -> u8 = self.jit.lookup(ENTRY)?;
                    PinpValue::Bool(f() & 1 != 0)
                }
                PinpType::Int => {
                    let f: extern "C" fn() -> i64 = self.jit.lookup(ENTRY)?;
                    PinpValue::Int(f())
                }
                PinpType::Float => {
                    let f: extern "C" fn() -> f64 = self.jit.lookup(ENTRY)?;
                    PinpValue::Float(f())
                }
                PinpType::Void => {
                    let f: extern "C" fn() = self.jit.lookup(ENTRY)?;
                    f();
                    PinpValue::Void
                }
            })
        }
    }

    /// Convenience: compile and run `src` in one call.
    pub fn eval(src: &str) -> Result<PinpValue, String> {
        Self::new(src)?.run()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn jit_evaluates_two_plus_two() {
        let jit = Jit::new().unwrap();
        // The raw Jit spine is exercised end-to-end by the PinpJit tests below;
        // here we just confirm a fresh instance constructs.
        drop(jit);
    }

    #[test]
    fn function_call() {
        let result = PinpJit::eval(indoc! {"
            sq(x: int): int is x*x
            sq(5)
        "})
        .unwrap();
        assert_eq!(result, PinpValue::Int(25));
    }

    #[test]
    fn global_and_compound_assignment() {
        let result = PinpJit::eval(indoc! {"
            g = 100
            bump(a: int): int is
                ::g += 1
                a + ::g
            bump(5)
        "})
        .unwrap();
        assert_eq!(result, PinpValue::Int(106));
    }

    #[test]
    fn local_in_block_body() {
        let result = PinpJit::eval(indoc! {"
            f(a: int, b: int): int is
                s = a + b*b
                s
            f(2, 3)
        "})
        .unwrap();
        assert_eq!(result, PinpValue::Int(11));
    }

    #[test]
    fn and_or_short_circuit() {
        // The right operand traps (integer division by zero) if evaluated. Correct short-circuit
        // means a deciding left operand skips it, so these must not crash and must return the
        // left-determined value.
        assert_eq!(
            PinpJit::eval("false and (1 div 0 == 0)").unwrap(),
            PinpValue::Bool(false)
        );
        assert_eq!(
            PinpJit::eval("true or (1 div 0 == 0)").unwrap(),
            PinpValue::Bool(true)
        );
        // And the live path is still taken when the left operand does not decide.
        assert_eq!(
            PinpJit::eval("true and 2 > 1").unwrap(),
            PinpValue::Bool(true)
        );
    }

    #[test]
    fn syntax_error_is_reported_before_codegen() {
        // A malformed parameter list (missing `:`) fails in the parser, so the flow
        // never reaches code generation. PinpJit must surface that as a normal
        // `Err`, carrying the parser's own message verbatim.
        let src = "f(a int): int is a";
        let expected = format!("{:?}", parse(src).err().unwrap());
        let error = PinpJit::eval(src).unwrap_err();
        assert_eq!(error, expected);
        assert!(error.contains("Expected Colon"));
    }
}
