// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]
// Each integration-test binary compiles this module separately and uses only the helpers it
// needs, so an unused helper here is expected rather than dead code.
#![allow(dead_code)]

//! Shared helpers for the end-to-end (integration) tests, which JIT-execute pinp
//! source through the public API and check the computed value.

use pinp::codegen::{PinpJit, PinpValue};

/// Compiles and runs `src`, returning its value; panics on any compile/run error.
pub fn eval(src: &str) -> PinpValue {
    PinpJit::eval(src).expect("Program failed to compile or run.")
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
