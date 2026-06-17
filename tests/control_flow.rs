// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end control-flow evaluation: the `if` expression (one-line ternary and block form),
//! `while`/`loop` statements, and the lexical mutate-outer scoping that lets loops drive a counter.
//! Requires the `llvm` feature.

mod common;
use common::{eval_bool, eval_float, eval_int};
use indoc::indoc;

#[test]
fn ternary_selects_branch() {
    assert_eq!(eval_int("10 if true else 20"), 10);
    assert_eq!(eval_int("10 if false else 20"), 20);
    // The branches join: an Int then-value is promoted to the Float result type.
    assert_eq!(eval_float("1 if true else 2.0"), 1.0);
    assert!(eval_bool("true if 1 < 2 else false"));
}

#[test]
fn wrapped_ternary_executes() {
    // The human's example, with the `else` on a second line aligned to the then-expression.
    assert_eq!(
        eval_int(indoc! {"
            a = 50
            fu = 42 + 142 if a > 42
                 else 42
            fu
        "}),
        184
    );
}

#[test]
fn max_via_block_if() {
    let src = indoc! {"
        mx(a: int, b: int): int is
            if a > b
                a
            else
                b
        mx(3, 9)
    "};
    assert_eq!(eval_int(src), 9);
}

#[test]
fn elif_chain() {
    let src = indoc! {"
        classify(x: int): int is
            if x > 0
                1
            elif x < 0
                0
            else
                2
        classify(-5)
    "};
    assert_eq!(eval_int(src), 0);
}

#[test]
fn conditional_update_of_outer_var() {
    // `m` is altered inside the `if`; the mutation is observed after it (mutate-outer).
    let src = indoc! {"
        pick(a: int, b: int): int is
            m = a
            if b > a
                m = b
            m
        pick(4, 7)
    "};
    assert_eq!(eval_int(src), 7);
}

#[test]
fn while_loop_accumulates_with_bare_counter() {
    let src = indoc! {"
        sum_to(n: int): int is
            i = 0
            s = 0
            while i < n
                s += i
                i += 1
            s
        sum_to(5)
    "};
    assert_eq!(eval_int(src), 10); // 0+1+2+3+4
}

#[test]
fn loop_while_runs_body_first() {
    let src = indoc! {"
        f(): int is
            i = 0
            loop
                i += 1
            while i < 3
            i
        f()
    "};
    assert_eq!(eval_int(src), 3);
}

#[test]
fn loop_until_stops_when_true() {
    let src = indoc! {"
        f(): int is
            i = 0
            loop
                i += 1
            until i >= 3
            i
        f()
    "};
    assert_eq!(eval_int(src), 3);
}

#[test]
fn top_level_while_through_globals() {
    let src = indoc! {"
        i = 0
        total = 0
        while i < 4
            total += i
            i += 1
        total
    "};
    assert_eq!(eval_int(src), 6); // 0+1+2+3
}

#[test]
fn if_value_bound_then_used() {
    // A block `if` as an assignment's value, then fed into arithmetic.
    let src = indoc! {"
        step(x: int): int is
            r = if x > 0
                1
            else
                0
            r + 100
        step(7)
    "};
    assert_eq!(eval_int(src), 101);
}
