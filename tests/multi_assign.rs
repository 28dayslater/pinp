// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end multiple-target assignment: parallel (`a, b = 1, 2`), swap, and chained
//! (`a = b = c`). Requires the `llvm` feature.

mod common;
use common::{eval_float, eval_int};
use indoc::indoc;

#[test]
fn parallel_defines_both() {
    assert_eq!(
        eval_int(indoc! {"
            a, b = 1, 2
            a + b
        "}),
        3
    );
}

#[test]
fn parallel_swap() {
    // Right-hand side is evaluated in full before any store, so this is a real swap.
    assert_eq!(
        eval_int(indoc! {"
            a = 1
            b = 5
            a, b = b, a
            a - b
        "}),
        4 // a=5, b=1
    );
}

#[test]
fn chained_binds_all_to_one_value() {
    assert_eq!(
        eval_int(indoc! {"
            a = b = c = 5
            a + b + c
        "}),
        15
    );
}

#[test]
fn parallel_in_function_body() {
    assert_eq!(
        eval_int(indoc! {"
            f(): int is
                x, y = 3, 4
                x*x + y*y
            f()
        "}),
        25
    );
}

#[test]
fn swap_mutates_function_locals() {
    // after swap: a=5, b=1
    assert_eq!(
        eval_int(indoc! {"
            diff(a: int, b: int): int is
                a, b = b, a
                a - b
            diff(1, 5)
        "}),
        4
    );
}

#[test]
fn combined_parallel_and_chained() {
    // (a,b) and (c,d) all become (1, 2).
    assert_eq!(
        eval_int(indoc! {"
            a, b = c, d = 1, 2
            a + b + c + d
        "}),
        6
    );
}

#[test]
fn aliased_target_last_write_wins() {
    // The spec specifies left-to-right stores, so a repeated target keeps the last value.
    assert_eq!(
        eval_int(indoc! {"
            a, a = 1, 2
            a
        "}),
        2
    );
}

#[test]
fn chained_promotes_into_existing_float_slot() {
    // `a` is Float; the chained Int value is promoted into its slot per target, while `b` is Int.
    assert_eq!(
        eval_float(indoc! {"
            a = 1.0
            a = b = 5
            a
        "}),
        5.0
    );
}

#[test]
fn wrapped_rhs_evaluates() {
    // The RHS wraps onto a second line aligned under the first value (`1` is at column 8). Kept as
    // explicit `\n` + spaces, since the alignment column is exactly what this exercises.
    assert_eq!(eval_int("a, b = 1,\n       2\na + b"), 3);
}
