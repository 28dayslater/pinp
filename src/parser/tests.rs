// SPDX-License-Identifier: MIT

use super::*;
use indoc::indoc;

fn parse_ok(src: &str) -> Ast<'_> {
    parse(src).unwrap()
}

fn func<'ast>(ast: &'ast Ast, index: usize) -> &'ast FuncDef {
    match &ast.top_level[index] {
        TopLevel::Func(func_def) => func_def,
        other => panic!("Top-level element {index} is not a function: {other:?}."),
    }
}

// ExprId of the last top-level statement's expression.
fn root(ast: &Ast) -> ExprId {
    match ast.top_level.last().unwrap() {
        TopLevel::Stmt(Stmt::Expr(expr_id)) => *expr_id,
        TopLevel::Stmt(Stmt::Assign { values, .. }) => *values.last().unwrap(),
        other => panic!("Program does not end in an expression: {other:?}."),
    }
}

// --- expression structure ------------------------------------------------------------

#[test]
fn precedence_mul_over_add() {
    let ast = parse_ok("2 + 3 * 4");
    let Node::Bin {
        op: BinOp::Add,
        lhs,
        rhs,
    } = *ast.node(root(&ast))
    else {
        panic!("Expected an Add node at the root.");
    };
    assert_eq!(*ast.node(lhs), Node::Int(2));
    assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Mul, .. }));
}

// --- ranges, for, membership ---------------------------------------------------------

#[test]
fn plain_range_is_inclusive_with_no_step() {
    let ast = parse_ok("1..10");
    let Node::Range {
        start,
        stop,
        step,
        kind,
    } = *ast.node(root(&ast))
    else {
        panic!("Expected a Range node at the root.");
    };
    assert_eq!(kind, RangeKind::Inclusive);
    assert_eq!(*ast.node(start), Node::Int(1));
    assert_eq!(*ast.node(stop), Node::Int(10));
    assert!(step.is_none());
}

#[test]
fn range_operator_selects_the_kind() {
    for (src, expected) in [
        ("1..<10", RangeKind::UpExclusive),
        ("5..>1", RangeKind::DownExclusive),
    ] {
        let ast = parse_ok(src);
        let Node::Range { kind, .. } = *ast.node(root(&ast)) else {
            panic!("Expected a Range node for `{src}`.");
        };
        assert_eq!(kind, expected);
    }
}

#[test]
fn range_carries_a_step() {
    let ast = parse_ok("1..10:2");
    let Node::Range { step, .. } = *ast.node(root(&ast)) else {
        panic!("Expected a Range node.");
    };
    assert_eq!(*ast.node(step.expect("a step")), Node::Int(2));
}

#[test]
fn range_bounds_bind_below_arithmetic() {
    // `1+1..2*2` is `(1+1)..(2*2)`: additive/multiplicative bind tighter than `..`.
    let ast = parse_ok("1+1..2*2");
    let Node::Range { start, stop, .. } = *ast.node(root(&ast)) else {
        panic!("Expected a Range node at the root.");
    };
    assert!(matches!(ast.node(start), Node::Bin { op: BinOp::Add, .. }));
    assert!(matches!(ast.node(stop), Node::Bin { op: BinOp::Mul, .. }));
}

#[test]
fn range_sits_in_a_multi_assignment_value_slot() {
    // No comma form survives, so the middle value is one `Range` and the arity is 3.
    let ast = parse_ok("a, r, b = 1, 2..8:2, 3");
    let TopLevel::Stmt(Stmt::Assign { values, .. }) = ast.top_level.last().unwrap() else {
        panic!("Expected an assignment.");
    };
    assert_eq!(values.len(), 3);
    assert!(matches!(ast.node(values[1]), Node::Range { .. }));
}

#[test]
fn for_in_is_a_statement() {
    let ast = parse_ok(indoc! {"
            for idx in 1..5
                idx
        "});
    let TopLevel::Stmt(Stmt::For {
        source: range,
        body,
        ..
    }) = &ast.top_level[0]
    else {
        panic!("Expected a for statement.");
    };
    assert!(matches!(ast.node(*range), Node::Range { .. }));
    assert!(body.result.is_some());
}

#[test]
fn membership_is_an_expression() {
    let ast = parse_ok("x in 1..9");
    let Node::Membership { value, range } = *ast.node(root(&ast)) else {
        panic!("Expected a Membership node at the root.");
    };
    assert!(matches!(ast.node(value), Node::Var(_)));
    assert!(matches!(ast.node(range), Node::Range { .. }));
}

#[test]
fn power_is_right_assoc() {
    let ast = parse_ok("2 ^ 2 ^ 3");
    let Node::Bin {
        op: BinOp::Pow,
        lhs,
        rhs,
    } = *ast.node(root(&ast))
    else {
        panic!("Expected a Pow node at the root.");
    };
    assert_eq!(*ast.node(lhs), Node::Int(2));
    assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Pow, .. }));
}

