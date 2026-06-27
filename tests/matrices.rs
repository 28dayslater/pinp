// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end matrix tests: literal init, element read/write, slicing, built-in members,
//! iteration, identity, and PinpValue::Matrix round-trips. Requires the `llvm` feature.
//!
//! Tests are ordered by the codegen step that makes them pass. Steps 13–19 light them up
//! progressively; until then they fail on a `todo!()` in the still-unimplemented path.

mod common;
use common::{eval_array, eval_float, eval_int, eval_matrix};
use indoc::indoc;
use pinp::codegen::{PinpJit, PinpValue};

// -----------------------------------------------------------------------------------------
// Step 13 — matrix literal allocation and element stores
// -----------------------------------------------------------------------------------------

#[test]
fn int_matrix_literal_round_trips() {
    let (rows, cols, elements) = eval_matrix("[1, 2, 3; 4, 5, 6]");
    assert_eq!(rows, 2);
    assert_eq!(cols, 3);
    assert_eq!(
        elements,
        vec![
            PinpValue::Int(1),
            PinpValue::Int(2),
            PinpValue::Int(3),
            PinpValue::Int(4),
            PinpValue::Int(5),
            PinpValue::Int(6),
        ]
    );
}

#[test]
fn float_matrix_literal_round_trips() {
    let (rows, cols, elements) = eval_matrix("[1.0, 2.0; 3.0, 4.0]");
    assert_eq!(rows, 2);
    assert_eq!(cols, 2);
    assert_eq!(
        elements,
        vec![
            PinpValue::Float(1.0),
            PinpValue::Float(2.0),
            PinpValue::Float(3.0),
            PinpValue::Float(4.0),
        ]
    );
}

#[test]
fn bool_matrix_literal_round_trips() {
    let (rows, cols, elements) = eval_matrix("[true, false; false, true]");
    assert_eq!(rows, 2);
    assert_eq!(cols, 2);
    assert_eq!(
        elements,
        vec![
            PinpValue::Bool(true),
            PinpValue::Bool(false),
            PinpValue::Bool(false),
            PinpValue::Bool(true),
        ]
    );
}

#[test]
fn mixed_int_float_literal_promotes_to_float() {
    let (rows, cols, elements) = eval_matrix("[1, 2; 3, 4.0]");
    assert_eq!(rows, 2);
    assert_eq!(cols, 2);
    assert_eq!(
        elements,
        vec![
            PinpValue::Float(1.0),
            PinpValue::Float(2.0),
            PinpValue::Float(3.0),
            PinpValue::Float(4.0),
        ]
    );
}

#[test]
fn column_vector_literal_round_trips() {
    let (rows, cols, elements) = eval_matrix("[10; 20; 30]");
    assert_eq!(rows, 3);
    assert_eq!(cols, 1);
    assert_eq!(
        elements,
        vec![PinpValue::Int(10), PinpValue::Int(20), PinpValue::Int(30)]
    );
}

#[test]
#[allow(clippy::identity_op)] // 1 * 4 + 2 spells out the row-major formula r*cols+c intentionally
fn large_matrix_element_count() {
    // 3×4 = 12 elements in row-major order.
    let (rows, cols, elements) = eval_matrix("[1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12]");
    assert_eq!(rows, 3);
    assert_eq!(cols, 4);
    assert_eq!(elements.len(), 12);
    assert_eq!(elements[0], PinpValue::Int(1));
    assert_eq!(elements[11], PinpValue::Int(12));
    // Row-major: element at (r, c) is elements[r * cols + c].
    assert_eq!(elements[1 * 4 + 2], PinpValue::Int(7));
}

// -----------------------------------------------------------------------------------------
// Step 14 — identity() built-in
// -----------------------------------------------------------------------------------------

#[test]
fn identity_int_3x3_diagonal_is_one() {
    let (rows, cols, elements) = eval_matrix("identity(3, int)");
    assert_eq!(rows, 3);
    assert_eq!(cols, 3);
    for row in 0..3 {
        for col in 0..3 {
            let expected = if row == col {
                PinpValue::Int(1)
            } else {
                PinpValue::Int(0)
            };
            assert_eq!(elements[row * 3 + col], expected, "at ({row},{col})");
        }
    }
}

