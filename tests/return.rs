// SPDX-License-Identifier: MIT

//! End-to-end `return` (0018): leaving a function early, from every kind of body, at any depth.

mod common;
use common::{eval_bool, eval_float, eval_int, eval_str};
use indoc::indoc;

#[test]
fn a_return_ends_straight_line_code() {
    assert_eq!(
        eval_int(indoc! {"
            f(n: int): int is
                return n * 2
                0
            f(21)
        "}),
        42
    );
}

#[test]
fn every_arm_of_an_if_chain_can_return() {
    let sign = indoc! {"
        sign(n: int): int is
            if n < 0
                return -1
            elif n == 0
                return 0
            else
                return 1
    "};
    assert_eq!(eval_int(&format!("{sign}sign(-7)")), -1);
    assert_eq!(eval_int(&format!("{sign}sign(0)")), 0);
    assert_eq!(eval_int(&format!("{sign}sign(9)")), 1);
}

#[test]
fn ifs_without_else_return_early_and_otherwise_fall_through() {
    let sign = indoc! {"
        sign(n: int): int is
            if n < 0
                return -1
            if n == 0
                return 0
            1
    "};
    assert_eq!(
        eval_int(&format!("{sign}sign(-3) * 100 + sign(0) * 10 + sign(3)")),
        -99
    );
}

#[test]
fn abs_returns_from_both_branches() {
    let abs = indoc! {"
        abs(n: int): int is
            if n < 0
                return -n
            else
                return n
    "};
    assert_eq!(eval_int(&format!("{abs}abs(-4) + abs(3)")), 7);
}

#[test]
fn a_return_leaves_a_while_loop() {
    assert_eq!(
        eval_int(indoc! {"
            next_power(n: int): int is
                p = 1
                while true
                    if p >= n
                        return p
                    p *= 2
                0
            next_power(100)
        "}),
        128
    );
}

#[test]
fn a_return_leaves_a_loop_until() {
    let src = indoc! {"
        root_above(n: int): int is
            i = 0
            loop
                i += 1
                if i * i > n
                    return i
            until i > 100
            -1
    "};
    assert_eq!(eval_int(&format!("{src}root_above(50)")), 8);
    assert_eq!(eval_int(&format!("{src}root_above(100000)")), -1);
}

#[test]
fn a_return_leaves_a_counted_loop_or_the_loop_runs_out() {
    let src = indoc! {"
        smallest_divisor(n: int): int is
            for d in 2..n
                if n mod d == 0
                    return d
            n
    "};
    assert_eq!(eval_int(&format!("{src}smallest_divisor(91)")), 7);
    assert_eq!(eval_int(&format!("{src}smallest_divisor(13)")), 13);
}

#[test]
fn a_return_leaves_an_array_loop() {
    let src = indoc! {"
        index_of(x: int): int is
            for i, v in [4, 8, 15, 16, 23, 42]
                if v == x
                    return i
            -1
    "};
    assert_eq!(eval_int(&format!("{src}index_of(15)")), 2);
    assert_eq!(eval_int(&format!("{src}index_of(5)")), -1);
}

#[test]
fn a_return_leaves_loops_nested_three_deep() {
    assert_eq!(
        eval_int(indoc! {"
            find(): int is
                for a in 1..5
                    for b in 1..5
                        for c in 1..5
                            if a * b * c == 24
                                return a * 100 + b * 10 + c
                0
            find()
        "}),
        234
    );
}

#[test]
fn a_return_as_the_last_statement_of_a_loop_body_returns_on_the_first_pass() {
    assert_eq!(
        eval_int(indoc! {"
            f(n: int): int is
                for i in 5..n
                    return i
                0
            f(9)
        "}),
        5
    );
    assert_eq!(
        eval_int(indoc! {"
            f(n: int): int is
                for i in 5..<n
                    return i
                0
            f(1)
        "}),
        0,
        "an empty range never runs the body"
    );
}

#[test]
fn an_int_returned_from_a_float_function_is_promoted() {
    let src = indoc! {"
        half(n: int): float is
            if n == 0
                return 0
            n / 2
    "};
    assert_eq!(eval_float(&format!("{src}half(0)")), 0.0);
    assert_eq!(eval_float(&format!("{src}half(3)")), 1.5);
}

#[test]
fn a_bool_function_returns_early() {
    let src = indoc! {"
        is_prime(n: int): bool is
            if n < 2
                return false
            for d in 2..<n
                if n mod d == 0
                    return false
            true
    "};
    assert!(eval_bool(&format!("{src}is_prime(13)")));
    assert!(!eval_bool(&format!("{src}is_prime(15)")));
    assert!(!eval_bool(&format!("{src}is_prime(1)")));
}

#[test]
fn a_void_function_returns_early_and_skips_the_rest() {
    assert_eq!(
        eval_int(indoc! {"
            g = 1
            set(x: int) is
                if x < 0
                    return
                ::g = x
            set(-5)
            a = g
            set(7)
            a * 100 + g
        "}),
        107
    );
}

#[test]
fn a_void_function_may_end_in_a_statement() {
    assert_eq!(
        eval_int(indoc! {"
            g = 0
            set(x: int) is
                ::g = x
            set(5)
            g
        "}),
        5
    );
}

#[test]
fn an_early_return_in_a_callee_does_not_end_the_caller() {
    assert_eq!(
        eval_int(indoc! {"
            clamp(n: int): int is
                if n > 10
                    return 10
                n
            total(): int is
                t = 0
                for i in 8..12
                    t += clamp(i)
                t
            total()
        "}),
        8 + 9 + 10 + 10 + 10
    );
}

#[test]
fn a_function_returns_strings_from_several_places() {
    let src = indoc! {"
        describe(n: int): str is
            name = str(n)
            if n < 0
                return 'negative'
            elif n == 0
                return name
            label = name + '!'
            for i in 1..n
                if i == 3
                    return label + label
            label
    "};
    assert_eq!(eval_str(&format!("{src}describe(-1)")), "negative");
    assert_eq!(eval_str(&format!("{src}describe(0)")), "0");
    assert_eq!(eval_str(&format!("{src}describe(5)")), "5!5!");
    assert_eq!(eval_str(&format!("{src}describe(2)")), "2!");
}

#[test]
fn code_after_a_return_does_not_run() {
    assert_eq!(
        eval_int(indoc! {"
            g = 0
            f(): int is
                return 1
                ::g = 99
                2
            f() + g
        "}),
        1
    );
}