#[test]
fn unary_minus_binds_below_power() {
    let ast = parse_ok("-2 ^ 2");
    let Node::Unary {
        op: UnOp::Neg,
        operand,
    } = *ast.node(root(&ast))
    else {
        panic!("Expected a unary Neg node at the root.");
    };
    assert!(matches!(
        ast.node(operand),
        Node::Bin { op: BinOp::Pow, .. }
    ));
}

#[test]
fn paren_grouping() {
    let ast = parse_ok("(2 + 3) * 4");
    let Node::Bin {
        op: BinOp::Mul,
        lhs,
        ..
    } = *ast.node(root(&ast))
    else {
        panic!("Expected a Mul node at the root.");
    };
    assert!(matches!(ast.node(lhs), Node::Bin { op: BinOp::Add, .. }));
}

#[test]
fn grouped_int_literal_value() {
    let ast = parse_ok("12_000_321");
    assert_eq!(*ast.node(root(&ast)), Node::Int(12_000_321));
}

#[test]
fn compound_assign_desugars_to_read_and_op() {
    // `b += 1` becomes `b = (b + 1)`: a single-target Assign whose value is a Bin reading `b`.
    let ast = parse_ok(indoc! {"
            b = 1
            b += 1
        "});
    let TopLevel::Stmt(Stmt::Assign {
        target_lists,
        values,
    }) = ast.top_level.last().unwrap()
    else {
        panic!("Expected a desugared local assignment.");
    };
    assert_eq!(target_lists.len(), 1);
    assert!(matches!(target_lists[0].as_slice(), [Place::Local(_)]));
    let Node::Bin {
        op: BinOp::Add,
        lhs,
        ..
    } = *ast.node(values[0])
    else {
        panic!("Expected the value to be an Add.");
    };
    assert!(matches!(ast.node(lhs), Node::Var(_)));
}

// --- multiple-target assignment ------------------------------------------------------

fn last_assign<'a>(ast: &'a Ast) -> (&'a Vec<Vec<Place>>, &'a Vec<ExprId>) {
    let TopLevel::Stmt(Stmt::Assign {
        target_lists,
        values,
    }) = ast.top_level.last().unwrap()
    else {
        panic!("Expected an assignment statement.");
    };
    (target_lists, values)
}

#[test]
fn parallel_assignment_shape() {
    let ast = parse_ok("a, b = 1, 2");
    let (target_lists, values) = last_assign(&ast);
    assert_eq!(target_lists.len(), 1);
    assert_eq!(target_lists[0].len(), 2);
    assert_eq!(values.len(), 2);
}

#[test]
fn chained_assignment_shape() {
    let ast = parse_ok("a = b = c = 0");
    let (target_lists, values) = last_assign(&ast);
    assert_eq!(target_lists.len(), 3); // a, b, c each their own group
    assert!(target_lists.iter().all(|group| group.len() == 1));
    assert_eq!(values.len(), 1);
}

#[test]
fn combined_parallel_and_chained_shape() {
    let ast = parse_ok("a, b = c, d = 1, 2");
    let (target_lists, values) = last_assign(&ast);
    assert_eq!(target_lists.len(), 2);
    assert!(target_lists.iter().all(|group| group.len() == 2));
    assert_eq!(values.len(), 2);
}

#[test]
fn single_assignment_is_degenerate_multi() {
    let ast = parse_ok("a = 1");
    let (target_lists, values) = last_assign(&ast);
    assert_eq!(target_lists.len(), 1);
    assert_eq!(target_lists[0].len(), 1);
    assert_eq!(values.len(), 1);
}

#[test]
fn arity_mismatch_is_error() {
    assert!(matches!(parse("a, b = 1"), Err(ParseError::Unexpected(_))));
    assert!(matches!(
        parse("a, b = c = 1, 2"),
        Err(ParseError::Unexpected(_))
    ));
}

#[test]
fn non_place_target_is_error() {
    assert!(matches!(parse("1 = 2"), Err(ParseError::Unexpected(_))));
    assert!(matches!(parse("a + b = 1"), Err(ParseError::Unexpected(_))));
}

#[test]
fn compound_assign_in_multi_target_is_error() {
    // Compound assignment is single-target only; `a, b += 1, 2` is rejected.
    assert!(matches!(
        parse("a, b += 1, 2"),
        Err(ParseError::Unexpected(_))
    ));
}

#[test]
fn bare_comma_list_without_assignment_is_error() {
    assert!(matches!(parse("a, b"), Err(ParseError::Unexpected(_))));
}

// --- multiple-target assignment: line continuation -----------------------------------

#[test]
fn rhs_continuation_aligned_parses() {
    // `1` is at column 8; the continued `2` aligns under it.
    let ast = parse_ok(indoc! {"
            a, b = 1,
                   2
        "});
    let (_, values) = last_assign(&ast);
    assert_eq!(values.len(), 2);
}

#[test]
fn lhs_continuation_aligned_parses() {
    // The LHS wraps via a trailing comma; the continuation is at the same indent.
    let ast = parse_ok(indoc! {"
            a,
            b = 1, 2
        "});
    let (target_lists, _) = last_assign(&ast);
    assert_eq!(target_lists[0].len(), 2);
}

#[test]
fn continuation_inside_function_body_parses() {
    // The RHS continuation is indented past the body, exercising the pending-dedent path.
    let ast = parse_ok(indoc! {"
            f(): int is
                a, b = 1,
                       2
                a + b
        "});
    let Stmt::Assign { values, .. } = &func(&ast, 0).body.stmts[0] else {
        panic!("Expected an assignment in the body.");
    };
    assert_eq!(values.len(), 2);
}

#[test]
fn misaligned_continuation_is_error() {
    // `2` does not align to `1`'s column (8).
    let src = indoc! {"
            a, b = 1,
              2
        "};
    assert!(matches!(parse(src), Err(ParseError::Layout(_))));
}

#[test]
fn comma_starting_a_line_is_error() {
    let src = indoc! {"
            a, b
            , c = 1, 2, 3
        "};
    assert!(matches!(parse(src), Err(ParseError::Layout(_))));
}

#[test]
fn trailing_comma_without_continuation_is_error() {
    let src = indoc! {"
            a, b = 1, 2,
        "};
    assert!(matches!(parse(src), Err(ParseError::Layout(_))));
}

// --- bool literals, logical & comparison operators -----------------------------------

#[test]
fn bool_literals() {
    let true_ast = parse_ok("true");
    assert_eq!(*true_ast.node(root(&true_ast)), Node::Bool(true));
    let false_ast = parse_ok("false");
    assert_eq!(*false_ast.node(root(&false_ast)), Node::Bool(false));
}

#[test]
fn logical_precedence_and_xor_or() {
    // `and` (30) > `xor` (25) > `or` (20): `a and b or a xor b` = `(a and b) or (a xor b)`.
    let ast = parse_ok("a and b or a xor b");
    let Node::Bin {
        op: BinOp::Or,
        lhs,
        rhs,
    } = *ast.node(root(&ast))
    else {
        panic!("Expected an Or at the root.");
    };
    assert!(matches!(ast.node(lhs), Node::Bin { op: BinOp::And, .. }));
    assert!(matches!(ast.node(rhs), Node::Bin { op: BinOp::Xor, .. }));
}

#[test]
fn not_is_unary_tight() {
    // C precedence: `not` binds tighter than every infix op, so `not a == b` = `(not a) == b`.
    let ast = parse_ok("not a == b");
    let Node::Bin {
        op: BinOp::Eq, lhs, ..
    } = *ast.node(root(&ast))
    else {
        panic!("Expected an Eq at the root.");
    };
    assert!(matches!(ast.node(lhs), Node::Unary { op: UnOp::Not, .. }));
}

#[test]
fn not_chains() {
    let ast = parse_ok("not not a");
    let Node::Unary {
        op: UnOp::Not,
        operand,
    } = *ast.node(root(&ast))
    else {
        panic!("Expected a Not at the root.");
    };
    assert!(matches!(
        ast.node(operand),
        Node::Unary { op: UnOp::Not, .. }
    ));
}

#[test]
fn comparison_binds_looser_than_arithmetic() {
    // `a + b < c` = `(a + b) < c`.
    let ast = parse_ok("a + b < c");
    let Node::Bin {
        op: BinOp::Lt, lhs, ..
    } = *ast.node(root(&ast))
    else {
        panic!("Expected a Lt at the root.");
    };
    assert!(matches!(ast.node(lhs), Node::Bin { op: BinOp::Add, .. }));
}

#[test]
fn lone_comparison_has_no_and() {
    let ast = parse_ok("a > b");
    assert!(matches!(
        ast.node(root(&ast)),
        Node::Bin { op: BinOp::Gt, .. }
    ));
}

#[test]
fn chained_comparison_desugars_with_shared_operand() {
    // `a < b < c` => `(a < b) and (b < c)`, with the middle `b` shared (same ExprId).
    let ast = parse_ok("a < b < c");
    let Node::Bin {
        op: BinOp::And,
        lhs,
        rhs,
    } = *ast.node(root(&ast))
    else {
        panic!("Expected an And at the root.");
    };
    let Node::Bin {
        op: BinOp::Lt,
        rhs: left_b,
        ..
    } = *ast.node(lhs)
    else {
        panic!("Expected the first comparison to be a Lt.");
    };
    let Node::Bin {
        op: BinOp::Lt,
        lhs: right_b,
        ..
    } = *ast.node(rhs)
    else {
        panic!("Expected the second comparison to be a Lt.");
    };
    assert_eq!(
        left_b, right_b,
        "the shared middle operand must reuse one ExprId"
    );
}

#[test]
fn monotonic_chains_parse() {
    parse_ok("a <= b < c");
    parse_ok("d >= e >= f > g");
    parse_ok("a == b == c");
}

#[test]
fn non_monotonic_chains_are_errors() {
    assert!(matches!(parse("a < b > c"), Err(ParseError::Unexpected(_))));
    assert!(matches!(
        parse("a <= b == c"),
        Err(ParseError::Unexpected(_))
    ));
    assert!(matches!(
        parse("a != b != c"),
        Err(ParseError::Unexpected(_))
    ));
}

// --- control flow: `if` expression, `while`/`loop` statements ------------------------

#[test]
fn ternary_binds_below_or() {
    // `a or b if c else d` = `(a or b) if c else d`.
    let ast = parse_ok("a or b if c else d");
    let Node::If { arms, else_block } = ast.node(root(&ast)) else {
        panic!("Expected an If at the root.");
    };
    assert_eq!(arms.len(), 1);
    let then = arms[0].body.result.unwrap();
    assert!(matches!(ast.node(then), Node::Bin { op: BinOp::Or, .. }));
    assert!(else_block.is_some());
}

#[test]
fn ternary_else_tail_is_right_associative() {
    // `a if p else b if q else c` = `a if p else (b if q else c)`.
    let ast = parse_ok("a if p else b if q else c");
    let Node::If { else_block, .. } = ast.node(root(&ast)) else {
        panic!("Expected an If at the root.");
    };
    let tail = else_block.as_ref().unwrap().result.unwrap();
    assert!(matches!(ast.node(tail), Node::If { .. }));
}

#[test]
fn wrapped_conditional_alignment() {
    // The `else` line must align to the column where the then-expression (`42`) starts.
    parse_ok(indoc! {"
            fu = 42 + 142 if a > 42
                 else 42
        "});
    let misaligned = indoc! {"
            fu = 42 + 142 if a > 42
               else 42
        "};
    assert!(matches!(parse(misaligned), Err(ParseError::Layout(_))));
}

#[test]
fn block_if_shape() {
    let ast = parse_ok(indoc! {"
            if a > b
                a
            elif a == b
                b
            else
                c
        "});
    let Node::If { arms, else_block } = ast.node(root(&ast)) else {
        panic!("Expected an If at the root.");
    };
    assert_eq!(arms.len(), 2); // `if` + one `elif`
    assert!(else_block.is_some());
    assert!(arms[0].body.result.is_some());
}

#[test]
fn block_if_as_function_result() {
    let ast = parse_ok(indoc! {"
            mx(a: int, b: int): int is
                if a > b
                    a
                else
                    b
        "});
    let func_def = func(&ast, 0);
    assert!(matches!(
        ast.node(func_def.body.result.unwrap()),
        Node::If { .. }
    ));
}

#[test]
fn while_and_loop_shapes() {
    let w = parse_ok(indoc! {"
            while a > 0
                a
        "});
    assert!(matches!(w.top_level[0], TopLevel::Stmt(Stmt::While { .. })));

    let l = parse_ok(indoc! {"
            loop
                a
            while a > 0
        "});
    assert!(matches!(
        l.top_level[0],
        TopLevel::Stmt(Stmt::Loop { until: false, .. })
    ));

    let u = parse_ok(indoc! {"
            loop
                a
            until a > 0
        "});
    assert!(matches!(
        u.top_level[0],
        TopLevel::Stmt(Stmt::Loop { until: true, .. })
    ));
}

#[test]
fn body_ending_in_statement_has_no_result() {
    let ast = parse_ok(indoc! {"
            while a > 0
                b = 1
        "});
    let TopLevel::Stmt(Stmt::While { body, .. }) = &ast.top_level[0] else {
        panic!("Expected a while statement.");
    };
    assert_eq!(body.stmts.len(), 1);
    assert!(body.result.is_none());
}

#[test]
fn loop_without_trailing_condition_is_error() {
    assert!(matches!(
        parse(indoc! {"
                loop
                    a
                b
            "}),
        Err(ParseError::Unexpected(_))
    ));
}

#[test]
fn deeply_nested_input_errors_rather_than_overflowing() {
    // Thousands of nested parens would blow the stack without the depth guard; instead the
    // parser bails fast with an error well before the real stack is exhausted.
    let deep = format!("{}1{}", "(".repeat(5_000), ")".repeat(5_000));
    assert!(matches!(parse(&deep), Err(ParseError::TooDeeplyNested(_))));
}

#[test]
fn moderately_nested_input_still_parses() {
    // Well under the limit — ordinary (if unusual) nesting must keep working.
    let nested = format!("{}1{}", "(".repeat(50), ")".repeat(50));
    assert!(parse(&nested).is_ok());
}

#[test]
fn excessive_if_arms_are_rejected() {
    // A flat `elif` ladder past the arm cap is refused (a sanity bound, not a stack risk).
    let mut src = String::from("if a > 0\n    1\n");
    for _ in 0..300 {
        src.push_str("elif a > 0\n    1\n");
    }
    src.push_str("else\n    1\n");
    assert!(matches!(parse(&src), Err(ParseError::TooManyArms(_))));
}

#[test]
fn bool_type_annotation() {
    let ast = parse_ok("f(a: bool): bool is a");
    assert_eq!(func(&ast, 0).params[0].param_type, PinpType::Bool);
    assert_eq!(func(&ast, 0).return_type, PinpType::Bool);
}

// --- function & program structure ----------------------------------------------------

#[test]
fn single_line_function() {
    let ast = parse_ok("fu(a:float, b:float, c:float): float is b^2 - 4*a*c");
    let func_def = func(&ast, 0);
    assert_eq!(ast.names[func_def.name.value()], "fu");
    assert_eq!(func_def.params.len(), 3);
    assert!(
        func_def
            .params
            .iter()
            .all(|param| param.param_type == PinpType::Float)
    );
    assert_eq!(func_def.return_type, PinpType::Float);
    assert!(func_def.body.stmts.is_empty());

    // Omitting the return type on the single-line form is an explicit, specific error —
    // it must name the missing return type, not fall back to a generic syntax message.
    let Err(err) = parse("fu(a: float) is a") else {
        panic!("Expected a missing-return-type error, but parsing succeeded.");
    };
    assert!(
        matches!(&err, ParseError::Unexpected(msg) if msg.contains("return type")),
        "Expected an explicit missing-return-type error, got {err:?}."
    );
}

#[test]
fn block_function_with_local() {
    let ast = parse_ok(indoc! {"
            _fu_bar_baz_1(a:int, b:int): int is
                xx = a+b*b
                xx
        "});
    let func_def = func(&ast, 0);
    assert_eq!(func_def.params.len(), 2);
    assert_eq!(func_def.return_type, PinpType::Int);
    assert_eq!(func_def.body.stmts.len(), 1);
    assert!(matches!(func_def.body.stmts[0], Stmt::Assign { .. }));
}

#[test]
fn multiline_params_aligned() {
    // `bb` aligns under `aa` (both column 3).
    let ast = parse_ok(indoc! {"
            f(aa: float,
              bb: float): float is aa^2 + bb^2
        "});
    assert_eq!(func(&ast, 0).params.len(), 2);
}

#[test]
fn hung_first_param_indented_parses() {
    // The opening line is bare; the hung params are indented past the function line.
    let ast = parse_ok(indoc! {"
            f(
                aa: float,
                bb: float): float is aa + bb
        "});
    assert_eq!(func(&ast, 0).params.len(), 2);
}

#[test]
fn block_body_trailing_call_shape() {
    // A block body's trailing expression `b + fu(b)` is a `Bin` whose rhs is a call.
    let ast = parse_ok(indoc! {"
            fu(a: int): int is a + 2
            bar(b: int): int is
                b + fu(b)
        "});
    let bar = func(&ast, 1);
    let Node::Bin { rhs, .. } = *ast.node(bar.body.result.unwrap()) else {
        panic!("Expected the body to end in a binary expression.");
    };
    assert!(matches!(ast.node(rhs), Node::Call { .. }));
}

#[test]
fn global_compound_assign_desugars_to_global_target() {
    let ast = parse_ok(indoc! {"
            g = 10
            bump(a: int): int is
                ::g += 1
                a + ::g
        "});
    let Stmt::Assign { target_lists, .. } = &func(&ast, 1).body.stmts[0] else {
        panic!("Expected a global compound assignment.");
    };
    assert_eq!(target_lists.len(), 1);
    assert!(matches!(target_lists[0].as_slice(), [Place::Global(_)]));
}

#[test]
fn return_type_annotation_resolved() {
    let ast = parse_ok("f(a: int): float is a");
    assert_eq!(func(&ast, 0).return_type, PinpType::Float);
}

#[test]
fn program_shape_with_several_top_level() {
    let ast = parse_ok(indoc! {"
            g = 100
            sq(x: int): int is x*x
            bump(a: int): int is
                ::g += 1
                a + ::g + sq(2)
            sq(3)
        "});
    assert_eq!(ast.top_level.len(), 4);
    assert!(matches!(ast.top_level[0], TopLevel::Stmt(_)));
    assert!(matches!(ast.top_level[1], TopLevel::Func(_)));
    assert!(matches!(ast.top_level[2], TopLevel::Func(_)));
    assert!(matches!(ast.top_level[3], TopLevel::Stmt(_)));
}

// --- syntactic / structural errors ---------------------------------------------------

#[test]
fn misaligned_param_is_error() {
    // `bb` is one column past `aa`.
    let src = indoc! {"
            f(aa: int,
               bb: int): int is aa+bb
        "};
    assert!(matches!(parse(src), Err(ParseError::Layout(_))));
}

#[test]
fn hung_first_param_not_indented_is_error() {
    // The opening line is bare, but the hung first param is not indented past the function line.
    let src = indoc! {"
            f(
            aa: int,
            bb: int): int is aa+bb
        "};
    assert!(matches!(parse(src), Err(ParseError::Layout(_))));
}

#[test]
fn duplicate_param_is_error() {
    assert!(matches!(
        parse("f(a: int, a: int): int is a"),
        Err(ParseError::Unexpected(_))
    ));
}

#[test]
fn unknown_type_name_is_error() {
    assert!(matches!(
        parse("f(a: blah): int is a"),
        Err(ParseError::Unexpected(_))
    ));
}

#[test]
fn integer_literal_overflow_is_error() {
    // Lexically valid but out of i64 range, in every base — rejected, not panicked.
    assert!(matches!(
        parse("99999999999999999999"),
        Err(ParseError::Unexpected(_))
    )); // decimal > i64::MAX
    assert!(matches!(
        parse("0xFFFF_FFFF_FFFF_FFFF_F"),
        Err(ParseError::Unexpected(_))
    )); // 68-bit hex
    assert!(matches!(parse("9E99"), Err(ParseError::Unexpected(_)))); // mantissa*10^exp overflows
    let wide_binary = format!("0b{}", "1".repeat(65));
    assert!(matches!(
        parse(&wide_binary),
        Err(ParseError::Unexpected(_))
    )); // 65-bit binary
}

#[test]
fn max_int_literal_is_accepted() {
    let ast = parse_ok("9223372036854775807"); // i64::MAX
    assert_eq!(*ast.node(root(&ast)), Node::Int(i64::MAX));
}

// --- array literals, comprehensions, index, member, indexed-assign -------------------

#[test]
fn array_literal_parse_shape() {
    let ast = parse_ok("[1, 2, 3]");
    let Node::ArrayLiteral { elements } = ast.node(root(&ast)) else {
        panic!("Expected ArrayLiteral.");
    };
    assert_eq!(elements.len(), 3);
    assert_eq!(*ast.node(elements[0]), Node::Int(1));
    assert_eq!(*ast.node(elements[1]), Node::Int(2));
    assert_eq!(*ast.node(elements[2]), Node::Int(3));
}

#[test]
fn single_element_array_parse_shape() {
    let ast = parse_ok("[42]");
    let Node::ArrayLiteral { elements } = ast.node(root(&ast)) else {
        panic!("Expected ArrayLiteral.");
    };
    assert_eq!(elements.len(), 1);
    assert_eq!(*ast.node(elements[0]), Node::Int(42));
}

#[test]
fn trailing_comma_in_array_literal_is_ok() {
    let ast = parse_ok("[1, 2, 3,]");
    let Node::ArrayLiteral { elements } = ast.node(root(&ast)) else {
        panic!("Expected ArrayLiteral.");
    };
    assert_eq!(elements.len(), 3);
}

#[test]
fn empty_array_literal_is_parse_error() {
    assert!(parse("[]").is_err());
}

#[test]
fn comprehension_parse_shape() {
    let ast = parse_ok("[x for x in 1..5]");
    let Node::Comprehension { var, var_type, .. } = ast.node(root(&ast)) else {
        panic!("Expected Comprehension.");
    };
    let (var, var_type) = (*var, *var_type);
    assert_eq!(ast.names[var.value()], "x");
    assert_eq!(var_type, PinpType::Int);
}

#[test]
fn comprehension_with_float_type_annotation() {
    let ast = parse_ok("[x for x:float in 1..5]");
    let Node::Comprehension { var_type, .. } = ast.node(root(&ast)) else {
        panic!("Expected Comprehension.");
    };
    assert_eq!(*var_type, PinpType::Float);
}

#[test]
fn index_parse_shape() {
    let ast = parse_ok("a[1]");
    let Node::Index { array, index } = ast.node(root(&ast)) else {
        panic!("Expected Index.");
    };
    let (array, index) = (*array, *index);
    assert!(matches!(ast.node(array), Node::Var(_)));
    assert_eq!(*ast.node(index), Node::Int(1));
}

#[test]
fn member_parse_shape() {
    let ast = parse_ok("a.len");
    let Node::Member { object, member } = ast.node(root(&ast)) else {
        panic!("Expected Member.");
    };
    let member = *member;
    assert!(matches!(ast.node(*object), Node::Var(_)));
    assert_eq!(ast.names[member.value()], "len");
}

#[test]
fn postfix_index_binds_tighter_than_unary_minus() {
    // `-a[0]` must parse as `-(a[0])`, not `(-a)[0]`.
    let ast = parse_ok("-a[0]");
    let Node::Unary {
        op: UnOp::Neg,
        operand,
    } = ast.node(root(&ast))
    else {
        panic!("Expected Unary Neg.");
    };
    assert!(matches!(ast.node(*operand), Node::Index { .. }));
}

#[test]
fn postfix_index_binds_tighter_than_exponentiation() {
    // `a^b[0]` must parse as `a^(b[0])`, not `(a^b)[0]`.
    let ast = parse_ok("a^b[0]");
    let Node::Bin {
        op: BinOp::Pow,
        lhs,
        rhs,
    } = ast.node(root(&ast))
    else {
        panic!("Expected Bin Pow.");
    };
    let (lhs, rhs) = (*lhs, *rhs);
    assert!(matches!(ast.node(lhs), Node::Var(_)));
    assert!(matches!(ast.node(rhs), Node::Index { .. }));
}

#[test]
fn chained_postfix_index_and_member() {
    // `a[0].len` parses as Member { object: Index { array: a, index: 0 }, member: len }.
    let ast = parse_ok("a[0].len");
    let Node::Member { object, member } = ast.node(root(&ast)) else {
        panic!("Expected Member.");
    };
    let member = *member;
    assert!(matches!(ast.node(*object), Node::Index { .. }));
    assert_eq!(ast.names[member.value()], "len");
}

#[test]
fn indexed_assign_parse_shape() {
    let ast = parse_ok("a[0] = 99");
    let TopLevel::Stmt(Stmt::IndexedAssign {
        target,
        index,
        value,
    }) = ast.top_level.last().unwrap()
    else {
        panic!("Expected IndexedAssign.");
    };
    assert!(matches!(target, Place::Local(_)));
    assert_eq!(*ast.node(*index), Node::Int(0));
    assert_eq!(*ast.node(*value), Node::Int(99));
}

#[test]
fn slice_parse_shape() {
    // arr[1..3] parses as Index { array: Var(arr), index: Range(1, 3, Inclusive) }.
    let ast = parse_ok("arr[1..3]");
    let Node::Index { array, index } = ast.node(root(&ast)) else {
        panic!("Expected Index.");
    };
    let (array, index) = (*array, *index);
    assert!(matches!(ast.node(array), Node::Var(_)));
    assert!(matches!(ast.node(index), Node::Range { .. }));
}

#[test]
fn exclusive_slice_parse_shape() {
    let ast = parse_ok("arr[1..<4]");
    let Node::Index { index, .. } = ast.node(root(&ast)) else {
        panic!("Expected Index.");
    };
    let Node::Range { kind, .. } = ast.node(*index) else {
        panic!("Expected Range inside Index.");
    };
    assert_eq!(*kind, RangeKind::UpExclusive);
}

#[test]
fn slice_assign_parse_shape() {
    // arr[1..3] = 5 → IndexedAssign { index: Range(1..3), value: Int(5) }.
    let ast = parse_ok("arr[1..3] = 5");
    let TopLevel::Stmt(Stmt::IndexedAssign {
        target,
        index,
        value,
    }) = ast.top_level.last().unwrap()
    else {
        panic!("Expected IndexedAssign.");
    };
    assert!(matches!(target, Place::Local(_)));
    assert!(matches!(ast.node(*index), Node::Range { .. }));
    assert_eq!(*ast.node(*value), Node::Int(5));
}

#[test]
fn multiline_array_literal_parses() {
    // Brackets suppress newlines just like parentheses.
    let ast = parse_ok(indoc! {"
            [1,
             2,
             3]
        "});
    let Node::ArrayLiteral { elements } = ast.node(root(&ast)) else {
        panic!("Expected ArrayLiteral.");
    };
    assert_eq!(elements.len(), 3);
}

#[test]
fn range_init_as_array_literal() {
    // `[1..5]` is an ArrayLiteral with a single Range element (the range-init syntax).
    let ast = parse_ok("[1..5]");
    let Node::ArrayLiteral { elements } = ast.node(root(&ast)) else {
        panic!("Expected ArrayLiteral.");
    };
    assert_eq!(elements.len(), 1);
    assert!(matches!(ast.node(elements[0]), Node::Range { .. }));
}

// --- matrix literals ---------------------------------------------------------------

#[test]
fn matrix_literal_two_by_two() {
    let ast = parse_ok("[1, 2; 3, 4]");
    let Node::MatrixLiteral { rows } = ast.node(root(&ast)) else {
        panic!("Expected MatrixLiteral.");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[1].len(), 2);
    assert_eq!(*ast.node(rows[0][0]), Node::Int(1));
    assert_eq!(*ast.node(rows[0][1]), Node::Int(2));
    assert_eq!(*ast.node(rows[1][0]), Node::Int(3));
    assert_eq!(*ast.node(rows[1][1]), Node::Int(4));
}

#[test]
fn matrix_literal_three_rows() {
    let ast = parse_ok("[1, 2; 3, 4; 5, 6]");
    let Node::MatrixLiteral { rows } = ast.node(root(&ast)) else {
        panic!("Expected MatrixLiteral.");
    };
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.len() == 2));
}

#[test]
fn matrix_literal_single_column() {
    // A column vector (N×1) is a valid matrix literal.
    let ast = parse_ok("[1; 2; 3]");
    let Node::MatrixLiteral { rows } = ast.node(root(&ast)) else {
        panic!("Expected MatrixLiteral.");
    };
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.len() == 1));
}

#[test]
fn matrix_literal_multiline_parses() {
    let ast = parse_ok(indoc! {"
        [1, 2, 3;
         4, 5, 6]
    "});
    let Node::MatrixLiteral { rows } = ast.node(root(&ast)) else {
        panic!("Expected MatrixLiteral.");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].len(), 3);
    assert_eq!(rows[1].len(), 3);
}

#[test]
fn trailing_semicolon_is_parse_error() {
    assert!(parse("[1, 2;]").is_err());
}

#[test]
fn trailing_comma_before_semicolon_is_parse_error() {
    // `[1, 2,; 3, 4]` — a dangling `,` before a row separator is invalid.
    assert!(matches!(
        parse("[1, 2,; 3, 4]"),
        Err(ParseError::Unexpected(_))
    ));
}

#[test]
fn matrix_multiline_misaligned_row_is_layout_error() {
    // Row 2 starts at a different column than row 1.
    let src = indoc! {"
        [1, 2, 3;
           4, 5, 6]
    "};
    assert!(matches!(parse(src), Err(ParseError::Layout(_))));
}

#[test]
fn direct_index_on_array_literal_parses() {
    // `[10, 20][0]` is valid parser output: Index { array: ArrayLiteral(...), index: Int(0) }.
    let ast = parse_ok("[10, 20][0]");
    let Node::Index { array, index } = ast.node(root(&ast)) else {
        panic!("Expected Index.");
    };
    let (array, index) = (*array, *index);
    assert!(matches!(ast.node(array), Node::ArrayLiteral { .. }));
    assert_eq!(*ast.node(index), Node::Int(0));
}
