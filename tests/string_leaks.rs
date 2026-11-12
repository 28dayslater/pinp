// SPDX-License-Identifier: MIT

//! The `str` freeing model, checked against the runtime's own bookkeeping: every heap byte a
//! program allocates must be released by the time the run is over, and released exactly once.
//!
//! Two things make these tests meaningful:
//!
//! * **Every literal here is longer than 15 bytes.** A shorter string lives inline in its 16-byte
//!   descriptor and never touches the allocator, so a leak of one is invisible to the counters — a
//!   suite built on `'a' + 'b'` would pass no matter how broken the freeing model was.
//! * **No arrays.** Array storage is deliberately never freed (out of scope, future work), so a
//!   program that builds one can never balance.
//!
//! The counters catch both failure directions: storage that is never released leaves
//! `allocation_count` ahead of `free_count`, and storage released twice pushes `free_count` ahead
//! (if mimalloc does not abort on the double free first). Each test also asserts the program's
//! *value*, so a premature free that corrupts live content fails here too. The frees are placed in
//! the IR (0017-F).

mod common;
use common::memory_counts;
use indoc::indoc;
use pinp::codegen::{PinpJit, PinpValue};

/// A literal comfortably past the 15-byte inline maximum, so it must be heap-allocated.
const LONG: &str = "a string that is certainly longer than fifteen bytes";

// TODO(KI-0008): arrays are never freed yet, so no test here may use one; add the array leak
// tests once array ownership lands.

/// Compiles and runs `src`, asserting that it used the heap and gave every byte back, and returns
/// its value.
///
/// The heap-traffic assertion is what stops a test from passing by allocating nothing at all — the
/// reason every program in this file works with strings longer than the inline maximum. The
/// counters are per thread, so tests on other threads do not disturb the measurement; the JIT is
/// dropped before the second reading so nothing the program allocated is still owned by a live
/// handle.
fn run_balanced(src: &str) -> PinpValue {
    let before = memory_counts();

    let value = {
        let jit = PinpJit::new(src).expect("Program failed to compile.");
        jit.run().expect("Program failed to run.")
    };

    let after = memory_counts();
    assert!(
        after.allocation_count > before.allocation_count,
        "expected heap traffic, but nothing was allocated for:\n{src}"
    );
    let allocated = after.allocation_count - before.allocation_count;
    let freed = after.free_count - before.free_count;
    assert_eq!(
        allocated, freed,
        "{allocated} allocation(s) but {freed} free(s) for:\n{src}"
    );
    assert_eq!(
        after.outstanding_bytes, before.outstanding_bytes,
        "outstanding bytes moved from {} to {} for:\n{src}",
        before.outstanding_bytes, after.outstanding_bytes
    );
    value
}

fn as_str(value: PinpValue) -> String {
    match value {
        PinpValue::Str(text) => text,
        other => panic!("Expected Str, got {other:?}."),
    }
}

// --- the basic shapes ------------------------------------------------------------------------

#[test]
fn a_literal_that_is_moved_out_is_freed_by_the_host() {
    // The program's result is moved out to the caller, so its free is the host's — and it must
    // still happen.
    let value = run_balanced(&format!("'{LONG}'"));
    assert_eq!(as_str(value), LONG);
}

