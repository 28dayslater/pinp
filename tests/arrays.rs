// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end arrays: literal init, range-init comprehension, index read/write, `.len`,
//! mutation, and bounds-check runtime errors. Requires the `llvm` feature.

mod common;
use common::{eval, eval_bool, eval_int};
use indoc::indoc;
use pinp::codegen::PinpValue;

// --- literal init -------------------------------------------------------------------------

#[test]
fn int_array_literal_round_trips() {
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(10),
            PinpValue::Int(20),
            PinpValue::Int(30),
        ])
    );
}

#[test]
fn float_array_literal_round_trips() {
    assert_eq!(
        eval(indoc! {"
            a = [1.5, 2.5, 3.5]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Float(1.5),
            PinpValue::Float(2.5),
            PinpValue::Float(3.5),
        ])
    );
}

#[test]
fn bool_array_literal_round_trips() {
    assert_eq!(
        eval(indoc! {"
            a = [true, false, true]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Bool(true),
            PinpValue::Bool(false),
            PinpValue::Bool(true),
        ])
    );
}

#[test]
fn int_array_promotes_bool_elements() {
    // A bool literal in an int-typed array context is zero-extended to Int.
    assert_eq!(
        eval(indoc! {"
            a = [1, true, 0]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(1),
            PinpValue::Int(1),
            PinpValue::Int(0),
        ])
    );
}

// --- index read ---------------------------------------------------------------------------

