// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end evidence that comments and blank lines are ignored: a program peppered with both
//! computes the same value as its stripped-down equivalent. Requires the `llvm` feature.

mod common;
use common::eval_int;
use indoc::indoc;

#[test]
fn comments_and_blank_lines_do_not_change_the_result() {
    let value = eval_int(indoc! {"
        # Sum 1..5, with notes and breathing room.
        total = 0

        for value in 1..5

            total += value      # running total

        total                   # the result
    "});
    assert_eq!(value, 15);
}

#[test]
fn blank_and_comment_lines_inside_a_function_body() {
    let value = eval_int(indoc! {"
        # A function that doubles its argument.
        twice(n: int): int is

            # nothing surprising here
            n + n

        twice(21)
    "});
    assert_eq!(value, 42);
}
