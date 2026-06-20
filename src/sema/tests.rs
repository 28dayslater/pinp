// SPDX-License-Identifier: MIT

use super::*;
use crate::parser::{Ast, BinOp, Node, PinpType, Stmt, TopLevel, parse};
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
