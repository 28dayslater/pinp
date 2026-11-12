// SPDX-License-Identifier: MIT

//! End-to-end `break` and `continue` (0019): leaving a loop or a pass, in every loop kind, through
//! labels across nested loops, and over matrices.

mod common;
use common::{eval_int, eval_str};
use indoc::indoc;

#[test]
fn break_leaves_a_while_loop() {
    assert_eq!(
        eval_int(indoc! {"
            i = 0
            while true
                i += 1
                if i == 5
                    break
            i
        "}),
        5
    );
}

#[test]
fn continue_skips_the_rest_of_a_while_pass() {
    assert_eq!(
        eval_int(indoc! {"
            i = 0
            odd = 0
            while i < 10
                i += 1
                if i mod 2 == 0
                    continue
                odd += i
            odd
        "}),
        1 + 3 + 5 + 7 + 9
    );
}

#[test]
fn break_leaves_a_loop_until() {
    assert_eq!(
        eval_int(indoc! {"
            i = 0
            loop
                i += 1
                if i == 4
                    break
            until i > 100
            i
        "}),
        4
    );
}

#[test]
fn continue_in_a_loop_until_still_tests_the_condition() {
    // Were `continue` to jump to the top of the body instead, this would never end.
    assert_eq!(
        eval_int(indoc! {"
            i = 0
            skipped = 0
            loop
                i += 1
                if i > 3
                    skipped += 1
                    continue
            until i == 6
            i * 10 + skipped
        "}),
        63
    );
}

#[test]
fn continue_in_a_loop_while_still_tests_the_condition() {
    assert_eq!(
        eval_int(indoc! {"
            i = 0
            loop
                i += 1
                continue
            while i < 7
            i
        "}),
        7
    );
}

#[test]
fn break_leaves_a_counted_for() {
    assert_eq!(
        eval_int(indoc! {"
            last = 0
            for i in 1..100
                if i * i > 50
                    break
                last = i
            last
        "}),
        7
    );
}

#[test]
fn continue_in_a_counted_for_still_steps() {
    assert_eq!(
        eval_int(indoc! {"
            t = 0
            for i in 1..10
                if i mod 3 != 0
                    continue
                t += i
            t
        "}),
        3 + 6 + 9
    );
}

#[test]
fn continue_in_a_counted_for_with_a_step_and_a_downward_range() {
    assert_eq!(
        eval_int(indoc! {"
            t = 0
            for i in 20..>0:-4
                if i == 12
                    continue
                t += i
            t
        "}),
        20 + 16 + 8 + 4
    );
}

#[test]
fn break_and_continue_in_an_array_for() {
    assert_eq!(
        eval_int(indoc! {"
            t = 0
            for i, v in [5, -1, 7, 0, 9]
                if v < 0
                    continue
                if v == 0
                    break
                t += i * 100 + v
            t
        "}),
        5 + (200 + 7)
    );
}

#[test]
fn break_leaves_a_matrix_for_entirely() {
    assert_eq!(
        eval_int(indoc! {"
            seen = 0
            for v in [1, 2, 3; 4, 5, 6; 7, 8, 9]
                if v == 5
                    break
                seen = seen * 10 + v
            seen
        "}),
        1234
    );
}

#[test]
fn continue_in_a_matrix_for_moves_to_the_next_element_also_across_a_row_end() {
    assert_eq!(
        eval_int(indoc! {"
            seen = 0
            for r, c, v in [1, 2, 3; 4, 5, 6]
                if c == 2
                    continue
                seen = seen * 10 + v
            seen
        "}),
        1245
    );
}

#[test]
fn an_unlabelled_break_leaves_only_the_innermost_loop() {
    assert_eq!(
        eval_int(indoc! {"
            pairs = 0
            for i in 1..4
                for j in 1..4
                    if j > i
                        break
                    pairs += 1
            pairs
        "}),
        1 + 2 + 3 + 4
    );
}

#[test]
fn a_labelled_break_leaves_every_loop_up_to_its_label() {
    assert_eq!(
        eval_int(indoc! {"
            found = 0
            for i in 1..9 @rows
                for j in 1..9
                    if i * j == 42
                        found = i * 10 + j
                        break @rows
            found
        "}),
        67
    );
}

#[test]
fn a_labelled_continue_resumes_the_outer_loops_next_pass() {
    assert_eq!(
        eval_int(indoc! {"
            t = 0
            for i in 1..4 @outer
                for j in 1..4
                    if j > 2
                        continue @outer
                    t += 1
                t += 100
            t
        "}),
        4 * 2,
        "every inner loop is left at j = 3, so the outer body's tail never runs"
    );
}

#[test]
fn a_labelled_continue_to_a_loop_until_tests_its_condition() {
    assert_eq!(
        eval_int(indoc! {"
            passes = 0
            loop @outer
                passes += 1
                for j in 1..3
                    continue @outer
                passes += 100
            until passes >= 5
            passes
        "}),
        5
    );
}

#[test]
fn a_labelled_exit_crosses_a_matrix_for_and_reaches_one() {
    assert_eq!(
        eval_int(indoc! {"
            hits = 0
            for k in 1..3 @outer
                for v in [1, 2; 3, 4]
                    if v == k + 1
                        continue @outer
                    hits += 1
            hits
        "}),
        1 + 2 + 3
    );
    assert_eq!(
        eval_int(indoc! {"
            seen = 0
            for v in [1, 2; 3, 4] @cells
                i = 0
                while true
                    i += 1
                    if v == 3
                        break @cells
                    if i == 2
                        continue @cells
                seen = 999
            seen
        "}),
        0,
        "the tail after the while never runs"
    );
}

#[test]
fn three_labelled_loops_each_reachable_from_the_innermost() {
    let src = indoc! {"
        count = 0
        for a in 1..3 @first
            for b in 1..3 @second
                for c in 1..3
                    count += 1
                    if c == 2
                        EXIT
            count += 1000
        count
    "};
    // break: each innermost loop runs twice; `@second`'s body after it does too.
    assert_eq!(
        eval_int(&src.replace("EXIT", "break")),
        3 * 3 * 2 + 3 * 1000
    );
    assert_eq!(
        eval_int(&src.replace("EXIT", "break @second")),
        3 * 2 + 3 * 1000
    );
    assert_eq!(eval_int(&src.replace("EXIT", "break @first")), 2);
    assert_eq!(eval_int(&src.replace("EXIT", "continue @first")), 3 * 2);
}

#[test]
fn sibling_loops_may_share_a_label() {
    assert_eq!(
        eval_int(indoc! {"
            t = 0
            for i in 1..5 @scan
                if i == 3
                    break @scan
                t += i
            for i in 1..5 @scan
                if i == 2
                    continue @scan
                t += i * 10
            t
        "}),
        3 + 130
    );
}

#[test]
fn break_and_continue_inside_functions() {
    assert_eq!(
        eval_int(indoc! {"
            first_multiple(n: int, of: int): int is
                found = -1
                for i in 1..n
                    if i mod of != 0
                        continue
                    found = i
                    break
                found
            first_multiple(20, 7) * 100 + first_multiple(5, 7)
        "}),
        700 - 1
    );
}

#[test]
fn a_break_inside_a_loop_inside_a_called_function_does_not_end_the_callers_loop() {
    assert_eq!(
        eval_int(indoc! {"
            inner(): int is
                n = 0
                while true
                    n += 1
                    if n == 3
                        break
                n
            t = 0
            for i in 1..4
                t += inner()
            t
        "}),
        12
    );
}

#[test]
fn return_from_inside_a_labelled_loop_still_leaves_the_function() {
    assert_eq!(
        eval_int(indoc! {"
            find(): int is
                for i in 1..9 @rows
                    for j in 1..9
                        if i * j == 12
                            return i * 10 + j
                        if j > i
                            continue @rows
                -1
            find()
        "}),
        34
    );
}

#[test]
fn a_loop_whose_every_pass_breaks_runs_its_body_once() {
    assert_eq!(
        eval_int(indoc! {"
            passes = 0
            for i in 1..10
                passes += 1
                break
            passes
        "}),
        1
    );
}

#[test]
fn strings_built_before_a_break_survive_it() {
    assert_eq!(
        eval_str(indoc! {"
            joined = ''
            for i in 1..9
                if i == 4
                    break
                if i == 2
                    continue
                joined = joined + str(i)
            joined
        "}),
        "13"
    );
}
