// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end bool evaluation: literals, comparisons, logicals, chained comparisons, and bool
//! flowing through function signatures and arithmetic. Requires the `llvm` feature.

mod common;
use common::{eval_bool, eval_int};

#[test]
fn literals_and_logicals() {
    assert!(eval_bool("true"));
    assert!(!eval_bool("false"));
    assert!(!eval_bool("true and false"));
    assert!(eval_bool("true or false"));
    assert!(!eval_bool("true xor true"));
    assert!(eval_bool("true xor false"));
    assert!(!eval_bool("not true"));
    // `a and b or a xor b` = `(a and b) or (a xor b)`.
    assert!(eval_bool("true and false or true xor false"));
}

#[test]
fn comparisons() {
    assert!(eval_bool("1 < 2"));
    assert!(!eval_bool("3 <= 2"));
    assert!(eval_bool("2.0 > 1")); // mixed int/float promotes
    assert!(eval_bool("1 == 1"));
    assert!(eval_bool("1 != 2"));
    assert!(eval_bool("true == true")); // bool operands compare directly
}

#[test]
fn bool_promotes_in_arithmetic() {
    assert_eq!(eval_int("true + true + false"), 2);
    assert_eq!(eval_int("(1 < 2) + (3 < 1)"), 1); // true + false = 1
                                                  // Mirrors the spec's `bar*baz + (b1 > b2)`: comparison promoted into the sum.
    assert_eq!(eval_int("2 * 3 + (5 > 2)"), 7);
}

#[test]
fn chained_comparisons() {
    assert!(eval_bool("1 < 2 < 3"));
    assert!(!eval_bool("1 < 5 < 3")); // 1<5 true, 5<3 false
    assert!(eval_bool("3 >= 2 >= 1"));
    assert!(eval_bool("0 <= 1 < 2")); // ascending, strict/non-strict mixed
    assert!(eval_bool("2 == 2 == 2"));
}

#[test]
fn bool_function_signature() {
    let prog = "fu(a: bool, b: bool): bool is a and b or a xor b\n";
    assert!(eval_bool(&format!("{prog}fu(true, false)")));
    assert!(eval_bool(&format!("{prog}fu(true, true)")));
    assert!(!eval_bool(&format!("{prog}fu(false, false)")));
}
