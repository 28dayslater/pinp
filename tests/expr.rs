// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end expression evaluation: pinp source is JIT-compiled and run, and the
//! computed value is verified. Requires the `llvm` feature.

mod common;
use common::{eval_float, eval_int};

#[test]
fn integer_arithmetic_and_precedence() {
    assert_eq!(eval_int("2 + 3 * 4"), 14);
    assert_eq!(eval_int("(2 + 3) * 4"), 20);
    assert_eq!(eval_int("10 - 2 - 3"), 5); // subtraction is left-associative
}

#[test]
fn unary_minus() {
    assert_eq!(eval_int("-5 + 2"), -3);
    assert_eq!(eval_int("-2 ^ 2"), -4); // unary minus binds below `^`, so -(2^2)
}

#[test]
fn integer_div_and_mod() {
    assert_eq!(eval_int("17 div 5"), 3);
    assert_eq!(eval_int("17 mod 5"), 2);
}

#[test]
fn float_arithmetic_and_promotion() {
    assert_eq!(eval_float("1.5 + 2.5"), 4.0);
    assert_eq!(eval_float("7 / 2"), 3.5); // `/` always yields a float
    assert_eq!(eval_float("3 + 0.5"), 3.5); // mixed operands promote int to float
}

#[test]
fn power_is_right_associative() {
    assert_eq!(eval_int("2 ^ 10"), 1024);
    assert_eq!(eval_float("2.0 ^ 3.0"), 8.0);
    assert_eq!(eval_int("2 ^ 3 ^ 2"), 512); // 2^(3^2) = 2^9
}