#[test]
fn index_reads_correct_element() {
    assert_eq!(
        eval_int(indoc! {"
            a = [10, 20, 30]
            a[1]
        "}),
        20
    );
}

#[test]
fn index_reads_first_and_last() {
    assert_eq!(
        eval_int(indoc! {"
            a = [7, 8, 9]
            a[0] + a[2]
        "}),
        16
    );
}

// --- .len ---------------------------------------------------------------------------------

#[test]
fn len_returns_element_count() {
    assert_eq!(
        eval_int(indoc! {"
            a = [1, 2, 3, 4, 5]
            a.len
        "}),
        5
    );
}

#[test]
fn len_usable_as_loop_bound() {
    assert_eq!(
        eval_int(indoc! {"
            a = [2, 4, 6, 8]
            total = 0
            for idx in 0..<a.len
                total += a[idx]
            total
        "}),
        20
    );
}

// --- mutation (IndexedAssign) -------------------------------------------------------------

#[test]
fn indexed_assign_updates_element() {
    assert_eq!(
        eval_int(indoc! {"
            a = [1, 2, 3]
            a[1] = 99
            a[1]
        "}),
        99
    );
}

#[test]
fn indexed_assign_does_not_disturb_neighbours() {
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30]
            a[1] = 99
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(10),
            PinpValue::Int(99),
            PinpValue::Int(30),
        ])
    );
}

#[test]
fn indexed_assign_in_loop_fills_array() {
    assert_eq!(
        eval(indoc! {"
            a = [0, 0, 0, 0, 0]
            for idx in 0..<5
                a[idx] = idx * 2
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(0),
            PinpValue::Int(2),
            PinpValue::Int(4),
            PinpValue::Int(6),
            PinpValue::Int(8),
        ])
    );
}

// --- range-init comprehension -------------------------------------------------------------

#[test]
fn comprehension_produces_sequential_ints() {
    assert_eq!(
        eval(indoc! {"
            a = [idx for idx in 1..5]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(1),
            PinpValue::Int(2),
            PinpValue::Int(3),
            PinpValue::Int(4),
            PinpValue::Int(5),
        ])
    );
}

#[test]
fn comprehension_applies_element_expression() {
    assert_eq!(
        eval(indoc! {"
            a = [idx * 2 for idx in 1..4]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(2),
            PinpValue::Int(4),
            PinpValue::Int(6),
            PinpValue::Int(8),
        ])
    );
}

#[test]
fn comprehension_with_exclusive_range() {
    assert_eq!(
        eval(indoc! {"
            a = [idx for idx in 0..<3]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(0),
            PinpValue::Int(1),
            PinpValue::Int(2),
        ])
    );
}

#[test]
fn comprehension_with_float_type_annotation() {
    assert_eq!(
        eval(indoc! {"
            a = [x for x:float in 1..3]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Float(1.0),
            PinpValue::Float(2.0),
            PinpValue::Float(3.0),
        ])
    );
}

#[test]
fn comprehension_len_matches_range_size() {
    assert_eq!(
        eval_int(indoc! {"
            a = [idx for idx in 0..<10]
            a.len
        "}),
        10
    );
}

// --- range-init `[start..stop[:step]]` ----------------------------------------------------

#[test]
fn range_init_inclusive_produces_sequence() {
    assert_eq!(
        eval(indoc! {"
            a = [1..5]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(1),
            PinpValue::Int(2),
            PinpValue::Int(3),
            PinpValue::Int(4),
            PinpValue::Int(5),
        ])
    );
}

#[test]
fn range_init_exclusive_drops_stop() {
    assert_eq!(
        eval(indoc! {"
            a = [0..<4]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(0),
            PinpValue::Int(1),
            PinpValue::Int(2),
            PinpValue::Int(3),
        ])
    );
}

#[test]
fn range_init_with_step() {
    assert_eq!(
        eval(indoc! {"
            a = [0..10:2]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(0),
            PinpValue::Int(2),
            PinpValue::Int(4),
            PinpValue::Int(6),
            PinpValue::Int(8),
            PinpValue::Int(10),
        ])
    );
}

#[test]
fn range_init_descending() {
    assert_eq!(
        eval(indoc! {"
            a = [5..1]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(5),
            PinpValue::Int(4),
            PinpValue::Int(3),
            PinpValue::Int(2),
            PinpValue::Int(1),
        ])
    );
}

#[test]
fn range_init_len_is_correct() {
    assert_eq!(
        eval_int(indoc! {"
            a = [1..10]
            a.len
        "}),
        10
    );
}

// --- error cases --------------------------------------------------------------------------

#[test]
fn empty_array_literal_is_parse_error() {
    let result = pinp::codegen::PinpJit::eval("[]");
    assert!(
        result.unwrap_err().contains("empty"),
        "expected empty-literal parse error"
    );
}

#[test]
fn bool_var_type_in_comprehension_is_sema_error() {
    let result = pinp::codegen::PinpJit::eval("[x for x:bool in 0..3]");
    assert!(
        result.unwrap_err().contains("bool"),
        "expected bool-annotation sema error"
    );
}

#[test]
fn empty_comprehension_range_is_sema_error() {
    let result = pinp::codegen::PinpJit::eval("[x for x in 1..<1]");
    assert!(
        result.unwrap_err().contains("empty"),
        "expected empty-range sema error"
    );
}

// --- bounds errors ------------------------------------------------------------------------

#[test]
fn index_too_high_is_runtime_error() {
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        a = [1, 2, 3]
        a[3]
    "});
    assert!(
        result.unwrap_err().contains("Array index out of bounds"),
        "expected bounds error"
    );
}

#[test]
fn negative_index_is_runtime_error() {
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        a = [1, 2, 3]
        a[-1]
    "});
    assert!(
        result.unwrap_err().contains("Array index out of bounds"),
        "expected bounds error"
    );
}

#[test]
fn indexed_assign_out_of_bounds_is_runtime_error() {
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        a = [1, 2, 3]
        a[5] = 99
        a[0]
    "});
    assert!(
        result.unwrap_err().contains("Array index out of bounds"),
        "expected bounds error"
    );
}

// --- array in a function ------------------------------------------------------------------

#[test]
fn array_passed_through_function_result() {
    // A function that returns an element of a locally constructed array.
    assert_eq!(
        eval_int(indoc! {"
            pick(n:int): int is
                a = [100, 200, 300]
                a[n]
            pick(2)
        "}),
        300
    );
}

#[test]
fn array_len_in_condition() {
    assert!(eval_bool(indoc! {"
            a = [1, 2, 3]
            a.len == 3
        "}));
}

// --- index boundaries ----------------------------------------------------------------

#[test]
fn index_at_last_valid_position_succeeds() {
    // n-1 is the last valid index; must not fault.
    assert_eq!(
        eval_int(indoc! {"
            a = [10, 20, 30]
            a[2]
        "}),
        30
    );
}

#[test]
fn indexed_assign_at_last_position_succeeds() {
    assert_eq!(
        eval_int(indoc! {"
            a = [0, 0, 0]
            a[2] = 77
            a[2]
        "}),
        77
    );
}

#[test]
fn index_at_exactly_len_is_runtime_error() {
    // a[3] on a 3-element array: the index equals len, which is out of bounds.
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        a = [1, 2, 3]
        a[3]
    "});
    assert!(
        result.unwrap_err().contains("out of bounds"),
        "expected bounds error"
    );
}

// --- float array mutation ------------------------------------------------------------

#[test]
fn float_array_indexed_assign_accepts_int() {
    // Int promotes to Float on store; the retrieved value should be the float counterpart.
    assert_eq!(
        eval(indoc! {"
            a = [1.0, 2.0, 3.0]
            a[0] = 5
            a[0]
        "}),
        PinpValue::Float(5.0)
    );
}

// --- two independent arrays ----------------------------------------------------------

#[test]
fn two_arrays_share_no_storage() {
    assert_eq!(
        eval_int(indoc! {"
            a = [1, 2, 3]
            b = [4, 5, 6]
            a[1] + b[1]
        "}),
        7
    );
}

#[test]
fn mutating_one_array_does_not_affect_another() {
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30]
            b = [10, 20, 30]
            a[1] = 99
            b
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(10),
            PinpValue::Int(20),
            PinpValue::Int(30),
        ])
    );
}

// --- array of size one ---------------------------------------------------------------

#[test]
fn single_element_array_round_trips() {
    assert_eq!(eval("[42]"), PinpValue::Array(vec![PinpValue::Int(42)]));
}

#[test]
fn single_element_array_indexed() {
    assert_eq!(eval_int("[42][0]"), 42);
}

// --- stepped comprehension -----------------------------------------------------------

#[test]
fn comprehension_with_stepped_range() {
    // 0..9:3 is inclusive with step 3: [0, 3, 6, 9] — 4 elements.
    assert_eq!(
        eval(indoc! {"
            a = [x for x in 0..9:3]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(0),
            PinpValue::Int(3),
            PinpValue::Int(6),
            PinpValue::Int(9),
        ])
    );
}

#[test]
fn comprehension_squared_element_expression() {
    assert_eq!(
        eval(indoc! {"
            a = [x * x for x in 1..4]
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(1),
            PinpValue::Int(4),
            PinpValue::Int(9),
            PinpValue::Int(16),
        ])
    );
}

// --- range-init edge cases -----------------------------------------------------------

#[test]
fn range_init_single_element_equal_bounds() {
    // Inclusive range start == stop yields exactly one element.
    assert_eq!(eval("[3..3]"), PinpValue::Array(vec![PinpValue::Int(3)]));
}

#[test]
fn range_init_descending_exclusive() {
    // [5..>1]: DownExclusive, excludes 1, so [5, 4, 3, 2].
    assert_eq!(
        eval("[5..>1]"),
        PinpValue::Array(vec![
            PinpValue::Int(5),
            PinpValue::Int(4),
            PinpValue::Int(3),
            PinpValue::Int(2),
        ])
    );
}

// --- chained/nested index operations -------------------------------------------------

#[test]
fn index_using_another_arrays_element() {
    // b[a[1]]: a[1] = 1, so b[1] = 20.
    assert_eq!(
        eval_int(indoc! {"
            a = [0, 1, 2]
            b = [10, 20, 30]
            b[a[1]]
        "}),
        20
    );
}

#[test]
fn index_directly_on_literal_array() {
    // [10, 20, 30][1] — no intermediate binding.
    assert_eq!(eval_int("[10, 20, 30][1]"), 20);
}

#[test]
fn index_directly_on_comprehension() {
    // [x for x in 1..5][2] — element at index 2 is 3.
    assert_eq!(eval_int("[x for x in 1..5][2]"), 3);
}

// --- empty for-loop over exclusive-equal-bound range ---------------------------------

#[test]
fn for_loop_over_empty_exclusive_range_body_never_runs() {
    // 1..<1 is valid but yields 0 iterations; `total` stays 0.
    assert_eq!(
        eval_int(indoc! {"
            total = 0
            for x in 1..<1
                total += x
            total
        "}),
        0
    );
}

// --- slice access --------------------------------------------------------------------

#[test]
fn slice_returns_int_subarray() {
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30, 40, 50]
            a[1..3]
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(20),
            PinpValue::Int(30),
            PinpValue::Int(40)
        ])
    );
}

#[test]
fn exclusive_slice_excludes_stop() {
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30, 40, 50]
            a[1..<4]
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(20),
            PinpValue::Int(30),
            PinpValue::Int(40)
        ])
    );
}

#[test]
fn slice_float_array() {
    assert_eq!(
        eval(indoc! {"
            a = [1.5, 2.5, 3.5, 4.5]
            a[1..2]
        "}),
        PinpValue::Array(vec![PinpValue::Float(2.5), PinpValue::Float(3.5)])
    );
}

#[test]
fn slice_bool_array() {
    assert_eq!(
        eval(indoc! {"
            a = [true, false, true, false]
            a[1..2]
        "}),
        PinpValue::Array(vec![PinpValue::Bool(false), PinpValue::Bool(true)])
    );
}

#[test]
fn slice_single_element() {
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30]
            a[1..1]
        "}),
        PinpValue::Array(vec![PinpValue::Int(20)])
    );
}

#[test]
fn slice_len_matches_range_count() {
    assert_eq!(
        eval_int(indoc! {"
            a = [10, 20, 30, 40, 50]
            a[1..3].len
        "}),
        3
    );
}

#[test]
fn slice_is_a_copy_not_a_view() {
    // Mutating the source after slicing must not change the slice's content.
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30, 40, 50]
            b = a[1..3]
            a[2] = 99
            b
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(20),
            PinpValue::Int(30),
            PinpValue::Int(40)
        ])
    );
}

#[test]
fn slice_from_first_element() {
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30, 40, 50]
            a[0..2]
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(10),
            PinpValue::Int(20),
            PinpValue::Int(30)
        ])
    );
}

#[test]
fn exclusive_slice_to_end() {
    assert_eq!(
        eval(indoc! {"
            a = [10, 20, 30]
            a[0..<3]
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(10),
            PinpValue::Int(20),
            PinpValue::Int(30)
        ])
    );
}

#[test]
fn slice_result_is_indexable() {
    assert_eq!(
        eval_int(indoc! {"
        a = [10, 20, 30, 40, 50]
        a[1..3][1]
    "}),
        30
    );
}

// --- slice assign --------------------------------------------------------------------

#[test]
fn slice_assign_fills_range_with_scalar() {
    assert_eq!(
        eval(indoc! {"
            a = [0, 0, 0, 0, 0]
            a[1..3] = 7
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(0),
            PinpValue::Int(7),
            PinpValue::Int(7),
            PinpValue::Int(7),
            PinpValue::Int(0),
        ])
    );
}

#[test]
fn slice_assign_does_not_disturb_neighbours() {
    assert_eq!(
        eval(indoc! {"
            a = [1, 2, 3, 4, 5]
            a[2..2] = 99
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(1),
            PinpValue::Int(2),
            PinpValue::Int(99),
            PinpValue::Int(4),
            PinpValue::Int(5),
        ])
    );
}

#[test]
fn slice_assign_int_to_float_array_promotes() {
    assert_eq!(
        eval(indoc! {"
            a = [1.0, 2.0, 3.0]
            a[0..1] = 9
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Float(9.0),
            PinpValue::Float(9.0),
            PinpValue::Float(3.0),
        ])
    );
}

#[test]
fn slice_assign_whole_array() {
    assert_eq!(
        eval(indoc! {"
            a = [1, 2, 3]
            a[0..2] = 0
            a
        "}),
        PinpValue::Array(vec![
            PinpValue::Int(0),
            PinpValue::Int(0),
            PinpValue::Int(0)
        ])
    );
}

// --- slice sema errors ---------------------------------------------------------------

#[test]
fn slice_variable_bounds_is_sema_error() {
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        a = [1, 2, 3]
        n = 2
        a[0..n]
    "});
    assert!(
        result.is_err(),
        "expected sema error for variable slice bounds"
    );
}

#[test]
fn slice_with_step_is_sema_error() {
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        a = [1, 2, 3, 4, 5]
        a[0..4:2]
    "});
    assert!(result.is_err(), "expected sema error for slice with step");
}

#[test]
fn slice_oob_is_sema_error() {
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        a = [1, 2, 3]
        a[0..3]
    "});
    assert!(
        result.is_err(),
        "expected sema error for out-of-bounds slice"
    );
}

#[test]
fn slice_descending_is_sema_error() {
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        a = [1, 2, 3, 4, 5]
        a[4..>1]
    "});
    assert!(result.is_err(), "expected sema error for descending slice");
}

// --- sema errors for empty range-init ------------------------------------------------

#[test]
fn range_init_empty_exclusive_is_sema_error() {
    let result = pinp::codegen::PinpJit::eval("[1..<1]");
    assert!(
        result.unwrap_err().contains("empty"),
        "expected empty range-init sema error"
    );
}

#[test]
fn range_init_variable_bounds_is_sema_error() {
    let result = pinp::codegen::PinpJit::eval(indoc! {"
        n = 5
        [n..10]
    "});
    assert!(
        result.unwrap_err().contains("literal"),
        "expected non-literal bound sema error"
    );
}