#[test]
fn identity_float_3x3_diagonal_is_one() {
    let (rows, cols, elements) = eval_matrix("identity(3, float)");
    assert_eq!(rows, 3);
    assert_eq!(cols, 3);
    for row in 0..3 {
        for col in 0..3 {
            let expected = if row == col {
                PinpValue::Float(1.0)
            } else {
                PinpValue::Float(0.0)
            };
            assert_eq!(elements[row * 3 + col], expected, "at ({row},{col})");
        }
    }
}

#[test]
fn identity_minimum_size_2x2() {
    let (rows, cols, elements) = eval_matrix("identity(2, int)");
    assert_eq!(rows, 2);
    assert_eq!(cols, 2);
    assert_eq!(
        elements,
        vec![
            PinpValue::Int(1),
            PinpValue::Int(0),
            PinpValue::Int(0),
            PinpValue::Int(1)
        ]
    );
}

#[test]
fn identity_4x4_float_element_count() {
    let (rows, cols, elements) = eval_matrix("identity(4, float)");
    assert_eq!(rows, 4);
    assert_eq!(cols, 4);
    assert_eq!(elements.len(), 16);
    // Spot-check: (0,0), (1,1) are 1.0; (0,1), (1,0) are 0.0
    assert_eq!(elements[0], PinpValue::Float(1.0));
    assert_eq!(elements[1], PinpValue::Float(0.0));
    assert_eq!(elements[4], PinpValue::Float(0.0));
    assert_eq!(elements[5], PinpValue::Float(1.0));
}

