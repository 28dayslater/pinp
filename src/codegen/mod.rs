// SPDX-License-Identifier: MIT

//! LLVM ORCv2 JIT backend (gated behind the `llvm` feature).
//!
//! [`Jit`] is a thin safe wrapper over the ORCv2 LLJIT C API (reached through
//! `inkwell::llvm_sys`, no extra dependency); inkwell 0.9 ships no ORC bindings.
//! `CodeGen` lowers a parsed [`Ast`] into an LLVM module, and [`PinpJit`] ties
//! the two together: source string in, executed, [`PinpValue`] out. This is the
//! harness pinp's runtime tests are written against.

use inkwell::AddressSpace;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue, ValueKind};
use rustc_hash::FxHashMap;

use crate::parser::{ArrayElementType, Ast, PinpType, Place, SymId, parse};
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
#[derive(Debug, Clone, PartialEq)]
pub enum PinpValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Void,
    Array(Vec<PinpValue>),
    Matrix {
        rows: usize,
        cols: usize,
        /// Elements in row-major order: `elements[row * cols + col]`.
        elements: Vec<PinpValue>,
    },
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
        unsafe extern "C" {
            // The C trampoline that holds the setjmp frame: calls entry(result_buf) and catches
            // any pinp_runtime_error longjmp, writing the error string to *error_out.
            fn pinp_run(
                entry: unsafe extern "C" fn(*mut u8),
                result: *mut u8,
                error_out: *mut *const std::ffi::c_char,
            );
        }

        // SAFETY: the entry is always `void (*)(void*)` now — the uniform out-pointer ABI.
        let entry: unsafe extern "C" fn(*mut u8) = unsafe { self.jit.lookup(ENTRY)? };
        // Use `u64` (not `[u8; 8]`) so the buffer has 8-byte alignment. LLVM emits `align 8`
        // stores for i64/f64 and pointer-sized values; a misaligned target is UB in LLVM's model.
        let mut result_buf: u64 = 0;
        let result_ptr = (&raw mut result_buf).cast::<u8>();
        let mut error: *const std::ffi::c_char = std::ptr::null();

        unsafe { pinp_run(entry, result_ptr, &mut error) };

        if !error.is_null() {
            let msg = unsafe { std::ffi::CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            return Err(msg);
        }

        Ok(match self.result_type {
            PinpType::Bool => PinpValue::Bool(result_buf as u8 & 1 != 0),
            PinpType::Int => PinpValue::Int(result_buf as i64),
            PinpType::Float => PinpValue::Float(f64::from_bits(result_buf)),
            PinpType::Void => PinpValue::Void,
            PinpType::Range => unreachable!("a program cannot evaluate to a range"),
            PinpType::Matrix(elem_type, row_count, col_count) => {
                let matrix_ptr = result_buf as *const u8;
                let total = row_count * col_count;
                let mut elements = Vec::with_capacity(total);
                for i in 0..total {
                    let element = match elem_type {
                        ArrayElementType::Bool => {
                            PinpValue::Bool(unsafe { *matrix_ptr.add(i) } & 1 != 0)
                        }
                        ArrayElementType::Int => {
                            let ptr = unsafe { matrix_ptr.add(i * 8) } as *const i64;
                            PinpValue::Int(unsafe { *ptr })
                        }
                        ArrayElementType::Float => {
                            let ptr = unsafe { matrix_ptr.add(i * 8) } as *const f64;
                            PinpValue::Float(unsafe { *ptr })
                        }
                    };
                    elements.push(element);
                }
                PinpValue::Matrix {
                    rows: row_count,
                    cols: col_count,
                    elements,
                }
            }
            PinpType::Array(elem_type, n) => {
                // The entry wrote the heap pointer into result_buf (already a `u64`).
                let array_ptr = result_buf as *const u8;
                let mut elements = Vec::with_capacity(n);
                for i in 0..n {
                    let element = match elem_type {
                        ArrayElementType::Bool => {
                            PinpValue::Bool(unsafe { *array_ptr.add(i) } & 1 != 0)
                        }
                        ArrayElementType::Int => {
                            let ptr = unsafe { array_ptr.add(i * 8) } as *const i64;
                            PinpValue::Int(unsafe { *ptr })
                        }
                        ArrayElementType::Float => {
                            let ptr = unsafe { array_ptr.add(i * 8) } as *const f64;
                            PinpValue::Float(unsafe { *ptr })
                        }
                    };
                    elements.push(element);
                }
                PinpValue::Array(elements)
            }
        })
    }

    /// Convenience: compile and run `src` in one call.
    pub fn eval(src: &str) -> Result<PinpValue, String> {
        Self::new(src)?.run()
    }
}