#[test]
fn a_discarded_temporary_is_freed() {
    // The concatenation's value is never bound or returned; the statement that produced it owns it
    // and must release it.
    let value = run_balanced(&format!(
        indoc! {"
            '{0}' + '{0}'
            0
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(0));
}

#[test]
fn concat_parts_are_freed_once_joined() {
    // `concat_n` copies its parts, so each part is dead the moment the join returns. Only the
    // result survives, to be moved out.
    let value = run_balanced(&format!("'{0}' + '{0}' + '{0}'", LONG));
    assert_eq!(as_str(value), format!("{LONG}{LONG}{LONG}"));
}

#[test]
fn a_bound_string_is_freed_at_scope_exit() {
    let value = run_balanced(&format!(
        indoc! {"
            s = '{0}' + '{0}'
            s.len
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(2 * LONG.len() as i64));
}

#[test]
fn rebinding_frees_the_previous_value() {
    // The slot's old content is released before the new value takes its place; without that, every
    // rebind would strand the previous string.
    let value = run_balanced(&format!(
        indoc! {"
            s = '{0} first'
            s = '{0} second'
            s = '{0} third'
            s.len
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(LONG.len() as i64 + 6));
}

#[test]
fn compound_concat_assignment_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            s = '{0}'
            s += '{0}'
            s
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("{LONG}{LONG}"));
}

// --- interpolation, conversion, and reads -----------------------------------------------------

#[test]
fn fstring_segments_are_freed() {
    let value = run_balanced(&format!(
        indoc! {"
            s = '{0}'
            n = 42
            f'{{s}} and {{n}} and {{s}}'
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("{LONG} and 42 and {LONG}"));
}

#[test]
fn str_conversion_temporaries_are_freed() {
    // The widest `i64` renders to 20 bytes, past the inline maximum, so this conversion allocates.
    let value = run_balanced(indoc! {"
        s = str(-9223372036854775807 - 1)
        s.len
    "});
    assert_eq!(value, PinpValue::Int(20));
}

#[test]
fn the_object_of_a_len_is_freed() {
    // `.len` consumes a temporary that nothing else will ever see.
    let value = run_balanced(&format!("('{0}' + '{0}').len", LONG));
    assert_eq!(value, PinpValue::Int(2 * LONG.len() as i64));
}

#[test]
fn comparison_operands_are_freed() {
    let value = run_balanced(&format!("('{0}' + 'x') == ('{0}' + 'x')", LONG));
    assert_eq!(value, PinpValue::Bool(true));
}

#[test]
fn a_comparison_inside_a_condition_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            s = '{0}'
            1 if s + 'x' > s else 0
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(1));
}

// --- scopes, loops, and functions -------------------------------------------------------------

#[test]
fn a_loop_body_frees_each_iteration() {
    // The body-local is rebound on every pass; nothing may accumulate across iterations.
    let value = run_balanced(&format!(
        indoc! {"
            total = 0
            for idx in 1..20
                line = '{0}' + str(idx)
                total += line.len
            total
        "},
        LONG
    ));
    let expected: i64 = (1..=20)
        .map(|idx| LONG.len() as i64 + idx.to_string().len() as i64)
        .sum();
    assert_eq!(value, PinpValue::Int(expected));
}

#[test]
fn a_string_grown_in_a_loop_is_balanced() {
    // Each pass frees the previous value of `s`, leaving only the final one alive.
    let value = run_balanced(&format!(
        indoc! {"
            s = '{0}'
            n = 0
            while n < 5
                s = s + 'x'
                n += 1
            s.len
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(LONG.len() as i64 + 5));
}

#[test]
fn function_locals_are_freed_before_returning() {
    let value = run_balanced(&format!(
        indoc! {"
            width(n: int): int is
                head = '{0}'
                body = head + str(n)
                body.len
            width(7)
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(LONG.len() as i64 + 1));
}

#[test]
fn a_returned_string_survives_its_frame() {
    // The result is moved out of the callee: its locals are released, but the value handed back
    // must not be.
    let value = run_balanced(&format!(
        indoc! {"
            build(n: int): str is
                head = '{0}'
                head + str(n)
            build(7)
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("{LONG}7"));
}

#[test]
fn nested_calls_are_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            inner(): str is '{0}' + 'inner'
            outer(): str is inner() + 'outer'
            outer().len
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(LONG.len() as i64 + 10));
}

#[test]
fn a_discarded_call_result_is_freed() {
    let value = run_balanced(&format!(
        indoc! {"
            build(): str is '{0}' + '{0}'
            build()
            0
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(0));
}

#[test]
fn chained_assignment_gives_each_binding_its_own_string() {
    // `a = b = expr` stores one value into two bindings; each must own its own, or the scope would
    // release the same storage twice.
    let value = run_balanced(&format!(
        indoc! {"
            a = b = '{0}'
            a.len + b.len
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(2 * LONG.len() as i64));
}

#[test]
fn a_discarded_expression_inside_a_function_is_freed() {
    let value = run_balanced(&format!(
        indoc! {"
            work(): int is
                '{0}' + '{0}'
                7
            work()
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(7));
}

// --- branches --------------------------------------------------------------------------------

#[test]
fn a_block_result_outlives_the_scope_that_held_it() {
    // `t` belongs to the branch's scope, which is released as the branch ends — so the value the
    // branch yields has to be owned before that happens.
    let value = run_balanced(&format!(
        indoc! {"
            s = '{0}'
            if s.len > 0
                t = s + ' inner'
                t
            else
                s
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("{LONG} inner"));
}

#[test]
fn both_conditional_arms_are_balanced() {
    // One arm hands back a fresh string, the other an existing binding. Whichever is taken, the
    // result is owned by the expression and the untaken arm allocates nothing.
    for (condition, expected) in [("true", format!("{LONG}x")), ("false", LONG.to_string())] {
        let value = run_balanced(&format!(
            indoc! {"
                s = '{0}'
                t = s + 'x' if {1} else s
                t
            "},
            LONG, condition
        ));
        assert_eq!(as_str(value), expected, "condition `{condition}`");
    }
}

#[test]
fn a_branch_that_rebinds_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            pick(n: int): str is
                s = '{0}'
                if n > 100
                    s = '{0}' + ' large'
                s
            pick(1000)
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("{LONG} large"));
}

// --- globals ---------------------------------------------------------------------------------

#[test]
fn a_global_string_is_freed_at_program_end() {
    let value = run_balanced(&format!(
        indoc! {"
            g = '{0}'
            read(): int is ::g.len
            read()
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(LONG.len() as i64));
}

#[test]
fn a_reassigned_global_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            g = '{0}'
            bump(): int is
                ::g = ::g + 'x'
                ::g.len
            bump()
            bump()
            ::g.len
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(LONG.len() as i64 + 2));
}

#[test]
fn a_global_returned_as_the_result_is_balanced() {
    // The program's value is a *borrowed* global: it has to reach the host as something the host
    // can free without disturbing the global's own release.
    let value = run_balanced(&format!(
        indoc! {"
            g = '{0}'
            ::g
        "},
        LONG
    ));
    assert_eq!(as_str(value), LONG);
}

// --- everything at once -----------------------------------------------------------------------

#[test]
fn a_concat_heavy_program_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            label(n: int): str is f'{{n}}: {0}'
            joined = ''
            count = 0
            for idx in 1..10
                line = label(idx) + ' | ' + str(idx * idx)
                if line.len > 0
                    count += 1
                joined = joined + '.'
            width = joined.len
            f'{{count}}/{{width}}: ' + joined
        "},
        LONG
    ));
    assert_eq!(as_str(value), "10/10: ..........");
}

// --- shapes the IR's ownership placement adds (0017-F) -----------------------------------------

#[test]
fn a_global_rebound_inside_a_function_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            g = '{0}'
            set(): int is
                ::g = '{0}' + '{0}'
                0
            set() + set()
            g.len
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(2 * LONG.len() as i64));
}

#[test]
fn a_read_the_next_operand_rebinds_is_copied_first() {
    // `g + f()`: `f` frees the value of `g` the concatenation has already read.
    let value = run_balanced(&format!(
        indoc! {"
            g = 'first {0}'
            f(): str is
                ::g = 'second {0}'
                '!'
            g + f()
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("first {LONG}!"));
}

#[test]
fn if_expression_strings_in_a_loop_are_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            total = 0
            for idx in 1..4
                s = '{0}' if idx > 2 else '{0}' + '{0}'
                total += s.len
            total
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(6 * LONG.len() as i64));
}

#[test]
fn a_comparison_with_a_literal_in_a_loop_condition_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            s = ''
            n = 0
            while s != '{0}{0}'
                s += '{0}'
                n += 1
            n
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(2));
}

#[test]
fn interpolation_in_a_loop_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            n = 0
            for idx in 1..3
                line = f'{0} {{idx}}'
                n += line.len
            n
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(3 * (LONG.len() as i64 + 2)));
}

#[test]
fn a_string_bound_only_on_an_untaken_branch_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            f(flag: bool): int is
                n = 0
                if flag
                    s = '{0}'
                    n = s.len
                n
            f(false) + f(true)
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(LONG.len() as i64));
}

#[test]
fn swapping_strings_is_balanced() {
    let value = run_balanced(&format!(
        indoc! {"
            a, b = 'a {0}', 'b {0}'
            a, b = b, a
            a + b
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("b {LONG}a {LONG}"));
}

// --- return (0018) ---------------------------------------------------------------------------

/// `describe` returns a string from five places: a literal, a parameter, a local built from it, a
/// concatenation, and its final expression — with other string locals live at each return.
fn describe_program(n: i64) -> String {
    format!(
        indoc! {"
            describe(n: int): str is
                name = '{0}'
                spare = name + '{0}'
                if n < 0
                    return '{0} negative'
                elif n == 0
                    return name
                label = name + '!'
                if n == 1
                    return label
                for i in 1..n
                    other = label + spare
                    if i == 3
                        return label + other
                spare
            describe({1})
        "},
        LONG, n
    )
}

#[test]
fn every_kind_of_returned_string_balances() {
    let expected = [
        (-1, format!("{LONG} negative")),
        (0, LONG.to_string()),
        (1, format!("{LONG}!")),
        (2, format!("{LONG}{LONG}")),
        (5, format!("{LONG}!{LONG}!{LONG}{LONG}")),
    ];
    for (n, value) in expected {
        assert_eq!(
            as_str(run_balanced(&describe_program(n))),
            value,
            "for n = {n}"
        );
    }
}

#[test]
fn a_return_from_a_void_function_frees_its_strings() {
    let value = run_balanced(&format!(
        indoc! {"
            g = 0
            note(n: int) is
                s = '{0}' + '{0}'
                if n > 0
                    return
                ::g = s.len
            note(1)
            note(0)
            g
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(2 * LONG.len() as i64));
}

#[test]
fn a_string_returned_from_a_loop_balances_across_calls() {
    let value = run_balanced(&format!(
        indoc! {"
            pick(k: int): str is
                for i in 1..5
                    s = '{0}' + str(i)
                    if i == k
                        return s
                '{0} none'
            a = pick(2)
            b = pick(9)
            a + b
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("{LONG}2{LONG} none"));
}

#[test]
fn a_return_frees_a_string_assigned_later_in_the_loop_on_an_earlier_pass() {
    // `s` is first assigned below the `return`, so it is not yet known when the `return` is
    // desugared — but on the second pass it already holds the first pass's string.
    let value = run_balanced(&format!(
        indoc! {"
            f(k: int): int is
                for i in 1..3
                    if i == k
                        return i
                    s = '{0}' + str(i)
                0
            f(2)
        "},
        LONG
    ));
    assert_eq!(value, PinpValue::Int(2));
}

// --- break and continue (0019) -----------------------------------------------------------------

#[test]
fn strings_assigned_in_a_loop_left_by_break_or_continue_balance() {
    for exit in ["break", "continue", "break @rows", "continue @rows"] {
        let src = format!(
            indoc! {"
                build(n: int): str is
                    kept = '{0}'
                    for i in 1..n @rows
                        for j in 1..2
                            piece = kept + str(i)
                            if i == 2
                                {1}
                            kept = piece
                    kept
                build(4)
            "},
            LONG, exit
        );
        run_balanced(&src);
    }
}

#[test]
fn a_string_overwritten_on_the_pass_after_a_continue_is_freed() {
    let value = run_balanced(&format!(
        indoc! {"
            last = ''
            for i in 1..4
                last = '{0}' + str(i)
                if i < 4
                    continue
            last
        "},
        LONG
    ));
    assert_eq!(as_str(value), format!("{LONG}4"));
}
