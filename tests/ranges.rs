// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end ranges: the `start..stop[:step]` value, the `for idx in <range>` loop, and the
//! `value in <range>` membership test — including stepped, descending, first-class, variable, and
//! empty ranges. Requires the `llvm` feature.

mod common;
use common::{eval_bool, eval_int};
use indoc::indoc;

// --- for-in loops --------------------------------------------------------------------------

#[test]
fn inclusive_loop_sums_both_ends() {
    // 1+2+3+4+5: `..` includes the stop.
    assert_eq!(
        eval_int(indoc! {"
            total = 0
            for idx in 1..5
                total += idx
            total
        "}),
        15
    );
}

#[test]
fn up_exclusive_loop_drops_the_stop() {
    // 1+2+3+4: `..<` excludes the stop.
    assert_eq!(
        eval_int(indoc! {"
            total = 0
            for idx in 1..<5
                total += idx
            total
        "}),
        10
    );
}

#[test]
fn stepped_loop_lands_on_reachable_values() {
    // 1,4,7,10 — the step divides the span exactly here, so the inclusive stop is hit.
    assert_eq!(
        eval_int(indoc! {"
            total = 0
            for idx in 1..10:3
                total += idx
            total
        "}),
        22
    );
}

#[test]
fn stepped_loop_stops_before_overshooting() {
    // 1,4,7,10,13,16,19 — 22 would pass the stop, so iteration ends at 19 (spec example `e`).
    assert_eq!(
        eval_int(indoc! {"
            total = 0
            for idx in 1..20:3
                total += idx
            total
        "}),
        70
    );
}

#[test]
fn descending_inclusive_loop() {
    // 5,4,3,2,1: `..` with a negative step counts down and keeps both ends.
    assert_eq!(
        eval_int(indoc! {"
            total = 0
            for idx in 5..1:-1
                total += idx
            total
        "}),
        15
    );
}

#[test]
fn descending_exclusive_loop() {
    // 17,15,13,11,9,7,5,3 — `..>` counts down and drops the stop (spec example `d`).
    assert_eq!(
        eval_int(indoc! {"
            total = 0
            for idx in 17..>1:-2
                total += idx
            total
        "}),
        80
    );
}

#[test]
fn first_class_range_bound_then_iterated() {
    // A range is a value: bind it, then drive a loop with the variable.
    assert_eq!(
        eval_int(indoc! {"
            r = 1..3
            total = 0
            for idx in r
                total += idx
            total
        "}),
        6
    );
}

#[test]
fn variable_bounds_drive_the_loop() {
    assert_eq!(
        eval_int(indoc! {"
            a, b, step = 1, 10, 2
            total = 0
            for idx in a..<b:step
                total += idx
            total
        "}),
        25 // 1+3+5+7+9
    );
}

#[test]
fn inconsistent_variable_range_is_empty() {
    // `a..<b` with a >= b at run time has no values: the body never runs.
    assert_eq!(
        eval_int(indoc! {"
            a, b = 5, 1
            count = 0
            for idx in a..<b
                count += 1
            count
        "}),
        0
    );
}

#[test]
fn runtime_zero_step_is_a_runtime_error() {
    // A zero step is only reachable through a variable; building such a range raises a runtime error
    // (reported as `Err`), rather than looping forever or silently doing nothing.
    let error = pinp::codegen::PinpJit::eval(indoc! {"
        step = 0
        total = 0
        for idx in 1..10:step
            total += idx
        total
    "})
    .expect_err("a zero-step range must be a runtime error");
    assert!(
        error.contains("Range step cannot be zero."),
        "unexpected error: {error}"
    );
}

#[test]
fn range_freezes_its_bounds_at_construction() {
    // `r` captures `a`'s value (1) when it is built; a later change to `a` does not reshape `r`.
    assert_eq!(
        eval_int(indoc! {"
            a = 1
            r = a..3
            a = 100
            total = 0
            for idx in r
                total += idx
            total
        "}),
        6 // 1 + 2 + 3, not an `a`-of-100 range
    );
}

// --- membership ----------------------------------------------------------------------------

#[test]
fn membership_on_a_plain_range() {
    assert!(eval_bool("5 in 1..10"));
    assert!(eval_bool("10 in 1..10")); // inclusive stop
    assert!(!eval_bool("0 in 1..10"));
    assert!(!eval_bool("11 in 1..10"));
}

#[test]
fn membership_respects_the_step() {
    // 1,4,7,10 — only on-step values are members.
    assert!(eval_bool("7 in 1..10:3"));
    assert!(!eval_bool("8 in 1..10:3"));
}

#[test]
fn membership_with_a_variable_step() {
    // {1,3,5,7,9}: closed-form arithmetic, no iteration needed.
    assert!(eval_bool(indoc! {"
        a, b, step = 1, 10, 2
        7 in a..<b:step
    "}));
    assert!(!eval_bool(indoc! {"
        a, b, step = 1, 10, 2
        8 in a..<b:step
    "}));
}

#[test]
fn membership_of_an_empty_range_is_false() {
    assert!(!eval_bool(indoc! {"
        a, b = 5, 1
        3 in a..<b
    "}));
}
