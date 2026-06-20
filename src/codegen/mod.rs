// SPDX-License-Identifier: MIT

//! LLVM ORCv2 JIT backend (gated behind the `llvm` feature).
//!
//! [`Jit`] is a thin safe wrapper over the ORCv2 LLJIT C API (reached through
//! `inkwell::llvm_sys`, no extra dependency); inkwell 0.9 ships no ORC bindings.
//! `CodeGen` lowers a parsed [`Ast`] into an LLVM module, and [`PinpJit`] ties
//! the two together: source string in, executed, [`PinpValue`] out. This is the
//! harness pinp's runtime tests are written against.

use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue, ValueKind};
use rustc_hash::FxHashMap;

use crate::parser::{Ast, PinpType, Place, SymId, parse};
use crate::sema::analyze;

mod expr;
mod jit;
mod lower;
mod stmt;

#[cfg(test)]
mod tests;

pub use jit::Jit;

// ---------------------------------------------------------------------------
// Module constants and runtime errors
// ---------------------------------------------------------------------------

/// Name of the synthetic entry function `CodeGen` emits for the top-level program.
const ENTRY: &str = "__pinp_main";

/// Name of the module global that carries a runtime-error code out to the host (`0` means none).
const RUNTIME_ERROR_SYMBOL: &str = "__pinp_runtime_error";

/// Runtime-error codes stored in [`RUNTIME_ERROR_SYMBOL`]. pinp has no runtime yet, so an error is
/// reported by code: the generated program records one and returns, and [`PinpJit::run`] maps it to
/// a message. Codes start at 1 so the initial `0` means "no error".
const RUNTIME_ERROR_ZERO_STEP: i64 = 1;

/// The message for a runtime-error code recorded in [`RUNTIME_ERROR_SYMBOL`].
fn runtime_error_message(code: i64) -> String {
    match code {
        RUNTIME_ERROR_ZERO_STEP => "Range step cannot be zero.".to_string(),
        other => format!("Unknown runtime error ({other})."),
    }
}

// ---------------------------------------------------------------------------
// Code generator
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

/// The symbol behind a place, whether it is a local or a global.
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

/// Wraps an inkwell IR-builder error as a message string.
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

    /// Executes the program and returns its result, or a runtime error the program raised (e.g. a
    /// range built with a zero step).
    pub fn run(&self) -> Result<PinpValue, String> {
        // SAFETY: the entry function's ABI matches the looked-up signature, chosen
        // from the program's statically inferred result type.
        let value = unsafe {
            match self.result_type {
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
                PinpType::Range => unreachable!("a program cannot evaluate to a range"),
            }
        };
        // The program runs to completion (pinp has no unwinding); a raised error is recorded in a
        // global, so it is read here — after execution — and reported instead of the value.
        match self.runtime_error()? {
            Some(message) => Err(message),
            None => Ok(value),
        }
    }

    /// The runtime error the last run recorded, if any.
    fn runtime_error(&self) -> Result<Option<String>, String> {
        // SAFETY: the symbol resolves to the `i64` runtime-error global declared in the module.
        let code = unsafe {
            let slot: *const i64 = self.jit.lookup(RUNTIME_ERROR_SYMBOL)?;
            *slot
        };
        Ok((code != 0).then(|| runtime_error_message(code)))
    }

    /// Convenience: compile and run `src` in one call.
    pub fn eval(src: &str) -> Result<PinpValue, String> {
        Self::new(src)?.run()
    }
}
