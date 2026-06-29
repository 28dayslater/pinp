// SPDX-License-Identifier: MIT

use super::*;
use crate::parser::{ArrayElementType, BinOp, Node, PinpType, ProgramAst, Stmt, TopLevel, parse};
use indoc::indoc;

/// Parse + analyze, returning the typed AST (panicking on any error).
fn analyzed(src: &str) -> ProgramAst<'_> {
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
fn bool_index_is_error() {
    // Only Int is a valid index; Bool is rejected even though it is int_like.
    assert!(matches!(
        sema_error(indoc! {"
                a = [10, 20, 30]
                a[true]
            "}),
        SemaError::Type(_)
    ));
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
fn indexed_assign_bool_index_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
                a = [1, 2, 3]
                a[true] = 99
            "}),
        SemaError::Type(_)
    ));
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

// --- Matrix type integration (Step 6) -----------------------------------------------
// `join` and `assignable` already operate on `PinpType`'s derived `PartialEq`.
// These tests verify the Matrix cases without requiring MatrixLiteral sema (step 7).

#[test]
fn join_identical_matrix_types() {
    let matrix = PinpType::Matrix(ArrayElementType::Int, 2, 3);
    assert_eq!(join(matrix, matrix), Some(matrix));
}

#[test]
fn join_different_shape_matrices_is_none() {
    let m1 = PinpType::Matrix(ArrayElementType::Int, 2, 2);
    let m2 = PinpType::Matrix(ArrayElementType::Int, 2, 3);
    assert_eq!(join(m1, m2), None);
}

#[test]
fn join_different_element_type_matrices_is_none() {
    // No promotion across element types for matrices — they are distinct types.
    let m_int = PinpType::Matrix(ArrayElementType::Int, 2, 2);
    let m_float = PinpType::Matrix(ArrayElementType::Float, 2, 2);
    assert_eq!(join(m_int, m_float), None);
}

#[test]
fn join_matrix_with_scalar_is_none() {
    let matrix = PinpType::Matrix(ArrayElementType::Int, 2, 2);
    assert_eq!(join(matrix, PinpType::Int), None);
    assert_eq!(join(PinpType::Int, matrix), None);
}

#[test]
fn assignable_same_matrix() {
    let matrix = PinpType::Matrix(ArrayElementType::Float, 3, 3);
    assert!(assignable(matrix, matrix));
}

#[test]
fn assignable_different_shape_matrix_is_false() {
    let m1 = PinpType::Matrix(ArrayElementType::Int, 2, 2);
    let m2 = PinpType::Matrix(ArrayElementType::Int, 3, 3);
    assert!(!assignable(m1, m2));
}

#[test]
fn assignable_matrix_to_scalar_is_false() {
    let matrix = PinpType::Matrix(ArrayElementType::Int, 2, 2);
    assert!(!assignable(matrix, PinpType::Int));
    assert!(!assignable(PinpType::Int, matrix));
    assert!(!assignable(matrix, PinpType::Float));
}

// --- 2D indexing and slicing (Step 8) -----------------------------------------------

// Sets up a 3×4 Int matrix `m` then appends `expr` as the final expression.
fn with_matrix(expr: &str) -> String {
    format!("m = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12]\n{expr}")
}

#[test]
fn index2d_scalar_both_yields_scalar() {
    assert_eq!(root_type(&with_matrix("m[0, 1]")), PinpType::Int);
}

#[test]
fn index2d_float_matrix_yields_float_scalar() {
    assert_eq!(
        root_type(indoc! {"
            m = [1.0, 2.0; 3.0, 4.0]
            m[0, 1]
        "}),
        PinpType::Float
    );
}

#[test]
fn index2d_scalar_row_range_col_yields_array() {
    // m[0..1, 0] — inclusive row slice (2 rows), scalar col → Array(Int, 2)
    assert_eq!(
        root_type(&with_matrix("m[0..1, 0]")),
        PinpType::Array(ArrayElementType::Int, 2)
    );
}

#[test]
fn index2d_scalar_row_range_col_exclusive_yields_array() {
    // m[0..<2, 0] — exclusive row slice (2 rows) → Array(Int, 2)
    assert_eq!(
        root_type(&with_matrix("m[0..<2, 0]")),
        PinpType::Array(ArrayElementType::Int, 2)
    );
}

#[test]
fn index2d_scalar_row_range_col_yields_correct_length() {
    // m[0, 1..3] — row 0, cols 1-3 inclusive (3 elements) → Array(Int, 3)
    assert_eq!(
        root_type(&with_matrix("m[0, 1..3]")),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn index2d_both_range_yields_matrix() {
    // m[0..1, 0..2] — 2 rows × 3 cols → Matrix(Int, 2, 3)
    assert_eq!(
        root_type(&with_matrix("m[0..1, 0..2]")),
        PinpType::Matrix(ArrayElementType::Int, 2, 3)
    );
}

#[test]
fn index2d_full_extent_row_yields_array() {
    // m[:, 0] — all 3 rows, col 0 → Array(Int, 3)
    assert_eq!(
        root_type(&with_matrix("m[:, 0]")),
        PinpType::Array(ArrayElementType::Int, 3)
    );
}

#[test]
fn index2d_full_extent_col_yields_array() {
    // m[0, :] — row 0, all 4 cols → Array(Int, 4)
    assert_eq!(
        root_type(&with_matrix("m[0, :]")),
        PinpType::Array(ArrayElementType::Int, 4)
    );
}

#[test]
fn index2d_both_full_extent_yields_matrix() {
    // m[:, :] — all rows × all cols → Matrix(Int, 3, 4)
    assert_eq!(
        root_type(&with_matrix("m[:, :]")),
        PinpType::Matrix(ArrayElementType::Int, 3, 4)
    );
}

#[test]
fn double_index_on_matrix_scalar_result_is_error() {
    // m[0, 1][0] — result of scalar index is Int, not indexable.
    assert!(matches!(
        sema_error(&with_matrix("m[0, 1][0]")),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_on_1d_array_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            a[0, 1]
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_row_slice_out_of_bounds_is_error() {
    // m[0..3, 0] — inclusive stop=3 on rows=3 → out of bounds
    assert!(matches!(
        sema_error(&with_matrix("m[0..3, 0]")),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_col_slice_out_of_bounds_is_error() {
    // m[0, 0..4] — inclusive stop=4 on cols=4 → out of bounds
    assert!(matches!(
        sema_error(&with_matrix("m[0, 0..4]")),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_variable_slice_bound_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12]
            n = 2
            m[0, n..3]
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_stepped_slice_is_error() {
    assert!(matches!(
        sema_error(&with_matrix("m[0, 0..2:1]")),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_descending_slice_is_error() {
    assert!(matches!(
        sema_error(&with_matrix("m[0, 2..>0]")),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_negative_slice_start_is_error() {
    assert!(matches!(
        sema_error(&with_matrix("m[-1..1, 0]")),
        SemaError::Type(_)
    ));
}

#[test]
fn full_extent_outside_index2d_is_error() {
    // `arr[:]` — FullExtent in a 1D index context is invalid.
    assert!(matches!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            a[:]
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_empty_exclusive_row_slice_is_error() {
    // m[0..<0, 0] — equal bounds with `..<` produce an empty slice; must be rejected.
    assert!(matches!(
        sema_error(&with_matrix("m[0..<0, 0]")),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_empty_exclusive_col_slice_is_error() {
    // m[0, 1..<1] — same rule applied to the column dimension.
    assert!(matches!(
        sema_error(&with_matrix("m[0, 1..<1]")),
        SemaError::Type(_)
    ));
}

// --- Matrix literal sema (Step 7) ---------------------------------------------------

#[test]
fn matrix_literal_int_type() {
    assert_eq!(
        root_type("[1, 2; 3, 4]"),
        PinpType::Matrix(ArrayElementType::Int, 2, 2)
    );
}

#[test]
fn matrix_literal_float_type() {
    assert_eq!(
        root_type("[1.0, 2.0; 3.0, 4.0]"),
        PinpType::Matrix(ArrayElementType::Float, 2, 2)
    );
}

#[test]
fn matrix_literal_bool_type() {
    assert_eq!(
        root_type("[true, false; false, true]"),
        PinpType::Matrix(ArrayElementType::Bool, 2, 2)
    );
}

#[test]
fn matrix_literal_column_vector() {
    assert_eq!(
        root_type("[1; 2; 3]"),
        PinpType::Matrix(ArrayElementType::Int, 3, 1)
    );
}

#[test]
fn matrix_literal_rectangular() {
    assert_eq!(
        root_type("[1, 2, 3; 4, 5, 6]"),
        PinpType::Matrix(ArrayElementType::Int, 2, 3)
    );
}

#[test]
fn matrix_literal_int_float_promotes_to_float() {
    assert_eq!(
        root_type("[1, 2; 3, 4.0]"),
        PinpType::Matrix(ArrayElementType::Float, 2, 2)
    );
}

#[test]
fn matrix_literal_bool_int_promotes_to_int() {
    assert_eq!(
        root_type("[true, false; 0, 1]"),
        PinpType::Matrix(ArrayElementType::Int, 2, 2)
    );
}

#[test]
fn matrix_literal_jagged_rows_is_error() {
    assert!(matches!(sema_error("[1, 2; 3, 4, 5]"), SemaError::Type(_)));
}

#[test]
fn matrix_literal_short_row_is_error() {
    assert!(matches!(sema_error("[1, 2; 3]"), SemaError::Type(_)));
}

#[test]
fn matrix_literal_inconsistent_element_types_is_error() {
    // Range is not joinable with Int — the common type cannot be determined.
    assert!(matches!(sema_error("[1, 2; 3, 1..5]"), SemaError::Type(_)));
}

// --- Built-in members on Matrix (Step 9) --------------------------------------------

#[test]
fn ndim_of_1d_array_is_int() {
    assert_eq!(
        root_type(indoc! {"
            a = [1, 2, 3]
            a.ndim
        "}),
        PinpType::Int
    );
}

#[test]
fn ndim_of_2d_matrix_is_int() {
    assert_eq!(root_type(&with_matrix("m.ndim")), PinpType::Int);
}

#[test]
fn ndim_on_scalar_is_error() {
    assert_eq!(
        sema_error(indoc! {"
            a = 5
            a.ndim
        "}),
        SemaError::Type("`.ndim` is not defined on Int.".into())
    );
}

#[test]
fn rows_of_matrix_is_int() {
    assert_eq!(root_type(&with_matrix("m.rows")), PinpType::Int);
}

#[test]
fn cols_of_matrix_is_int() {
    assert_eq!(root_type(&with_matrix("m.cols")), PinpType::Int);
}

#[test]
fn rows_on_1d_array_is_error() {
    assert_eq!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            a.rows
        "}),
        SemaError::Type(".rows is not defined for a 1D array. Use .len.".into())
    );
}

#[test]
fn cols_on_1d_array_is_error() {
    assert_eq!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            a.cols
        "}),
        SemaError::Type(".cols is not defined for a 1D array. Use .len.".into())
    );
}

#[test]
fn len_of_matrix_is_int() {
    // .len on a 3x4 matrix is valid and returns Int.
    assert_eq!(root_type(&with_matrix("m.len")), PinpType::Int);
}

#[test]
fn unknown_member_on_matrix_is_error() {
    assert!(matches!(
        sema_error(&with_matrix("m.foo")),
        SemaError::Type(_)
    ));
}

#[test]
fn rows_on_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            a = 5
            a.rows
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn cols_on_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            a = 5
            a.cols
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn ndim_on_range_is_error() {
    assert!(matches!(sema_error("(1..5).ndim"), SemaError::Type(_)));
}

// --- identity() built-in (Step 10) --------------------------------------------------

#[test]
fn identity_int_yields_square_int_matrix() {
    assert_eq!(
        root_type("identity(3, int)"),
        PinpType::Matrix(ArrayElementType::Int, 3, 3)
    );
}

#[test]
fn identity_float_yields_square_float_matrix() {
    assert_eq!(
        root_type("identity(3, float)"),
        PinpType::Matrix(ArrayElementType::Float, 3, 3)
    );
}

#[test]
fn identity_size_2_is_minimum_valid() {
    assert_eq!(
        root_type("identity(2, int)"),
        PinpType::Matrix(ArrayElementType::Int, 2, 2)
    );
}

#[test]
fn identity_result_has_member_access() {
    // .rows/.cols on the result type work because it is a Matrix.
    assert_eq!(root_type("identity(3, int).rows"), PinpType::Int);
    assert_eq!(root_type("identity(4, float).cols"), PinpType::Int);
}

#[test]
fn identity_size_1_is_error() {
    assert_eq!(
        sema_error("identity(1, int)"),
        SemaError::Type("identity() size must be a literal integer >= 2.".into())
    );
}

#[test]
fn identity_size_0_is_error() {
    assert_eq!(
        sema_error("identity(0, int)"),
        SemaError::Type("identity() size must be a literal integer >= 2.".into())
    );
}

#[test]
fn identity_bool_element_type_is_error() {
    assert_eq!(
        sema_error("identity(3, bool)"),
        SemaError::Type("identity() does not support bool element type.".into())
    );
}

#[test]
fn identity_unknown_type_name_is_error() {
    assert_eq!(
        sema_error("identity(3, complex)"),
        SemaError::Type("identity() type must be int or float.".into())
    );
}

#[test]
fn identity_non_var_type_arg_is_error() {
    // Second arg must be a type-name identifier, not a literal.
    assert_eq!(
        sema_error("identity(3, 5)"),
        SemaError::Type("identity() type must be int or float.".into())
    );
}

#[test]
fn identity_wrong_arg_count_one_is_error() {
    assert_eq!(
        sema_error("identity(3)"),
        SemaError::Type("identity() takes exactly 2 arguments.".into())
    );
}

#[test]
fn identity_wrong_arg_count_zero_is_error() {
    assert_eq!(
        sema_error("identity()"),
        SemaError::Type("identity() takes exactly 2 arguments.".into())
    );
}

#[test]
fn identity_wrong_arg_count_three_is_error() {
    assert_eq!(
        sema_error("identity(3, int, 1)"),
        SemaError::Type("identity() takes exactly 2 arguments.".into())
    );
}

#[test]
fn identity_variable_size_is_error() {
    // First arg must be a literal — a variable is not accepted.
    assert_eq!(
        sema_error(indoc! {"
            n = 4
            identity(n, int)
        "}),
        SemaError::Type("identity() size must be a literal integer >= 2.".into())
    );
}

#[test]
fn identity_assignable_to_matrix_slot() {
    // Result is assignable; re-binding a Matrix slot works when shapes match.
    analyzed(indoc! {"
        m = identity(3, int)
        m = identity(3, int)
    "});
}

#[test]
fn identity_user_defined_conflicts_with_builtin_is_error() {
    assert_eq!(
        sema_error(indoc! {"
            identity(n: int): int is
                n * 2
            identity(3, int)
        "}),
        SemaError::Type(
            "Cannot define a function named `identity`: conflicts with a built-in.".into()
        )
    );
}

// --- ForArray and extended For (Step 11) --------------------------------------------

// -- Extended Stmt::For (1-binder, array/matrix source) --

#[test]
fn for_range_with_array_len_bound_and_indexed_assign() {
    // `idx` is a range loop variable (Int, read-only). Using it as an index into `ary`
    // is a READ, not an assignment to `idx`, so the loop-var guard must not fire.
    // Note: compound `ary[idx] += 1` is a parse error until step-20 loose ends; use `=`.
    analyzed(indoc! {"
        ary = [1, 2, 3]
        for idx in 0..ary.len
            ary[idx] = ary[idx] + 1
    "});
}

#[test]
fn for_over_1d_int_array_binds_int() {
    analyzed(indoc! {"
        a = [1, 2, 3]
        total = 0
        for val in a
            total += val
    "});
}

#[test]
fn for_over_2d_int_matrix_binds_int() {
    analyzed(indoc! {"
        m = [1, 2; 3, 4]
        total = 0
        for val in m
            total += val
    "});
}

#[test]
fn for_over_float_array_binds_float() {
    analyzed(indoc! {"
        a = [1.0, 2.0, 3.0]
        total = 0.0
        for val in a
            total += val
    "});
}

#[test]
fn for_over_bool_array_binds_bool() {
    analyzed(indoc! {"
        a = [true, false, true]
        for val in a
            val
    "});
}

#[test]
fn for_over_range_still_works_after_extension() {
    analyzed(indoc! {"
        total = 0
        for idx in 1..3
            total += idx
    "});
}

#[test]
fn for_over_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            for val in 42
                val
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn for_over_bool_scalar_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            for val in true
                val
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn for_loop_var_on_1d_array_is_read_only() {
    assert!(matches!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            for val in a
                val = 5
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn for_loop_var_on_2d_matrix_is_read_only() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            for val in m
                val = 5
        "}),
        SemaError::Type(_)
    ));
}

// -- Stmt::ForArray (2 binders, 1D) --

#[test]
fn for_array_2_binders_on_int_array() {
    // idx is Int, val is Int — both usable in the body.
    analyzed(indoc! {"
        a = [10, 20, 30]
        total = 0
        for idx, val in a
            total += val + idx
    "});
}

#[test]
fn for_array_2_binders_on_float_array() {
    analyzed(indoc! {"
        a = [1.0, 2.0, 3.0]
        total = 0.0
        for idx, val in a
            total += val
    "});
}

#[test]
fn for_array_2_binders_index_is_int() {
    // idx is Int: using it as a range bound is valid.
    analyzed(indoc! {"
        a = [10, 20, 30]
        for idx, val in a
            0..idx
    "});
}

#[test]
fn for_array_2_binders_on_matrix_is_error() {
    assert_eq!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            for idx, val in m
                val
        "}),
        SemaError::Type("Binder count does not match array rank.".into())
    );
}

#[test]
fn for_array_2_binders_on_range_is_error() {
    assert_eq!(
        sema_error(indoc! {"
            for idx, val in 1..5
                val
        "}),
        SemaError::Type("Binder count does not match array rank.".into())
    );
}

#[test]
fn for_array_index_binder_is_read_only() {
    assert!(matches!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            for idx, val in a
                idx = 0
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn for_array_value_binder_is_read_only() {
    assert!(matches!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            for idx, val in a
                val = 0
        "}),
        SemaError::Type(_)
    ));
}

// -- Stmt::ForArray (3 binders, 2D) --

#[test]
fn for_array_3_binders_on_int_matrix() {
    analyzed(indoc! {"
        m = [1, 2; 3, 4]
        total = 0
        for row, col, val in m
            total += val + row + col
    "});
}

#[test]
fn for_array_3_binders_on_float_matrix() {
    analyzed(indoc! {"
        m = [1.0, 2.0; 3.0, 4.0]
        total = 0.0
        for row, col, val in m
            total += val
    "});
}

#[test]
fn for_array_3_binders_on_1d_array_is_error() {
    assert_eq!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            for row, col, val in a
                val
        "}),
        SemaError::Type("Binder count does not match array rank.".into())
    );
}

#[test]
fn for_array_row_binder_is_read_only() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            for row, col, val in m
                row = 0
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn for_array_col_binder_is_read_only() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            for row, col, val in m
                col = 0
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn for_array_3_binders_value_is_read_only() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            for row, col, val in m
                val = 0
        "}),
        SemaError::Type(_)
    ));
}

// -- Stmt::IndexedAssign2D --

#[test]
fn indexed_assign2d_int_value_to_int_matrix() {
    analyzed(indoc! {"
        m = [1, 2; 3, 4]
        m[0, 1] = 42
    "});
}

#[test]
fn indexed_assign2d_float_value_to_float_matrix() {
    analyzed(indoc! {"
        m = [1.0, 2.0; 3.0, 4.0]
        m[0, 1] = 9.9
    "});
}

#[test]
fn indexed_assign2d_bool_promotes_to_int_matrix() {
    analyzed(indoc! {"
        m = [1, 2; 3, 4]
        m[0, 0] = true
    "});
}

#[test]
fn indexed_assign2d_int_promotes_to_float_matrix() {
    analyzed(indoc! {"
        m = [1.0, 2.0; 3.0, 4.0]
        m[0, 0] = 42
    "});
}

#[test]
fn indexed_assign2d_float_to_int_matrix_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            m[0, 1] = 1.5
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn indexed_assign2d_target_not_matrix_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            a[0, 1] = 42
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn indexed_assign2d_float_row_index_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            m[1.0, 0] = 42
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn indexed_assign2d_float_col_index_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            m[0, 1.0] = 42
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn indexed_assign2d_bool_row_index_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            m[true, 0] = 99
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn indexed_assign2d_bool_col_index_is_error() {
    assert!(matches!(
        sema_error(indoc! {"
            m = [1, 2; 3, 4]
            m[0, false] = 99
        "}),
        SemaError::Type(_)
    ));
}

#[test]
fn index2d_bool_row_read_gives_clear_diagnostic() {
    // Bool in a read index position must give the "Matrix index must be Int" message,
    // not the misleading "Slice requires a literal-bound range" fallback.
    assert_eq!(
        sema_error(&with_matrix("m[true, 0]")),
        SemaError::Type("Matrix index must be Int, got Bool.".into())
    );
}

#[test]
fn index2d_bool_col_read_gives_clear_diagnostic() {
    assert_eq!(
        sema_error(&with_matrix("m[0, false]")),
        SemaError::Type("Matrix index must be Int, got Bool.".into())
    );
}

// -----------------------------------------------------------------------------------------
// Step 19 — literal scalar index bounds checking (1D and 2D)
// -----------------------------------------------------------------------------------------

fn with_array4(expr: &str) -> String {
    format!("a = [10, 20, 30, 40]\n{expr}")
}

fn with_matrix4(expr: &str) -> String {
    format!("m = [1, 2, 3, 4; 5, 6, 7, 8; 9, 10, 11, 12; 13, 14, 15, 16]\n{expr}")
}

#[test]
fn literal_positive_oob_index_on_1d_is_sema_error() {
    assert_eq!(
        sema_error(&with_array4("a[4]")),
        SemaError::Type("Array index out of bounds.".into())
    );
}

#[test]
fn literal_negative_oob_index_on_1d_is_sema_error() {
    // -5 on a 4-element array: effective = -5 + 4 = -1 < 0.
    assert_eq!(
        sema_error(&with_array4("a[-5]")),
        SemaError::Type("Array index out of bounds.".into())
    );
}

#[test]
fn literal_negative_in_bounds_index_on_1d_is_valid() {
    assert_eq!(root_type(&with_array4("a[-4]")), PinpType::Int); // first element
    assert_eq!(root_type(&with_array4("a[-1]")), PinpType::Int); // last element
}

#[test]
fn literal_positive_oob_row_on_2d_is_sema_error() {
    assert_eq!(
        sema_error(&with_matrix4("m[4, 0]")),
        SemaError::Type("Array index out of bounds.".into())
    );
}

#[test]
fn literal_negative_oob_col_on_2d_is_sema_error() {
    assert_eq!(
        sema_error(&with_matrix4("m[0, -5]")),
        SemaError::Type("Array index out of bounds.".into())
    );
}

#[test]
fn literal_negative_in_bounds_index_on_2d_is_valid() {
    assert_eq!(root_type(&with_matrix4("m[-4, -4]")), PinpType::Int);
    assert_eq!(root_type(&with_matrix4("m[-1, -1]")), PinpType::Int);
}

#[test]
fn literal_oob_write_index_on_1d_is_sema_error() {
    assert_eq!(
        sema_error(&with_array4("a[-5] = 99")),
        SemaError::Type("Array index out of bounds.".into())
    );
}

#[test]
fn literal_valid_negative_write_index_on_1d_is_accepted() {
    assert!(!analyzed(&with_array4("a[-1] = 99")).top_level.is_empty());
}

#[test]
fn literal_oob_write_index_on_2d_is_sema_error() {
    assert_eq!(
        sema_error(&with_matrix4("m[0, -5] = 99")),
        SemaError::Type("Array index out of bounds.".into())
    );
}

// ---- step 20 — loose ends -------------------------------------------------------

#[test]
fn for_underscore_value_binder_on_1d_is_sema_error() {
    assert_eq!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            for _ in a
                0
        "}),
        SemaError::Type("Value binder must not be `_`.".into())
    );
}

#[test]
fn for_idx_underscore_value_binder_on_1d_is_sema_error() {
    assert_eq!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            for idx, _ in a
                0
        "}),
        SemaError::Type("Value binder must not be `_`.".into())
    );
}

#[test]
fn for_both_underscore_binders_on_1d_is_sema_error() {
    assert_eq!(
        sema_error(indoc! {"
            a = [1, 2, 3]
            for _, _ in a
                0
        "}),
        SemaError::Type("Value binder must not be `_`.".into())
    );
}

#[test]
fn for_underscore_value_binder_on_2d_is_sema_error() {
    assert_eq!(
        sema_error(&with_matrix4(indoc! {"
            for row, col, _ in m
                0
        "})),
        SemaError::Type("Value binder must not be `_`.".into())
    );
}

#[test]
fn for_all_underscore_binders_on_2d_is_sema_error() {
    assert_eq!(
        sema_error(&with_matrix4(indoc! {"
            for _, _, _ in m
                0
        "})),
        SemaError::Type("Value binder must not be `_`.".into())
    );
}

#[test]
fn indexed_compound_assign_1d_type_mismatch_is_sema_error() {
    // `a[0] += 1.5` on Int array: value becomes Float, not assignable to Int element.
    assert_eq!(
        sema_error(&with_array4("a[0] += 1.5")),
        SemaError::Type("Cannot assign Float to array element of type Int.".into())
    );
}

#[test]
fn indexed_compound_assign_2d_type_mismatch_is_sema_error() {
    // `m[0, 0] *= 2.5` on Int matrix: value becomes Float, not assignable to Int element.
    assert_eq!(
        sema_error(&with_matrix4("m[0, 0] *= 2.5")),
        SemaError::Type("Cannot assign Float to matrix element of type Int.".into())
    );
}

#[test]
fn indexed_compound_div_eq_on_int_array_is_sema_error() {
    // `/=` always produces Float; cannot store Float back into an Int array.
    assert_eq!(
        sema_error(&with_array4("a[0] /= 2")),
        SemaError::Type("Cannot assign Float to array element of type Int.".into())
    );
}

// --- `str` annotations rejected until string values land in sema/codegen -------------

#[test]
fn str_parameter_is_rejected() {
    // The `str` annotation parses (`parse_type` accepts it), but a `str` parameter is deferred this
    // iteration, so sema rejects it rather than letting `Str` reach codegen's unimplemented path.
    assert_eq!(
        sema_error("greet(name: str): int is 5"),
        SemaError::Type("String parameters are not yet supported.".into())
    );
}

#[test]
fn str_comprehension_annotation_is_rejected() {
    // A range counter cannot promote to a string; sema rejects the annotation before codegen.
    assert_eq!(
        sema_error("[1 for x:str in 1..3]"),
        SemaError::Type("Cannot promote range variable to string.".into())
    );
}
