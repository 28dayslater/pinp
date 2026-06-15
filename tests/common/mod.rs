// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! Shared helpers for the end-to-end (integration) tests, which JIT-execute pinp
//! source through the public API and check the computed value.

use pinp::codegen::{PinpJit, PinpValue};

/// Compiles and runs `src`, returning its value; panics on any compile/run error.
pub fn eval(src: &str) -> PinpValue {
    PinpJit::eval(src).expect("Program failed to compile or run.")
}

/// Runs `src` and asserts it produced an integer.
pub fn eval_int(src: &str) -> i64 {
    match eval(src) {
        PinpValue::Int(n) => n,
        other => panic!("Expected an integer result, got {other:?}."),
    }
}

/// Runs `src` and asserts it produced a float.
pub fn eval_float(src: &str) -> f64 {
    match eval(src) {
        PinpValue::Float(f) => f,
        other => panic!("Expected a float result, got {other:?}."),
    }
}
