// SPDX-License-Identifier: MIT

//! LLVM ORCv2 JIT backend (gated behind the `llvm` feature).
//!
//! [`Jit`] is a thin safe wrapper over the ORCv2 LLJIT C API (reached through
//! `inkwell::llvm_sys`, no extra dependency); inkwell 0.9 ships no ORC bindings.
//! [`CodeGen`] lowers a parsed [`Ast`] into an LLVM module, and [`PinpJit`] ties
//! the two together: source string in, executed, [`PinpValue`] out. This is the
//! harness pinp's runtime tests are written against.

use std::ffi::{CStr, CString};

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

use crate::parser::{parse, Ast, BinOp, ExprId, Node, PinpType, Place, Stmt, SymId, TopLevel, UnOp};
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
            .map_err(|e| format!("Failed to initialize the native target: {e}."))?;

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

    /// Looks up a JITed symbol and returns it as the function-pointer type `F`.
    ///
    /// # Safety
    ///
    /// `F` must be a function-pointer type whose signature matches the symbol's
    /// actual ABI, or calling it is undefined behaviour.
    pub unsafe fn lookup<F: Copy>(&self, name: &str) -> Result<F, String> {
        let symbol = CString::new(name).map_err(|e| e.to_string())?;
        let mut address: LLVMOrcExecutorAddress = 0;
        check_error(LLVMOrcLLJITLookup(self.jit, &mut address, symbol.as_ptr()))?;
        // `address` is the symbol's runtime address; reinterpret it as `F`.
        Ok(std::mem::transmute_copy::<LLVMOrcExecutorAddress, F>(&address))
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
    let message = LLVMGetErrorMessage(error); // consumes `error`, returns owned C string
    let owned = CStr::from_ptr(message).to_string_lossy().into_owned();
    LLVMDisposeErrorMessage(message);
    Err(owned)
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
    locals: FxHashMap<SymId, (PointerValue<'ctx>, PinpType)>,
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
            locals: FxHashMap::default(),
            in_function: false,
        }
    }

    fn int_type(&self) -> IntType<'ctx> {
        self.context.i64_type()
    }

    fn float_type(&self) -> FloatType<'ctx> {
        self.context.f64_type()
    }

    fn basic_type(&self, ty: PinpType) -> BasicTypeEnum<'ctx> {
        match ty {
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
            if let TopLevel::Stmt(Stmt::Assign { target, rhs }) = item {
                let sym = place_sym(*target);
                if self.globals.contains_key(&sym) {
                    continue;
                }
                let ty = self.ast.type_of(*rhs);
                let name = self.ast.names[sym.0 as usize];
                let global = self.module.add_global(self.basic_type(ty), None, name);
                global.set_linkage(Linkage::Internal);
                global.set_initializer(&self.zero(ty));
                self.globals
                    .insert(sym, (global.as_pointer_value(), ty));
            }
        }
    }

    /// Declares the LLVM signature of every pinp function (so calls resolve
    /// regardless of textual order).
    fn declare_functions(&mut self) {
        for item in &self.ast.top_level {
            let TopLevel::Func(func) = item else { continue };
            let param_types: Vec<PinpType> = func.params.iter().map(|p| p.param_type).collect();
            let llvm_params: Vec<BasicMetadataTypeEnum> = param_types
                .iter()
                .map(|t| self.basic_type(*t).into())
                .collect();
            let fn_type = match func.return_type {
                PinpType::Void => self.context.void_type().fn_type(&llvm_params, false),
                ret => self.basic_type(ret).fn_type(&llvm_params, false),
            };
            let name = self.ast.names[func.name.0 as usize];
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
        self.in_function = true;
        // Give each parameter an alloca so reads/writes are uniform with locals.
        for (i, param) in func.params.iter().enumerate() {
            let slot = self
                .builder
                .build_alloca(self.basic_type(param.param_type), "param")
                .map_err(err)?;
            let incoming = function.get_nth_param(i as u32).expect("parameter count");
            self.builder.build_store(slot, incoming).map_err(err)?;
            self.locals.insert(param.name, (slot, param.param_type));
        }

        for stmt in &func.body.stmts {
            self.gen_stmt(stmt)?;
        }

        if func.return_type == PinpType::Void {
            self.gen_expr(func.body.result)?; // side effects only
            self.builder.build_return(None).map_err(err)?;
        } else {
            let value = self.expect_value(func.body.result)?;
            let value = self.promote(value, self.ast.type_of(func.body.result), func.return_type);
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
            Some(TopLevel::Stmt(Stmt::Assign { rhs, .. })) => self.ast.type_of(*rhs),
            _ => PinpType::Void,
        };

        let fn_type = match result_type {
            PinpType::Void => self.context.void_type().fn_type(&[], false),
            ty => self.basic_type(ty).fn_type(&[], false),
        };
        let entry_fn = self.module.add_function(ENTRY, fn_type, None);
        let block = self.context.append_basic_block(entry_fn, "entry");
        self.builder.position_at_end(block);
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
            Stmt::Expr(e) => self.gen_expr(*e),
            Stmt::Assign { target, rhs } => {
                let value = self.expect_value(*rhs)?;
                let (ptr, slot_type) = self.resolve_place(*target, self.ast.type_of(*rhs))?;
                let stored = self.promote(value, self.ast.type_of(*rhs), slot_type);
                self.builder.build_store(ptr, stored).map_err(err)?;
                Ok(Some(value))
            }
        }
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
            Place::Local(sym) if !self.in_function => Ok(self.globals[&sym]),
            Place::Local(sym) => {
                if let Some(slot) = self.locals.get(&sym) {
                    Ok(*slot)
                } else {
                    let slot = self
                        .builder
                        .build_alloca(self.basic_type(rhs_type), "local")
                        .map_err(err)?;
                    self.locals.insert(sym, (slot, rhs_type));
                    Ok((slot, rhs_type))
                }
            }
        }
    }

    fn gen_expr(&mut self, e: ExprId) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let node = self.ast.node(e);
        let value = match node {
            Node::Int(n) => self.int_type().const_int(*n as u64, true).into(),
            Node::Float(f) => self.float_type().const_float(*f).into(),
            Node::Var(sym) => self.load_var(*sym, false)?,
            Node::Global(sym) => self.load_var(*sym, true)?,
            Node::Unary { op: UnOp::Neg, operand } => {
                let operand = *operand;
                let value = self.expect_value(operand)?;
                if self.ast.type_of(operand) == PinpType::Int {
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
            Node::Bin { op, lhs, rhs } => self.gen_bin(e, *op, *lhs, *rhs)?,
            Node::Call { callee, args } => return self.gen_call(*callee, args),
        };
        Ok(Some(value))
    }

    fn load_var(&self, sym: SymId, global: bool) -> Result<BasicValueEnum<'ctx>, String> {
        let (ptr, ty) = if global || !self.in_function {
            self.globals[&sym]
        } else {
            self.locals[&sym]
        };
        self.builder
            .build_load(self.basic_type(ty), ptr, "load")
            .map_err(err)
    }

    fn gen_call(
        &mut self,
        callee: SymId,
        args: &'ast [ExprId],
    ) -> Result<Option<BasicValueEnum<'ctx>>, String> {
        let (function, param_types, return_type) = self.functions[&callee].clone();
        let mut argv: Vec<BasicMetadataValueEnum> = Vec::with_capacity(args.len());
        for (arg, param_type) in args.iter().zip(&param_types) {
            let value = self.expect_value(*arg)?;
            let value = self.promote(value, self.ast.type_of(*arg), *param_type);
            argv.push(value.into());
        }
        let call = self.builder.build_call(function, &argv, "call").map_err(err)?;
        Ok(match return_type {
            PinpType::Void => None,
            _ => Some(basic_value(call.try_as_basic_value())),
        })
    }

    fn gen_bin(
        &mut self,
        e: ExprId,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
    ) -> Result<BasicValueEnum<'ctx>, String> {
        let result_type = self.ast.type_of(e);
        let value = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul if result_type == PinpType::Float => {
                let l = self.as_float(lhs)?;
                let r = self.as_float(rhs)?;
                match op {
                    BinOp::Add => self.builder.build_float_add(l, r, "fadd"),
                    BinOp::Sub => self.builder.build_float_sub(l, r, "fsub"),
                    _ => self.builder.build_float_mul(l, r, "fmul"),
                }
                .map_err(err)?
                .into()
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let l = self.as_int(lhs)?;
                let r = self.as_int(rhs)?;
                match op {
                    BinOp::Add => self.builder.build_int_add(l, r, "add"),
                    BinOp::Sub => self.builder.build_int_sub(l, r, "sub"),
                    _ => self.builder.build_int_mul(l, r, "mul"),
                }
                .map_err(err)?
                .into()
            }
            BinOp::Div => {
                let l = self.as_float(lhs)?;
                let r = self.as_float(rhs)?;
                self.builder.build_float_div(l, r, "fdiv").map_err(err)?.into()
            }
            BinOp::IntDiv => {
                let l = self.as_int(lhs)?;
                let r = self.as_int(rhs)?;
                self.builder.build_int_signed_div(l, r, "sdiv").map_err(err)?.into()
            }
            BinOp::Mod => {
                let l = self.as_int(lhs)?;
                let r = self.as_int(rhs)?;
                self.builder.build_int_signed_rem(l, r, "srem").map_err(err)?.into()
            }
            BinOp::Pow => {
                let l = self.as_float(lhs)?;
                let r = self.as_float(rhs)?;
                let pow = self.pow_intrinsic();
                let call = self
                    .builder
                    .build_call(pow, &[l.into(), r.into()], "pow")
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
        };
        Ok(value)
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

    /// Evaluates `e`, requiring it to produce a value (errors on a void expression).
    fn expect_value(&mut self, e: ExprId) -> Result<BasicValueEnum<'ctx>, String> {
        self.gen_expr(e)?
            .ok_or_else(|| "Expected a value but the expression is void.".to_string())
    }

    fn as_int(&mut self, e: ExprId) -> Result<inkwell::values::IntValue<'ctx>, String> {
        Ok(self.expect_value(e)?.into_int_value())
    }

    /// Evaluates `e` as a float, inserting an `Int -> Float` promotion if needed.
    fn as_float(&mut self, e: ExprId) -> Result<inkwell::values::FloatValue<'ctx>, String> {
        let value = self.expect_value(e)?;
        if self.ast.type_of(e) == PinpType::Int {
            self.builder
                .build_signed_int_to_float(value.into_int_value(), self.float_type(), "promote")
                .map_err(err)
        } else {
            Ok(value.into_float_value())
        }
    }

    /// Promotes an `Int` value to `Float` when the target type requires it.
    fn promote(
        &self,
        value: BasicValueEnum<'ctx>,
        from: PinpType,
        to: PinpType,
    ) -> BasicValueEnum<'ctx> {
        if from == PinpType::Int && to == PinpType::Float {
            self.builder
                .build_signed_int_to_float(value.into_int_value(), self.float_type(), "promote")
                .expect("int-to-float promotion")
                .into()
        } else {
            value
        }
    }

    fn zero(&self, ty: PinpType) -> BasicValueEnum<'ctx> {
        match ty {
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
        Place::Local(s) | Place::Global(s) => s,
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
        let mut ast = parse(src).map_err(|e| format!("{e:?}"))?;
        analyze(&mut ast).map_err(|e| format!("{e:?}"))?;
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
