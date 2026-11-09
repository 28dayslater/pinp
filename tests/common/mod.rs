// SPDX-License-Identifier: MIT
// Each integration-test binary compiles this module separately and uses only the helpers it
// needs, so an unused helper here is expected rather than dead code.
#![allow(dead_code)]

//! Shared helpers for the end-to-end (integration) tests, which JIT-execute pinp
//! source through the public API and check the computed value.

use pinp::codegen::{PinpJit, PinpValue};

/// Compiles and runs `src`, returning its value; panics on any compile/run error.
pub fn eval(src: &str) -> PinpValue {
    eval_result(src).expect("Program failed to compile or run.")
}

/// Compiles and runs `src`, returning its value or its error.
///
/// Every program is also desugared to the IR and verified first, so the whole end-to-end corpus
/// doubles as a test of the IR.
pub fn eval_result(src: &str) -> Result<PinpValue, String> {
    check_ir(src);
    PinpJit::eval(src)
}

/// Desugars `src`, then flattens it, verifying every function at both levels and printing the one
/// that fails. A program that does not compile has no IR to check.
fn check_ir(src: &str) {
    let Ok(mut ast) = pinp::parser::parse(src) else {
        return;
    };
    if pinp::sema::analyze(&mut ast).is_err() {
        return;
    }
    let mut program = pinp::desugar::desugar(&ast);
    for (_, function) in program.functions.iter() {
        if let Err(error) = pinp::ir::verify(function) {
            let printer = pinp::ir::Printer::new(&program.tables, &ast.names, &ast.string_literals);
            panic!("{error:?} in\n{}", printer.function(function));
        }
    }
    let functions: Vec<_> = program.functions.iter().map(|(_, f)| f.clone()).collect();
    for function in functions {
        let flat = pinp::flatten::flatten(function, &mut program.tables);
        if let Err(error) = pinp::ir::verify(&flat) {
            let printer = pinp::ir::Printer::new(&program.tables, &ast.names, &ast.string_literals);
            panic!("{error:?} in\n{}", printer.function(&flat));
        }
    }
}

/// Defines a typed `eval_*` helper that runs `src` and unwraps the expected [`PinpValue`] variant,
/// panicking if the program produced a different type. One invocation per pinp value type keeps the
/// run-and-unwrap boilerplate in a single place as the language grows more types.
macro_rules! eval_as {
    ($helper:ident -> $ty:ty = $variant:ident) => {
        pub fn $helper(src: &str) -> $ty {
            match eval(src) {
                PinpValue::$variant(value) => value,
                other => panic!("Expected {}, got {other:?}.", stringify!($variant)),
            }
        }
    };
}

eval_as!(eval_int -> i64 = Int);
eval_as!(eval_float -> f64 = Float);
eval_as!(eval_bool -> bool = Bool);
eval_as!(eval_array -> Vec<PinpValue> = Array);
eval_as!(eval_str -> String = Str);

/// The runtime allocator's bookkeeping, mirroring `pinp_mem_info` in src/runtime/pinp_runtime.h.
/// Every pinp heap allocation — arrays, and now string storage — is counted here.
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct MemoryCounts {
    pub outstanding_bytes: i64,
    pub allocation_count: i64,
    pub free_count: i64,
}

/// Reads the runtime's current allocation bookkeeping for the calling thread — the thread a test
/// runs its programs on.
pub fn memory_counts() -> MemoryCounts {
    unsafe extern "C" {
        fn pinp_memory_info(info: *mut MemoryCounts);
    }
    let mut counts = MemoryCounts::default();
    // SAFETY: the signature matches the shim's, and the out-parameter is a live local.
    unsafe { pinp_memory_info(&mut counts) };
    counts
}

/// Runs `src` and unwraps a `PinpValue::Matrix`, returning `(rows, cols, elements)`.
pub fn eval_matrix(src: &str) -> (usize, usize, Vec<PinpValue>) {
    match eval(src) {
        PinpValue::Matrix {
            rows,
            cols,
            elements,
        } => (rows, cols, elements),
        other => panic!("Expected Matrix, got {other:?}."),
    }
}
