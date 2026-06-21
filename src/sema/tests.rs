// SPDX-License-Identifier: MIT

use super::*;
use crate::parser::{ArrayElementType, Ast, BinOp, Node, PinpType, Stmt, TopLevel, parse};
use indoc::indoc;

/// Parse + analyze, returning the typed AST (panicking on any error).
fn analyzed(src: &str) -> Ast<'_> {
    let mut ast = parse(src).expect("source should parse");
    analyze(&mut ast).expect("source should analyze");
    ast
}

/// Parse (expecting success) then analyze, returning the semantic error.
fn sema_error(src: &str) -> SemaError {
    let mut ast = parse(src).expect("source should parse");
    analyze(&mut ast).expect_err("analysis should fail")
}

/// Type of the program's final top-level expression.
fn root_type(src: &str) -> PinpType {
    let ast = analyzed(src);
    match ast.top_level.last().unwrap() {
        TopLevel::Stmt(Stmt::Expr(expr_id)) => ast.type_of(*expr_id),
        TopLevel::Stmt(Stmt::Assign { values, .. }) => ast.type_of(*values.last().unwrap()),
        other => panic!("program does not end in an expression: {other:?}"),
    }
}

// --- type inference ------------------------------------------------------------------

#[test]
fn arithmetic_type_rules() {
    assert_eq!(root_type("2 + 3"), PinpType::Int);
    assert_eq!(root_type("10 / 4"), PinpType::Float);
    assert_eq!(root_type("10 div 4"), PinpType::Int);
    assert_eq!(root_type("7 mod 3"), PinpType::Int);
    assert_eq!(root_type("2 ^ 10"), PinpType::Int);
    assert_eq!(root_type("2.0 ^ 10"), PinpType::Float);
    assert_eq!(root_type("-3.14"), PinpType::Float);
}

// --- ranges, for, membership ---------------------------------------------------------

#[test]
fn range_types_as_range() {
    assert_eq!(root_type("1..10"), PinpType::Range);
    assert_eq!(root_type("1..<10:2"), PinpType::Range);
}

#[test]
fn float_part_is_a_type_error() {
    assert!(matches!(sema_error("1.0..3"), SemaError::Type(_)));
    assert!(matches!(sema_error("1..10:2.0"), SemaError::Type(_)));
}

#[test]
fn literal_ranges_are_validated() {
    for (src, message) in [
        ("1..149:-13", "The range ascends, but the step is negative."),
        ("1..>149:13", "An ascending range must use ..<."),
        ("42..<1:5", "A descending range must use ..>."),
        ("1..10:0", "Range step cannot be zero."),
    ] {
        assert_eq!(
            sema_error(src),
            SemaError::Type(message.into()),
            "for `{src}`"
        );
    }
}