#[test]
fn index2d_scalar_read_int() {
    assert_eq!(
        eval_int(indoc! {"
        mat = [10, 20, 30; 40, 50, 60]
        mat[0, 1]
    "}),
        20
    );
    assert_eq!(
        eval_int(indoc! {"
        mat = [10, 20, 30; 40, 50, 60]
        mat[1, 2]
    "}),
        60
    );
}

#[test]
fn index2d_scalar_read_float() {
    assert_eq!(
        eval_float(indoc! {"
        mat = [1.0, 2.0; 3.0, 4.0]
        mat[1, 0]
    "}),
        3.0
    );
}

#[test]
fn index2d_row_oob_is_runtime_error() {
    let error = PinpJit::eval(indoc! {"
        mat = [1, 2; 3, 4]
        mat[5, 0]
    "})
    .unwrap_err();
    assert!(error.contains("out of bounds"), "got: {error}");
}

#[test]
fn index2d_col_oob_is_runtime_error() {
    let error = PinpJit::eval(indoc! {"
        mat = [1, 2; 3, 4]
        mat[0, 5]
    "})
    .unwrap_err();
    assert!(error.contains("out of bounds"), "got: {error}");
}

#[test]
fn index2d_negative_row_oob_is_runtime_error() {
    // -3 on a 2-row matrix: effective = -3 + 2 = -1 — still OOB.
    let error = PinpJit::eval(indoc! {"
        mat = [1, 2; 3, 4]
        i = 0 - 3
        mat[i, 0]
    "})
    .unwrap_err();
    assert!(error.contains("out of bounds"), "got: {error}");
}

#[test]
fn row_slice_returns_correct_elements() {
    // mat[0, 1..2] on a 2×3 int matrix → row 0, cols 1 and 2 (inclusive)
    let elements = eval_array(indoc! {"
        mat = [10, 20, 30; 40, 50, 60]
        mat[0, 1..2]
    "});
    assert_eq!(elements, vec![PinpValue::Int(20), PinpValue::Int(30)]);
}

#[test]
fn col_slice_returns_correct_elements() {
    // mat[0..1, 0] on a 3×2 int matrix → rows 0 and 1 (inclusive), col 0
    let elements = eval_array(indoc! {"
        mat = [10, 20; 30, 40; 50, 60]
        mat[0..1, 0]
    "});
    assert_eq!(elements, vec![PinpValue::Int(10), PinpValue::Int(30)]);
}

#[test]
fn full_extent_row_gives_full_row() {
    // mat[1, :] → all columns of row 1
    let elements = eval_array(indoc! {"
        mat = [10, 20, 30; 40, 50, 60]
        mat[1, :]
    "});
    assert_eq!(
        elements,
        vec![PinpValue::Int(40), PinpValue::Int(50), PinpValue::Int(60)]
    );
}

#[test]
fn full_extent_col_gives_full_col() {
    // mat[:, 1] → all rows at column 1
    let elements = eval_array(indoc! {"
        mat = [10, 20; 30, 40; 50, 60]
        mat[:, 1]
    "});
    assert_eq!(
        elements,
        vec![PinpValue::Int(20), PinpValue::Int(40), PinpValue::Int(60)]
    );
}

#[test]
fn submatrix_slice_returns_correct_shape_and_elements() {
    // mat[0..1, 1..2] on 3×3 → 2×2 submatrix
    let (rows, cols, elements) = eval_matrix(indoc! {"
        mat = [1, 2, 3; 4, 5, 6; 7, 8, 9]
        mat[0..1, 1..2]
    "});
    assert_eq!(rows, 2);
    assert_eq!(cols, 2);
    assert_eq!(
        elements,
        vec![
            PinpValue::Int(2),
            PinpValue::Int(3),
            PinpValue::Int(5),
            PinpValue::Int(6)
        ]
    );
}

#[test]
fn full_extent_both_dims_returns_same_matrix() {
    // mat[:, :] → copy of the whole matrix
    let (rows, cols, elements) = eval_matrix(indoc! {"
        mat = [1, 2; 3, 4]
        mat[:, :]
    "});
    assert_eq!(rows, 2);
    assert_eq!(cols, 2);
    assert_eq!(
        elements,
        vec![
            PinpValue::Int(1),
            PinpValue::Int(2),
            PinpValue::Int(3),
            PinpValue::Int(4)
        ]
    );
}

// -----------------------------------------------------------------------------------------
// Step 16 — built-in members (ndim, rows, cols, len)
// -----------------------------------------------------------------------------------------

#[test]
fn identity_result_member_rows_cols() {
    assert_eq!(eval_int("identity(5, int).rows"), 5);
    assert_eq!(eval_int("identity(5, int).cols"), 5);
}

#[test]
fn ndim_on_1d_array_is_one() {
    assert_eq!(eval_int("[1, 2, 3].ndim"), 1);
}

#[test]
fn ndim_on_2d_matrix_is_two() {
    assert_eq!(eval_int("[1, 2; 3, 4].ndim"), 2);
}

#[test]
fn len_on_matrix_is_rows_times_cols() {
    // 2×3 → len == 6
    assert_eq!(eval_int("[1, 2, 3; 4, 5, 6].len"), 6);
}

// -----------------------------------------------------------------------------------------
// Step 17 — for-array iteration
// -----------------------------------------------------------------------------------------

#[test]
fn for_val_in_1d_array_visits_all_elements() {
    // sum = 1+2+3+4+5 = 15
    assert_eq!(
        eval_int(indoc! {"
            arr = [1, 2, 3, 4, 5]
            total = 0
            for val in arr
                total += val
            total
        "}),
        15
    );
}

#[test]
fn for_idx_val_in_1d_array_gives_correct_index_and_value() {
    // weighted sum: 0*10 + 1*20 + 2*30 = 0 + 20 + 60 = 80
    assert_eq!(
        eval_int(indoc! {"
            arr = [10, 20, 30]
            total = 0
            for idx, val in arr
                total += idx * val
            total
        "}),
        80
    );
}

#[test]
fn for_val_in_matrix_visits_elements_row_major() {
    // 2×3 matrix; row-major sum = 1+2+3+4+5+6 = 21
    assert_eq!(
        eval_int(indoc! {"
            mat = [1, 2, 3; 4, 5, 6]
            total = 0
            for val in mat
                total += val
            total
        "}),
        21
    );
}

#[test]
fn for_row_col_val_in_matrix_gives_correct_indices() {
    // For a 2×3 matrix: sum of (row*10 + col) across all 6 elements.
    // (0*10+0)+(0*10+1)+(0*10+2)+(1*10+0)+(1*10+1)+(1*10+2) = 0+1+2+10+11+12 = 36
    assert_eq!(
        eval_int(indoc! {"
            mat = [1, 2, 3; 4, 5, 6]
            total = 0
            for row, col, val in mat
                total += row * 10 + col
            total
        "}),
        36
    );
}

#[test]
fn for_underscore_col_binder_is_silently_discarded() {
    // `_` in the col position: row and val are still accessible.
    // sum of row+val for [10,20; 30,40]: (0+10)+(0+20)+(1+30)+(1+40) = 10+20+31+41 = 102
    assert_eq!(
        eval_int(indoc! {"
            mat = [10, 20; 30, 40]
            total = 0
            for row, _, val in mat
                total += row + val
            total
        "}),
        102
    );
}

// -----------------------------------------------------------------------------------------
// Step 18 — IndexedAssign2D (mat[row, col] = value)
// -----------------------------------------------------------------------------------------

#[test]
fn indexed_assign2d_int_element_write_and_read() {
    assert_eq!(
        eval_int(indoc! {"
            mat = [1, 2, 3; 4, 5, 6]
            mat[0, 1] = 99
            mat[0, 1]
        "}),
        99
    );
}

#[test]
fn indexed_assign2d_float_element_write_and_read() {
    assert_eq!(
        eval_float(indoc! {"
            mat = [1.0, 2.0; 3.0, 4.0]
            mat[1, 0] = 7.5
            mat[1, 0]
        "}),
        7.5
    );
}

#[test]
fn indexed_assign2d_promotes_int_to_float_element() {
    // Writing an int literal to a float matrix element should promote.
    assert_eq!(
        eval_float(indoc! {"
            mat = [1.0, 2.0; 3.0, 4.0]
            mat[0, 0] = 42
            mat[0, 0]
        "}),
        42.0
    );
}

#[test]
fn indexed_assign2d_does_not_disturb_other_elements() {
    // Only mat[1,2] is touched; all others remain at their original values.
    let (rows, cols, elements) = eval_matrix(indoc! {"
        mat = [1, 2, 3; 4, 5, 6]
        mat[1, 2] = 100
        mat
    "});
    assert_eq!(rows, 2);
    assert_eq!(cols, 3);
    assert_eq!(
        elements,
        vec![
            PinpValue::Int(1),
            PinpValue::Int(2),
            PinpValue::Int(3),
            PinpValue::Int(4),
            PinpValue::Int(5),
            PinpValue::Int(100),
        ]
    );
}

#[test]
fn indexed_assign2d_computed_row_col() {
    // Row and col computed at runtime (not literal constants).
    assert_eq!(
        eval_int(indoc! {"
            mat = [10, 20; 30, 40]
            r = 1
            c = 0
            mat[r, c] = 77
            mat[1, 0]
        "}),
        77
    );
}

#[test]
fn indexed_assign2d_multiple_writes_last_wins() {
    assert_eq!(
        eval_int(indoc! {"
            mat = [0, 0; 0, 0]
            mat[0, 0] = 10
            mat[0, 0] = 20
            mat[0, 0]
        "}),
        20
    );
}

#[test]
fn indexed_assign2d_row_oob_is_runtime_error() {
    let error = PinpJit::eval(indoc! {"
        mat = [1, 2; 3, 4]
        mat[5, 0] = 99
    "})
    .unwrap_err();
    assert!(error.contains("out of bounds"), "got: {error}");
}

#[test]
fn indexed_assign2d_col_oob_is_runtime_error() {
    let error = PinpJit::eval(indoc! {"
        mat = [1, 2; 3, 4]
        mat[0, 5] = 99
    "})
    .unwrap_err();
    assert!(error.contains("out of bounds"), "got: {error}");
}

#[test]
fn indexed_assign2d_negative_row_is_runtime_error() {
    // -3 on a 2-row matrix: effective = -3 + 2 = -1 — still OOB.
    let error = PinpJit::eval(indoc! {"
        mat = [1, 2; 3, 4]
        i = 0 - 3
        mat[i, 0] = 99
    "})
    .unwrap_err();
    assert!(error.contains("out of bounds"), "got: {error}");
}

// -----------------------------------------------------------------------------------------
// Step 19 — negative indexing (2D)
// -----------------------------------------------------------------------------------------

#[test]
fn negative_index_2d_reads_corners_and_middle() {
    // 4×4 matrix; test top-left, bottom-right, and a mixed-sign pair.
    //   [  1,  2,  3,  4 ]
    //   [  5,  6,  7,  8 ]
    //   [  9, 10, 11, 12 ]
    //   [ 13, 14, 15, 16 ]
    assert_eq!(
        eval_int(indoc! {"
        mat = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12; 13, 14, 15, 16]
        mat[-4, -4]
    "}),
        1
    ); // top-left = [0,0]
    assert_eq!(
        eval_int(indoc! {"
        mat = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12; 13, 14, 15, 16]
        mat[-1, -1]
    "}),
        16
    ); // bottom-right = [3,3]
    assert_eq!(
        eval_int(indoc! {"
        mat = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12; 13, 14, 15, 16]
        mat[0, -1]
    "}),
        4
    ); // first row, last column = [0,3]
    assert_eq!(
        eval_int(indoc! {"
        mat = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12; 13, 14, 15, 16]
        mat[-1, 0]
    "}),
        13
    ); // last row, first column = [3,0]
}

#[test]
fn negative_index_2d_variable_reads_element() {
    // Variable indices: runtime normalization in both dimensions.
    assert_eq!(
        eval_int(indoc! {"
        mat = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12; 13, 14, 15, 16]
        r = 0 - 1
        c = 0 - 1
        mat[r, c]
    "}),
        16
    );
}

#[test]
fn negative_index_2d_variable_row_oob_is_runtime_error() {
    let error = PinpJit::eval(indoc! {"
        mat = [1, 2; 3, 4; 5, 6; 7, 8]
        r = 0 - 5
        mat[r, 0]
    "})
    .unwrap_err();
    assert!(error.contains("out of bounds"), "got: {error}");
}

#[test]
fn negative_index_2d_writes_and_reads_back() {
    // Write to all four corners via negative indices; read them back by positive indices.
    let (rows, cols, elements) = eval_matrix(indoc! {"
        mat = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12; 13, 14, 15, 16]
        mat[-4, -4] = 100
        mat[-4, -1] = 200
        mat[-1, -4] = 300
        mat[-1, -1] = 400
        mat
    "});
    assert_eq!(rows, 4);
    assert_eq!(cols, 4);
    assert_eq!(elements[0], PinpValue::Int(100)); // [0,0]
    assert_eq!(elements[3], PinpValue::Int(200)); // [0,3]
    assert_eq!(elements[12], PinpValue::Int(300)); // [3,0]
    assert_eq!(elements[15], PinpValue::Int(400)); // [3,3]
    // Untouched interior elements unchanged.
    assert_eq!(elements[5], PinpValue::Int(6)); // [1,1]
}

// -----------------------------------------------------------------------------------------
// Step 20 — loose ends
// -----------------------------------------------------------------------------------------

#[test]
fn identity_100x100_float_has_correct_diagonal() {
    let (rows, cols, elements) = eval_matrix("identity(100, float)");
    assert_eq!(rows, 100);
    assert_eq!(cols, 100);
    // Spot-check: diagonal elements are 1.0.
    assert_eq!(elements[0], PinpValue::Float(1.0)); // [0, 0]
    assert_eq!(elements[50 * 100 + 50], PinpValue::Float(1.0)); // [50, 50]
    assert_eq!(elements[99 * 100 + 99], PinpValue::Float(1.0)); // [99, 99]
    // Spot-check: off-diagonal elements are 0.0.
    assert_eq!(elements[1], PinpValue::Float(0.0)); // [0, 1]
    assert_eq!(elements[100], PinpValue::Float(0.0)); // [1, 0]
}

#[test]
fn indexed_compound_assign_2d_doubles_element() {
    let (rows, cols, elements) = eval_matrix(indoc! {"
        mat = [1, 2; 3, 4]
        mat[0, 1] *= 10
        mat
    "});
    assert_eq!(rows, 2);
    assert_eq!(cols, 2);
    assert_eq!(elements[0], PinpValue::Int(1));
    assert_eq!(elements[1], PinpValue::Int(20)); // was 2, now 2 * 10
    assert_eq!(elements[2], PinpValue::Int(3));
    assert_eq!(elements[3], PinpValue::Int(4));
}
