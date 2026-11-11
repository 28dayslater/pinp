// SPDX-License-Identifier: MIT

//! The backend: a type-checked program compiled to machine code by Cranelift and run.
//!
//! [`PinpJit`] is the harness pinp's runtime tests are written against: source string in,
//! executed, [`PinpValue`] out.

use crate::parser::{ArrayElementType, PinpType, parse};
use crate::runtime::string::{PinpStr, pinp_str_free};
use crate::sema::analyze;

mod cranelift;

/// The value a pinp program evaluates to.
#[derive(Debug, Clone, PartialEq)]
pub enum PinpValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    /// Content of a `str` result, copied out of the program's `PinpStr` before it is freed. pinp
    /// strings are byte strings (ASCII by design, any other byte carried opaquely), so content that
    /// is not valid UTF-8 arrives with the usual replacement characters rather than failing.
    Str(String),
    Void,
    Array(Vec<PinpValue>),
    Matrix {
        rows: usize,
        cols: usize,
        /// Elements in row-major order: `elements[row * cols + col]`.
        elements: Vec<PinpValue>,
    },
}

/// Compiles a pinp source string and JIT-executes it, the test harness for the runtime.
pub struct PinpJit {
    compiled: cranelift::Compiled,
    result_type: PinpType,
}

impl PinpJit {
    /// Parses, type-checks, and JIT-compiles `src`, ready to [`run`](Self::run).
    pub fn new(src: &str) -> Result<Self, String> {
        let mut ast = parse(src).map_err(|error| format!("{error:?}"))?;
        analyze(&mut ast).map_err(|error| format!("{error:?}"))?;
        let (compiled, result_type) = cranelift::compile(&ast)?;
        Ok(PinpJit {
            compiled,
            result_type,
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

        let entry = self.compiled.entry();
        // `u64`s (not bytes), so the buffer is 8-byte aligned: the entry writes each result word
        // with an aligned store. Two of them, because the widest result is the 16-byte `PinpStr`;
        // a scalar uses the first only.
        let mut result_buf: [u64; 2] = [0; 2];
        let result_ptr = result_buf.as_mut_ptr().cast::<u8>();
        let mut error: *const std::ffi::c_char = std::ptr::null();

        unsafe { pinp_run(entry, result_ptr, &mut error) };

        if !error.is_null() {
            let msg = unsafe { std::ffi::CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            return Err(msg);
        }

        Ok(match self.result_type {
            PinpType::Bool => PinpValue::Bool(result_buf[0] as u8 & 1 != 0),
            PinpType::Int => PinpValue::Int(result_buf[0] as i64),
            PinpType::Float => PinpValue::Float(f64::from_bits(result_buf[0])),
            // The program moved its `PinpStr` out into the buffer, so the host now owns it: copy the
            // content, then release the storage. This is the matching free for that move-out — the
            // one deterministic free in place before the freeing step lands.
            PinpType::Str => {
                // SAFETY: a `Str` program wrote a whole `PinpStr` into the 16-byte buffer.
                let mut pinp_str = unsafe { result_ptr.cast::<PinpStr>().read() };
                let text = String::from_utf8_lossy(pinp_str.as_bytes()).into_owned();
                // SAFETY: the descriptor is live, owned here, and freed exactly once.
                unsafe { pinp_str_free(&mut pinp_str) };
                PinpValue::Str(text)
            }
            PinpType::Void => PinpValue::Void,
            PinpType::Range => unreachable!("a program cannot evaluate to a range"),
            PinpType::Never => unreachable!("`return` outside a function is rejected"),
            PinpType::Matrix(elem_type, row_count, col_count) => {
                let matrix_ptr = result_buf[0] as *const u8;
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
                let array_ptr = result_buf[0] as *const u8;
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