#[test]
fn zero_step_is_rejected_even_with_variable_bounds() {
    // The bounds are variables, so the direction checks are deferred — but a literal zero step
    // never advances and is invalid regardless.
    assert_eq!(
        sema_error(indoc! {"
                a, b = 1, 10
                a..b:0
            "}),
        SemaError::Type("Range step cannot be zero.".into())
    );
}

#[test]
fn for_binds_an_int_loop_variable() {
    assert_eq!(
        root_type(indoc! {"
                total = 0
                for idx in 1..3
                    total += idx
                total
            "}),
        PinpType::Int
    );
}

#[test]
fn loop_variable_is_read_only() {
    assert!(matches!(
        sema_error(indoc! {"
                for idx in 1..3
                    idx = 5
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn for_over_a_non_range_is_an_error() {
    assert!(matches!(
        sema_error(indoc! {"
                for idx in 5
                    idx
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn membership_types_as_bool() {
    assert_eq!(root_type("3 in 1..9"), PinpType::Bool);
}

#[test]
fn membership_rejects_a_non_int_value() {
    assert!(matches!(sema_error("1.5 in 1..9"), SemaError::Type(_)));
}

#[test]
fn range_is_not_a_scalar_operand() {
    // A range is not numeric: arithmetic, comparison, and unary minus on one are type errors,
    // not values that reach codegen as scalars.
    assert!(matches!(sema_error("(1..5) + 1"), SemaError::Type(_)));
    assert!(matches!(
        sema_error(indoc! {"
                r = 1..5
                r == r
            "}),
        SemaError::Type(_)
    ));
    assert!(matches!(sema_error("-(1..5)"), SemaError::Type(_)));
}

#[test]
fn int_promotes_to_float() {
    assert_eq!(
        root_type(indoc! {"
                a = 2
                2.0 * a
            "}),
        PinpType::Float
    );
}

#[test]
fn assignment_then_reference() {
    let ast = analyzed(indoc! {"
            a = 2 + 3
            a * a
        "});
    let TopLevel::Stmt(Stmt::Expr(expr_id)) = ast.top_level.last().unwrap() else {
        panic!("expected an expression statement");
    };
    assert!(matches!(
        ast.node(*expr_id),
        Node::Bin { op: BinOp::Mul, .. }
    ));
    assert_eq!(ast.type_of(*expr_id), PinpType::Int);
}

#[test]
fn reassignment_to_incompatible_type_is_error() {
    // Decision (0004): assignment is checked uniformly — re-binding to a non-assignable
    // type is an error, just like a compound assignment. `Float` is not assignable to `Int`.
    assert!(matches!(
        sema_error(indoc! {"
                a = 1
                a = 2.0
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn parallel_assignment_introduces_each_target() {
    // `a` (Int) and `b` (Float) are both defined and usable afterwards.
    assert_eq!(
        root_type(indoc! {"
                a, b = 1, 2.0
                a
            "}),
        PinpType::Int
    );
    assert_eq!(
        root_type(indoc! {"
                a, b = 1, 2.0
                b
            "}),
        PinpType::Float
    );
}

#[test]
fn chained_assignment_types_all_targets_from_value() {
    assert_eq!(
        root_type(indoc! {"
                a = b = 2.0
                a + b
            "}),
        PinpType::Float
    );
}

#[test]
fn parallel_assignment_mutates_existing_targets() {
    // Pre-existing Int `a`,`b`; a swap re-binds both (type-checked), staying Int.
    assert_eq!(
        root_type(indoc! {"
                a = 1
                b = 2
                a, b = b, a
                a
            "}),
        PinpType::Int
    );
}

#[test]
fn parallel_assignment_rejects_void_value() {
    // `noop` is void (its body is an `else`-less `if`, which is Void); binding its result fails.
    assert!(matches!(
        sema_error(indoc! {"
                noop(c: bool) is
                    if c
                        0
                a, b = 1, noop(true)
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn parallel_assignment_type_mismatch_is_error() {
    // `a` is Int; re-binding it to a Float in a parallel assignment is rejected.
    assert!(matches!(
        sema_error(indoc! {"
                a = 1
                a, b = 2.0, 3
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn parallel_assignment_to_unknown_global_is_error() {
    // A `::global` target must already exist, even in a multi-target assignment.
    assert!(matches!(
        sema_error("a, ::g = 1, 2"),
        SemaError::UnknownSymbol(_)
    ));
}

#[test]
fn reassignment_promotes_into_float_slot() {
    // `a` is Float; re-assigning an Int value is fine (Int promotes), and `a` stays Float.
    assert_eq!(
        root_type(indoc! {"
                a = 1.0
                a = 2
                a
            "}),
        PinpType::Float
    );
}

// --- bool, comparisons & logicals ----------------------------------------------------

#[test]
fn bool_literal_and_logicals() {
    assert_eq!(root_type("true"), PinpType::Bool);
    assert_eq!(root_type("true and false"), PinpType::Bool);
    assert_eq!(root_type("true or false xor true"), PinpType::Bool);
    assert_eq!(root_type("not true"), PinpType::Bool);
}

#[test]
fn comparisons_yield_bool() {
    assert_eq!(root_type("1 < 2"), PinpType::Bool);
    assert_eq!(root_type("1.0 >= 2"), PinpType::Bool);
    assert_eq!(root_type("true == false"), PinpType::Bool);
    assert_eq!(root_type("1 < 2 < 3"), PinpType::Bool); // chained
}

#[test]
fn bool_promotes_in_arithmetic() {
    assert_eq!(root_type("true + false"), PinpType::Int); // bool -> int
    assert_eq!(root_type("true + 1"), PinpType::Int);
    assert_eq!(root_type("true + 1.0"), PinpType::Float);
    assert_eq!(root_type("-true"), PinpType::Int); // unary minus promotes bool to int
}

#[test]
fn bool_assignable_to_numeric_slot() {
    // bool flows into an int/float return slot, but not the reverse.
    assert_eq!(
        root_type(indoc! {"
                f(a: bool): int is a
                f(true)
            "}),
        PinpType::Int
    );
    assert!(matches!(
        sema_error("f(a: int): bool is a"),
        SemaError::Type(_)
    ));
}

#[test]
fn logical_on_non_bool_is_error() {
    assert!(matches!(sema_error("1 and 2"), SemaError::Type(_)));
    assert!(matches!(sema_error("not 1"), SemaError::Type(_)));
}

#[test]
fn bool_function_signature() {
    assert_eq!(
        root_type(indoc! {"
                fu(a: bool, b: bool): bool is a and b or a xor b
                fu(true, false)
            "}),
        PinpType::Bool
    );
}

// --- control flow --------------------------------------------------------------------

#[test]
fn if_expression_type_is_branch_join() {
    assert_eq!(root_type("1 if true else 2"), PinpType::Int);
    assert_eq!(root_type("1 if true else 2.0"), PinpType::Float); // join(Int, Float)
    assert_eq!(root_type("true if false else false"), PinpType::Bool);
}

#[test]
fn condition_must_be_bool() {
    assert!(matches!(sema_error("1 if 5 else 2"), SemaError::Type(_)));
    assert!(matches!(
        sema_error(indoc! {"
                i = 0
                while i
                    i += 1
                i
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn if_without_else_is_void_and_unusable_as_value() {
    assert!(matches!(
        sema_error(indoc! {"
                x = 0
                x = if true
                    1
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn body_local_does_not_escape() {
    // `m` is introduced inside the if-body, so it is not in scope afterwards.
    assert!(matches!(
        sema_error(indoc! {"
                if true
                    m = 1
                m
            "}),
        SemaError::UnknownSymbol(_)
    ));
}

#[test]
fn conditional_update_mutates_outer() {
    // `m` exists before the `if`; assigning it inside mutates that binding, so `m` stays in
    // scope (and Int) afterwards.
    assert_eq!(
        root_type(indoc! {"
                m = 5
                if true
                    m = 7
                m
            "}),
        PinpType::Int
    );
}

#[test]
fn bare_loop_counter_type_checks() {
    assert_eq!(
        root_type(indoc! {"
                i = 0
                while i < 3
                    i += 1
                i
            "}),
        PinpType::Int
    );
}

// --- error cases ---------------------------------------------------------------------

#[test]
fn float_div_is_type_error() {
    assert!(matches!(sema_error("2.0 div 1"), SemaError::Type(_)));
}

#[test]
fn unassigned_symbol_is_error() {
    assert!(matches!(sema_error("x + 1"), SemaError::UnknownSymbol(_)));
}

#[test]
fn call_typechecks() {
    assert_eq!(
        root_type(indoc! {"
                sq(x: int): int is x*x
                sq(5)
            "}),
        PinpType::Int
    );
}

#[test]
fn call_arg_promotes_int_to_float() {
    assert_eq!(
        root_type(indoc! {"
                f(x: float): float is x
                f(3)
            "}),
        PinpType::Float
    );
}

#[test]
fn call_arity_mismatch_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                sq(x: int): int is x*x
                sq(1, 2)
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn call_arg_type_mismatch_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                sq(x: int): int is x*x
                sq(1.5)
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn call_before_definition_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                foo(1)
                foo(x: int): int is x
            "}),
        SemaError::UnknownSymbol(_)
    ));
}

#[test]
fn bare_name_does_not_see_global() {
    assert!(matches!(
        sema_error(indoc! {"
                g = 10
                f(a: int): int is a + g
            "}),
        SemaError::UnknownSymbol(_)
    ));
}

#[test]
fn compound_assign_local() {
    assert_eq!(
        root_type(indoc! {"
                f(a: int): int is
                    b = a
                    b += 1
                    b
                f(1)
            "}),
        PinpType::Int
    );
}

#[test]
fn compound_div_breaks_int_place() {
    // `b` is Int; `b /= 2` yields Float, not assignable back to Int.
    assert!(matches!(
        sema_error(indoc! {"
                f(a: int): int is
                    b = a
                    b /= 2
                    b
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn void_function_with_value_body_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                f(a: int) is
                    a + 1
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn return_float_to_int_is_error() {
    assert!(matches!(
        sema_error("f(a: float): int is a"),
        SemaError::Type(_)
    ));
}

#[test]
fn void_parameter_is_error() {
    // `void` is the no-return marker, not a value type, so it cannot be a parameter.
    assert!(matches!(
        sema_error("f(a: void): int is 1"),
        SemaError::Type(_)
    ));
}

#[test]
fn global_access_and_compound_assign() {
    assert_eq!(
        root_type(indoc! {"
                g = 10
                bump(a: int): int is
                    ::g += 1
                    a + ::g
                bump(5)
            "}),
        PinpType::Int
    );
}

// --- arrays: literal type inference --------------------------------------------------

#[test]
fn int_array_literal_type() {
    assert_eq!(
        root_type("[1, 2, 3]"),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn float_array_literal_type() {
    assert_eq!(
        root_type("[1.5, 2.5]"),
        PinpType::Array(ArrayElementType::Float, 2)
    );
}

#[test]
fn bool_array_literal_type() {
    assert_eq!(
        root_type("[true, false, true]"),
        PinpType::Array(ArrayElementType::Bool, 3)
    );
}

#[test]
fn int_bool_elements_join_to_int_array() {
    // bool promotes to int under the join lattice.
    assert_eq!(
        root_type("[1, true, 3]"),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn int_float_elements_join_to_float_array() {
    assert_eq!(
        root_type("[1, 2, 3.5]"),
        PinpType::Array(ArrayElementType::Float, 3)
    );
}

#[test]
fn bool_float_elements_join_to_float_array() {
    // bool → int → float: full three-way promotion in one literal.
    assert_eq!(
        root_type("[true, 2, 3.5]"),
        PinpType::Array(ArrayElementType::Float, 3)
    );
}

#[test]
fn range_element_mixed_with_scalar_is_error() {
    // A Range and an Int have no common type in the join lattice.
    assert!(matches!(sema_error("[1, 1..5]"), SemaError::Type(_)));
}

#[test]
fn single_element_array_literal_type() {
    assert_eq!(root_type("[42]"), PinpType::Array(ArrayElementType::Int, 1));
}

// --- arrays: index expression --------------------------------------------------------

#[test]
fn index_of_int_array_is_int() {
    assert_eq!(
        root_type(indoc! {"
                a = [1, 2, 3]
                a[0]
            "}),
        PinpType::Int
    );
}

#[test]
fn index_of_float_array_is_float() {
    assert_eq!(
        root_type(indoc! {"
                a = [1.5, 2.5]
                a[0]
            "}),
        PinpType::Float
    );
}

#[test]
fn index_of_bool_array_is_bool() {
    assert_eq!(
        root_type(indoc! {"
                a = [true, false]
                a[0]
            "}),
        PinpType::Bool
    );
}

#[test]
fn index_on_non_array_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = 5
                a[0]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn float_index_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                a[1.5]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn index_on_int_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            a = 5
            a[0]
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn index_on_float_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            a = 3.14
            a[0]
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn index_on_bool_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            a = true
            a[0]
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn bool_index_is_accepted() {
    // bool is int_like, so it is a valid index.
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30]
                a[true]
            "}),
        PinpType::Int
    );
}

#[test]
fn double_index_on_scalar_result_is_error() {
    // a[0] is Int; indexing an Int is not valid.
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                a[0][1]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn index_of_comprehension_result_is_element_type() {
    // The comprehension type is Array(Int, 5); indexing it yields Int.
    assert_eq!(root_type("[x for x in 1..5][2]"), PinpType::Int);
}

// --- arrays: member access -----------------------------------------------------------

#[test]
fn len_of_int_array_is_int() {
    assert_eq!(
        root_type(indoc! {"
                a = [1, 2, 3]
                a.len
            "}),
        PinpType::Int
    );
}

#[test]
fn len_of_float_array_is_int() {
    assert_eq!(
        root_type(indoc! {"
                a = [1.0, 2.0, 3.0, 4.0]
                a.len
            "}),
        PinpType::Int
    );
}

#[test]
fn member_on_int_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = 5
                a.len
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn member_on_float_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = 3.14
                a.len
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn unknown_member_name_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                a.foo
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn len_usable_as_range_bound() {
    // a.len is Int, so it can appear as a range bound (variable bound, runtime-evaluated).
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30]
                0..<a.len
            "}),
        PinpType::Range
    );
}

// --- arrays: comprehension -----------------------------------------------------------

#[test]
fn comprehension_int_var_type() {
    assert_eq!(
        root_type("[x for x in 1..5]"),
        PinpType::Array(ArrayElementType::Int, 5)
    );
}

#[test]
fn comprehension_float_var_type() {
    assert_eq!(
        root_type("[x for x:float in 1..3]"),
        PinpType::Array(ArrayElementType::Float, 3)
    );
}

#[test]
fn comprehension_element_expr_promotes_to_float() {
    // x is Int; x * 2.0 promotes to Float, so the resulting array is Float.
    assert_eq!(
        root_type("[x * 2.0 for x in 1..3]"),
        PinpType::Array(ArrayElementType::Float, 3)
    );
}

#[test]
fn comprehension_squared_element_stays_int() {
    assert_eq!(
        root_type("[x * x for x in 1..4]"),
        PinpType::Array(ArrayElementType::Int, 4)
    );
}

#[test]
fn comprehension_exclusive_range_count() {
    assert_eq!(
        root_type("[x for x in 0..<10]"),
        PinpType::Array(ArrayElementType::Int, 10)
    );
}

#[test]
fn comprehension_stepped_range_count() {
    assert_eq!(
        root_type("[x for x in 0..9:3]"),
        PinpType::Array(ArrayElementType::Int, 4)
    );
}

#[test]
fn comprehension_bool_var_is_error() {
    assert!(matches!(
        sema_error("[x for x:bool in 1..5]"),
        SemaError::Type(_)
    ));
}

#[test]
fn comprehension_source_must_be_range() {
    assert!(matches!(sema_error("[x for x in 5]"), SemaError::Type(_)));
}

#[test]
fn comprehension_variable_bounds_are_rejected() {
    // Non-literal bounds make the array length unknown at compile time.
    assert!(matches!(
        sema_error(indoc! {"
                a = 5
                b = 10
                [x for x in a..b]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn comprehension_empty_up_exclusive_range_is_error() {
    let err = sema_error("[x for x in 1..<1]");
    assert!(matches!(&err, SemaError::Type(msg) if msg.contains("empty")));
}

#[test]
fn comprehension_empty_down_exclusive_range_is_error() {
    let err = sema_error("[x for x in 1..>1]");
    assert!(matches!(&err, SemaError::Type(msg) if msg.contains("empty")));
}

// A plain empty exclusive range as a `for` source is NOT a sema error — the body just never runs.
#[test]
fn empty_exclusive_range_in_for_loop_is_valid() {
    assert_eq!(
        root_type(indoc! {"
                total = 0
                for x in 1..<1
                    total += x
                total
            "}),
        PinpType::Int
    );
}

// --- arrays: range-init --------------------------------------------------------------

#[test]
fn range_init_inclusive_type() {
    assert_eq!(
        root_type("[1..5]"),
        PinpType::Array(ArrayElementType::Int, 5)
    );
}

#[test]
fn range_init_exclusive_type() {
    assert_eq!(
        root_type("[0..<4]"),
        PinpType::Array(ArrayElementType::Int, 4)
    );
}

#[test]
fn range_init_stepped_type() {
    assert_eq!(
        root_type("[0..10:2]"),
        PinpType::Array(ArrayElementType::Int, 6)
    );
}

#[test]
fn range_init_descending_inclusive_type() {
    assert_eq!(
        root_type("[5..1]"),
        PinpType::Array(ArrayElementType::Int, 5)
    );
}

#[test]
fn range_init_descending_exclusive_type() {
    assert_eq!(
        root_type("[5..>1]"),
        PinpType::Array(ArrayElementType::Int, 4)
    );
}

#[test]
fn range_init_single_element_equal_bounds() {
    // Inclusive range with start == stop yields exactly one element.
    assert_eq!(
        root_type("[3..3]"),
        PinpType::Array(ArrayElementType::Int, 1)
    );
}

#[test]
fn range_init_empty_up_exclusive_is_error() {
    let err = sema_error("[1..<1]");
    assert!(matches!(&err, SemaError::Type(msg) if msg.contains("empty")));
}

#[test]
fn range_init_empty_down_exclusive_is_error() {
    let err = sema_error("[1..>1]");
    assert!(matches!(&err, SemaError::Type(msg) if msg.contains("empty")));
}

#[test]
fn range_init_variable_bound_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = 5
                [a..10]
            "}),
        SemaError::Type(_)
    ));
}

// --- arrays: indexed assignment ------------------------------------------------------

#[test]
fn indexed_assign_int_to_float_array_promotes() {
    // Int is assignable to Float, so this must not error.
    analyzed(indoc! {"
            a = [1.0, 2.0, 3.0]
            a[0] = 5
        "});
}

#[test]
fn indexed_assign_float_to_int_array_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                a[0] = 1.5
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn indexed_assign_bool_to_int_array_is_ok() {
    // bool promotes to int.
    analyzed(indoc! {"
            a = [1, 2, 3]
            a[0] = true
        "});
}

#[test]
fn indexed_assign_to_non_array_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = 5
                a[0] = 1
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn indexed_assign_float_index_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                a[1.5] = 99
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn indexed_assign_bool_index_is_accepted() {
    analyzed(indoc! {"
            a = [1, 2, 3]
            a[true] = 99
        "});
}

#[test]
fn indexed_assign_to_undeclared_name_is_unknown_symbol() {
    assert!(matches!(
        sema_error("a[0] = 1"),
        SemaError::UnknownSymbol(_)
    ));
}

#[test]
fn indexed_assign_range_value_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                a[0] = 1..5
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn array_cannot_be_passed_as_scalar_argument() {
    // Functions accept only scalar types; an Array is not assignable to Int.
    assert!(matches!(
        sema_error(indoc! {"
                sq(x: int): int is x * x
                a = [1, 2, 3]
                sq(a)
            "}),
        SemaError::Type(_)
    ));
}

// --- slices: access ------------------------------------------------------------------

#[test]
fn slice_of_int_array_type() {
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30, 40, 50]
                a[1..3]
            "}),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn slice_of_float_array_type() {
    assert_eq!(
        root_type(indoc! {"
                a = [1.5, 2.5, 3.5, 4.5]
                a[0..1]
            "}),
        PinpType::Array(ArrayElementType::Float, 2)
    );
}

#[test]
fn slice_of_bool_array_type() {
    assert_eq!(
        root_type(indoc! {"
                a = [true, false, true, false]
                a[0..2]
            "}),
        PinpType::Array(ArrayElementType::Bool, 3)
    );
}

#[test]
fn exclusive_slice_type() {
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30, 40, 50]
                a[1..<4]
            "}),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn single_element_slice_type() {
    // Inclusive range with start == stop is a one-element slice.
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30]
                a[1..1]
            "}),
        PinpType::Array(ArrayElementType::Int, 1)
    );
}

#[test]
fn slice_spanning_whole_array() {
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30]
                a[0..2]
            "}),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn slice_stop_at_last_valid_index_is_ok() {
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30, 40, 50]
                a[2..4]
            "}),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn slice_stop_equals_len_inclusive_is_error() {
    // Inclusive stop == len is out of bounds (valid indices are 0..len-1).
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30]
                a[0..3]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn exclusive_slice_stop_equals_len_is_ok() {
    // Exclusive stop == len is valid: up to but not including index len.
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30]
                a[0..<3]
            "}),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn exclusive_slice_stop_exceeds_len_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30]
                a[0..<4]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_negative_start_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30]
                a[-1..1]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_with_step_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30, 40, 50]
                a[0..4:2]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_with_variable_start_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30]
                n = 1
                a[n..2]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_with_variable_stop_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30]
                n = 2
                a[0..n]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_descending_operator_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30, 40, 50]
                a[4..>1]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_empty_exclusive_equal_bounds_is_error() {
    // 1..<1 is an empty slice (0 elements) — rejected.
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30]
                a[1..<1]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_on_int_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = 5
                a[0..1]
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_result_is_indexable() {
    // A slice is an Array; indexing it yields the element type.
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30, 40, 50]
                a[1..3][0]
            "}),
        PinpType::Int
    );
}

#[test]
fn slice_result_has_len() {
    assert_eq!(
        root_type(indoc! {"
                a = [10, 20, 30, 40, 50]
                a[1..3].len
            "}),
        PinpType::Int
    );
}

// --- slices: assign ------------------------------------------------------------------

#[test]
fn slice_assign_scalar_to_int_array_is_ok() {
    analyzed(indoc! {"
            a = [1, 2, 3, 4, 5]
            a[1..3] = 0
        "});
}

#[test]
fn slice_assign_bool_to_int_array_is_ok() {
    analyzed(indoc! {"
            a = [1, 2, 3, 4, 5]
            a[1..3] = true
        "});
}

#[test]
fn slice_assign_int_to_float_array_promotes() {
    analyzed(indoc! {"
            a = [1.0, 2.0, 3.0, 4.0, 5.0]
            a[1..3] = 0
        "});
}

#[test]
fn slice_assign_float_to_int_array_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3, 4, 5]
                a[1..3] = 1.5
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_assign_with_step_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3, 4, 5]
                a[0..4:2] = 0
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_assign_oob_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                a[0..3] = 0
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_assign_variable_bounds_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                n = 2
                a[0..n] = 0
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_assign_to_non_array_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = 5
                a[0..1] = 0
            "}),
        SemaError::Type(_)
    ));
}

#[test]
fn slice_assign_negative_start_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3, 4, 5]
                a[-1..2] = 0
            "}),
        SemaError::Type(_)
    ));
}
