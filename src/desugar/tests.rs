// SPDX-License-Identifier: MIT

use indoc::indoc;

use super::*;
use crate::ir::{Operation, Printer, verify};
use crate::parser::parse;
use crate::sema::analyze;

/// Parses, analyses, and desugars `src`, checks that every function verifies, and returns them all
/// printed in id order (the entry last), separated by blank lines.
fn desugared(src: &str) -> String {
    let (ast, program) = compile(src);
    let printer = Printer::new(&program.tables, &ast.names, &ast.string_literals);
    program
        .functions
        .iter()
        .map(|(_, function)| printer.function(function))
        .collect::<Vec<_>>()
        .join("\n")
}

fn compile(src: &str) -> (crate::parser::ProgramAst<'_>, Program) {
    let mut ast = parse(src).expect("The test program parses.");
    analyze(&mut ast).expect("The test program type-checks.");
    let program = desugar(&ast);
    for (function_id, function) in program.functions.iter() {
        assert_eq!(verify(function), Ok(()), "function {function_id:?}");
    }
    (ast, program)
}

/// The entry function's `result` section, unindented: what the program evaluates to.
fn result_of(src: &str) -> String {
    let printed = desugared(src);
    let (_, result) = printed
        .rsplit_once("result\n")
        .expect("The program has a result.");
    result
        .lines()
        .map(|line| format!("{}\n", line.strip_prefix("  ").unwrap_or(line)))
        .collect()
}

// ---------------------------------------------------------------------------
// Operators and promotions
// ---------------------------------------------------------------------------

#[test]
fn the_simplest_program_is_one_result() {
    assert_eq!(
        desugared("1 + 2"),
        indoc! {"
            function f0 (0 parameters), structured
            body
            result
              add: int
                constant 1: int
                constant 2: int
        "}
    );
}

#[test]
fn every_implicit_promotion_becomes_an_explicit_convert() {
    let cases = [
        (
            "1 + 2.5",
            indoc! {"
                add: float
                  convert: float
                    constant 1: int
                  constant 2.5: float
            "},
        ),
        (
            "true + true",
            indoc! {"
                add: int
                  convert: int
                    constant true: bool
                  convert: int
                    constant true: bool
            "},
        ),
        (
            "7 / 2",
            indoc! {"
                divide: float
                  convert: float
                    constant 7: int
                  convert: float
                    constant 2: int
            "},
        ),
        (
            "2 ^ 0.5",
            indoc! {"
                power: float
                  convert: float
                    constant 2: int
                  constant 0.5: float
            "},
        ),
        (
            "2 ^ 10",
            indoc! {"
                power: int
                  constant 2: int
                  constant 10: int
            "},
        ),
        (
            "-true",
            indoc! {"
                negate: int
                  convert: int
                    constant true: bool
            "},
        ),
        (
            "1 < 2.5",
            indoc! {"
                less: bool
                  convert: float
                    constant 1: int
                  constant 2.5: float
            "},
        ),
        // Booleans compare as the integers 0 and 1, so `false < true`.
        (
            "false < true",
            indoc! {"
                less: bool
                  convert: int
                    constant false: bool
                  convert: int
                    constant true: bool
            "},
        ),
        (
            "true xor false",
            indoc! {"
                xor: bool
                  constant true: bool
                  constant false: bool
            "},
        ),
        (
            "not true",
            indoc! {"
                not: bool
                  constant true: bool
            "},
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(result_of(src), expected, "{src}");
    }
}

#[test]
fn div_and_mod_become_checked_integer_divisions() {
    assert_eq!(
        result_of("7 div 2"),
        "div checked: int\n  constant 7: int\n  constant 2: int\n"
    );
    assert_eq!(
        result_of("true mod 2"),
        indoc! {"
            mod checked: int
              convert: int
                constant true: bool
              constant 2: int
        "}
    );
}

#[test]
fn and_becomes_a_temporary_and_an_if() {
    assert_eq!(
        desugared(indoc! {"
            a = true
            b = false
            a and b
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: bool
            body
              write_global g0 a
                constant true: bool
              write_global g1 b
                constant false: bool
              write_local l0
                read_global g0 a: bool
              if
                read_local l0: bool
                then
                  write_local l0
                    read_global g1 b: bool
            result
              read_local l0: bool
        "}
    );
}

#[test]
fn or_evaluates_its_right_operand_only_when_the_left_is_false() {
    assert_eq!(
        desugared(indoc! {"
            a = true
            b = false
            a or b
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: bool
            body
              write_global g0 a
                constant true: bool
              write_global g1 b
                constant false: bool
              write_local l0
                read_global g0 a: bool
              if
                read_local l0: bool
                then
                else
                  write_local l0
                    read_global g1 b: bool
            result
              read_local l0: bool
        "}
    );
}

#[test]
fn and_nested_in_or_keeps_both_short_circuits() {
    // `and` binds tighter: `a or (b and c)`.
    assert_eq!(
        desugared(indoc! {"
            a = true
            b = false
            c = true
            a or b and c
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: bool
              l1: bool
            body
              write_global g0 a
                constant true: bool
              write_global g1 b
                constant false: bool
              write_global g2 c
                constant true: bool
              write_local l0
                read_global g0 a: bool
              if
                read_local l0: bool
                then
                else
                  write_local l1
                    read_global g1 b: bool
                  if
                    read_local l1: bool
                    then
                      write_local l1
                        read_global g2 c: bool
                  write_local l0
                    read_local l1: bool
            result
              read_local l0: bool
        "}
    );
}

// ---------------------------------------------------------------------------
// Evaluation order
// ---------------------------------------------------------------------------

#[test]
fn an_operand_left_of_hoisted_code_is_saved_first() {
    // The `if` runs as statements before the `add`; `x` is read into a temporary ahead of it, so
    // it is still read first, as the source says.
    assert_eq!(
        desugared(indoc! {"
            x = 1
            c = true
            x + (2 if c else 3)
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
              l1: int
            body
              write_global g0 x
                constant 1: int
              write_global g1 c
                constant true: bool
              write_local l1
                read_global g0 x: int
              if
                read_global g1 c: bool
                then
                  write_local l0
                    constant 2: int
                else
                  write_local l0
                    constant 3: int
            result
              add: int
                read_local l1: int
                read_local l0: int
        "}
    );
}

#[test]
fn a_constant_left_of_hoisted_code_needs_no_temporary() {
    assert_eq!(
        result_of(indoc! {"
            c = true
            1 + (2 if c else 3)
        "}),
        "add: int\n  constant 1: int\n  read_local l0: int\n"
    );
}

#[test]
fn call_arguments_keep_their_order_around_hoisted_code() {
    assert_eq!(
        desugared(indoc! {"
            f(a: int, b: int): int is a - b
            x = 5
            c = true
            f(x, 1 if c else 2)
        "}),
        indoc! {"
            function f0 f (2 parameters), structured
            locals
              l0 a: int
              l1 b: int
            body
            result
              subtract: int
                read_local l0 a: int
                read_local l1 b: int

            function f1 (0 parameters), structured
            locals
              l0: int
              l1: int
            body
              write_global g0 x
                constant 5: int
              write_global g1 c
                constant true: bool
              write_local l1
                read_global g0 x: int
              if
                read_global g1 c: bool
                then
                  write_local l0
                    constant 1: int
                else
                  write_local l0
                    constant 2: int
            result
              call f0 f: int
                read_local l1: int
                read_local l0: int
        "}
    );
}

// ---------------------------------------------------------------------------
// Assignment
// ---------------------------------------------------------------------------

#[test]
fn a_program_ending_in_an_assignment_evaluates_to_the_value_assigned() {
    assert_eq!(
        desugared("x = 1"),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
            body
              write_local l0
                constant 1: int
              write_global g0 x
                read_local l0: int
            result
              read_local l0: int
        "}
    );
}

#[test]
fn an_assignment_converts_to_the_target_but_the_program_keeps_the_value_type() {
    assert_eq!(
        desugared(indoc! {"
            x = 1.5
            x = 2
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
            body
              write_global g0 x
                constant 1.5: float
              write_local l0
                constant 2: int
              write_global g0 x
                convert: float
                  read_local l0: int
            result
              read_local l0: int
        "}
    );
}

#[test]
fn a_swap_evaluates_every_value_before_any_store() {
    assert_eq!(
        desugared(indoc! {"
            a, b = 1, 2
            a, b = b, a
            a
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
              l1: int
              l2: int
              l3: int
            body
              write_local l0
                constant 1: int
              write_local l1
                constant 2: int
              write_global g0 a
                read_local l0: int
              write_global g1 b
                read_local l1: int
              write_local l2
                read_global g1 b: int
              write_local l3
                read_global g0 a: int
              write_global g0 a
                read_local l2: int
              write_global g1 b
                read_local l3: int
            result
              read_global g0 a: int
        "}
    );
}

#[test]
fn a_chained_assignment_evaluates_its_value_once() {
    assert_eq!(
        desugared(indoc! {"
            a = b = 3
            a + b
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
            body
              write_local l0
                constant 3: int
              write_global g0 a
                read_local l0: int
              write_global g1 b
                read_local l0: int
            result
              add: int
                read_global g0 a: int
                read_global g1 b: int
        "}
    );
}

// ---------------------------------------------------------------------------
// Functions and calls
// ---------------------------------------------------------------------------

#[test]
fn arguments_and_results_convert_to_the_declared_types() {
    assert_eq!(
        desugared(indoc! {"
            half(x: float): float is x / 2
            one(): float is 1
            half(3) + one()
        "}),
        indoc! {"
            function f0 half (1 parameter), structured
            locals
              l0 x: float
            body
            result
              divide: float
                read_local l0 x: float
                convert: float
                  constant 2: int

            function f1 one (0 parameters), structured
            body
            result
              convert: float
                constant 1: int

            function f2 (0 parameters), structured
            body
            result
              add: float
                call f0 half: float
                  convert: float
                    constant 3: int
                call f1 one: float
        "}
    );
}

#[test]
fn a_void_function_evaluates_its_last_expression_for_effect() {
    assert_eq!(
        desugared(indoc! {"
            count = 0
            bump() is
                if true
                    ::count += 1
            twice() is
                bump()
                bump()
            twice()
            count
        "}),
        indoc! {"
            function f0 bump (0 parameters), structured
            body
              if
                constant true: bool
                then
                  write_global g0 count
                    add: int
                      read_global g0 count: int
                      constant 1: int

            function f1 twice (0 parameters), structured
            body
              evaluate
                call f0 bump
              evaluate
                call f0 bump

            function f2 (0 parameters), structured
            body
              write_global g0 count
                constant 0: int
              evaluate
                call f1 twice
            result
              read_global g0 count: int
        "}
    );
}

#[test]
fn a_void_function_ending_in_an_if_runs_it_as_a_statement() {
    assert_eq!(
        desugared(indoc! {"
            count = 0
            touch(c: bool) is
                if c
                    ::count = 1
            touch(true)
        "}),
        indoc! {"
            function f0 touch (1 parameter), structured
            locals
              l0 c: bool
            body
              if
                read_local l0 c: bool
                then
                  write_global g0 count
                    constant 1: int

            function f1 (0 parameters), structured
            body
              write_global g0 count
                constant 0: int
              evaluate
                call f0 touch
                  constant true: bool
        "}
    );
}

#[test]
fn the_tables_record_every_signature_and_global() {
    let (ast, program) = compile(indoc! {"
        g = 1.5
        f(a: int, b: bool): float is 1.0
        f(1, true)
    "});
    let name = |sym_id: crate::parser::SymId| ast.names[sym_id.value()];
    let types = &program.tables.types;
    let signatures: Vec<_> = program.tables.signatures.iter().map(|(_, s)| s).collect();
    assert_eq!(signatures.len(), 2);
    assert_eq!(signatures[0].name.map(name), Some("f"));
    let parameters: Vec<PinpType> = signatures[0]
        .parameters
        .iter()
        .map(|&type_id| types.get(type_id))
        .collect();
    assert_eq!(parameters, [PinpType::Int, PinpType::Bool]);
    assert_eq!(types.get(signatures[0].result), PinpType::Float);
    // The entry is last, unnamed, and returns the program's value.
    assert_eq!(signatures[1].name, None);
    assert!(signatures[1].parameters.is_empty());
    assert_eq!(types.get(signatures[1].result), PinpType::Float);
    assert_eq!(program.entry.index(), 1);
    let globals: Vec<_> = program.tables.globals.iter().map(|(_, g)| g).collect();
    assert_eq!(globals.len(), 1);
    assert_eq!(name(globals[0].name), "g");
    assert_eq!(types.get(globals[0].type_id), PinpType::Float);
}

#[test]
fn a_program_ending_in_a_statement_or_a_definition_has_no_result() {
    for src in ["x = 1\nwhile false\n    x = 2\n", "1 + 1\nf(): int is 0\n"] {
        let printed = desugared(src);
        let entry = printed.rsplit("\n\n").next().unwrap_or_default();
        assert!(!entry.contains("result"), "{printed}");
    }
}

// ---------------------------------------------------------------------------
// if
// ---------------------------------------------------------------------------

#[test]
fn an_elif_chain_nests_in_the_else_body() {
    assert_eq!(
        desugared(indoc! {"
            classify(x: int): int is
                if x > 0
                    1
                elif x < 0
                    0
                else
                    2
            classify(-5)
        "}),
        indoc! {"
            function f0 classify (1 parameter), structured
            locals
              l0 x: int
              l1: int
            body
              if
                greater: bool
                  read_local l0 x: int
                  constant 0: int
                then
                  write_local l1
                    constant 1: int
                else
                  if
                    less: bool
                      read_local l0 x: int
                      constant 0: int
                    then
                      write_local l1
                        constant 0: int
                    else
                      write_local l1
                        constant 2: int
            result
              read_local l1: int

            function f1 (0 parameters), structured
            body
            result
              call f0 classify: int
                negate: int
                  constant 5: int
        "}
    );
}

#[test]
fn if_branches_convert_to_their_joined_type() {
    assert_eq!(
        desugared(indoc! {"
            c = true
            1 if c else 2.5
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: float
            body
              write_global g0 c
                constant true: bool
              if
                read_global g0 c: bool
                then
                  write_local l0
                    convert: float
                      constant 1: int
                else
                  write_local l0
                    constant 2.5: float
            result
              read_local l0: float
        "}
    );
}

#[test]
fn an_if_with_a_value_used_as_a_statement_evaluates_its_branches_for_effect() {
    assert_eq!(
        desugared(indoc! {"
            c = true
            1 if c else 2
            0
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            body
              write_global g0 c
                constant true: bool
              if
                read_global g0 c: bool
                then
                  evaluate
                    constant 1: int
                else
                  evaluate
                    constant 2: int
            result
              constant 0: int
        "}
    );
}

// ---------------------------------------------------------------------------
// Loops
// ---------------------------------------------------------------------------

#[test]
fn a_while_condition_that_needs_statements_gets_a_header() {
    assert_eq!(
        desugared(indoc! {"
            f(n: int): int is
                i = 0
                while i < n and i < 10
                    i += 1
                i
            f(3)
        "}),
        indoc! {"
            function f0 f (1 parameter), structured
            locals
              l0 n: int
              l1 i: int
              l2: bool
            body
              write_local l1 i
                constant 0: int
              while
                header
                  write_local l2
                    less: bool
                      read_local l1 i: int
                      read_local l0 n: int
                  if
                    read_local l2: bool
                    then
                      write_local l2
                        less: bool
                          read_local l1 i: int
                          constant 10: int
                read_local l2: bool
                body
                  write_local l1 i
                    add: int
                      read_local l1 i: int
                      constant 1: int
            result
              read_local l1 i: int

            function f1 (0 parameters), structured
            body
            result
              call f0 f: int
                constant 3: int
        "}
    );
}

#[test]
fn a_loop_condition_that_needs_statements_computes_them_at_the_end_of_the_body() {
    assert_eq!(
        desugared(indoc! {"
            f(): int is
                i = 0
                loop
                    i += 1
                until i >= 3 or i < 0
                i
            f()
        "}),
        indoc! {"
            function f0 f (0 parameters), structured
            locals
              l0 i: int
              l1: bool
            body
              write_local l0 i
                constant 0: int
              do_until
                body
                  write_local l0 i
                    add: int
                      read_local l0 i: int
                      constant 1: int
                  write_local l1
                    greater_equal: bool
                      read_local l0 i: int
                      constant 3: int
                  if
                    read_local l1: bool
                    then
                    else
                      write_local l1
                        less: bool
                          read_local l0 i: int
                          constant 0: int
                read_local l1: bool
            result
              read_local l0 i: int

            function f1 (0 parameters), structured
            body
            result
              call f0 f: int
        "}
    );
}

#[test]
fn a_top_level_loop_updates_globals() {
    assert_eq!(
        desugared(indoc! {"
            i = 0
            while i < 3
                i += 1
            i
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            body
              write_global g0 i
                constant 0: int
              while
                less: bool
                  read_global g0 i: int
                  constant 3: int
                body
                  write_global g0 i
                    add: int
                      read_global g0 i: int
                      constant 1: int
            result
              read_global g0 i: int
        "}
    );
}

// ---------------------------------------------------------------------------
// Scopes
// ---------------------------------------------------------------------------

#[test]
fn a_name_new_to_a_block_is_a_local_of_that_block_even_at_the_top_level() {
    assert_eq!(
        desugared(indoc! {"
            c = true
            if c
                t = 1
            else
                t = 2
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0 t: int
              l1 t: int
            body
              write_global g0 c
                constant true: bool
              if
                read_global g0 c: bool
                then
                  write_local l0 t
                    constant 1: int
                else
                  write_local l1 t
                    constant 2: int
        "}
    );
}

#[test]
fn a_block_assigns_to_the_nearest_enclosing_binding() {
    let printed = desugared(indoc! {"
        pick(a: int, b: int): int is
            m = a
            if b > a
                m = b
            m
        pick(4, 7)
    "});
    assert!(
        printed.contains(indoc! {"
            locals
              l0 a: int
              l1 b: int
              l2 m: int
            body
              write_local l2 m
                read_local l0 a: int
              if
                greater: bool
                  read_local l1 b: int
                  read_local l0 a: int
                then
                  write_local l2 m
                    read_local l1 b: int
            result
              read_local l2 m: int
        "}),
        "{printed}"
    );
}

// ---------------------------------------------------------------------------
// Spans
// ---------------------------------------------------------------------------

/// The source text of every node of the entry function whose operation `matches`.
fn texts_of(src: &str, matches: impl Fn(&Operation) -> bool) -> Vec<&str> {
    let (_, program) = compile(src);
    let entry = &program.functions[program.entry];
    entry
        .arena
        .operations
        .iter()
        .filter(|(_, operation)| matches(operation))
        .map(|(node_id, _)| {
            let span = entry.arena.spans[node_id];
            &src[span.start as usize..span.end as usize]
        })
        .collect()
}

#[test]
fn a_convert_is_spanned_by_the_value_it_converts() {
    let src = "x = 1.5\ny = x + 2\n";
    assert_eq!(
        texts_of(src, |operation| matches!(
            operation,
            Operation::Convert { .. }
        )),
        ["2"]
    );
}

#[test]
fn a_write_is_spanned_by_its_target() {
    let src = "x = 1.5\nyy = x\nyy";
    assert_eq!(
        texts_of(src, |operation| matches!(
            operation,
            Operation::WriteGlobal { .. }
        )),
        ["x", "yy"]
    );
}

#[test]
fn code_made_for_and_is_spanned_by_the_whole_and() {
    let src = "a = true\nb = a and a\nb";
    let is_made_for_and = |operation: &Operation| {
        matches!(
            operation,
            Operation::If { .. } | Operation::WriteLocal { .. }
        )
    };
    assert_eq!(
        texts_of(src, is_made_for_and),
        ["a and a", "a and a", "a and a"]
    );
}

// ---------------------------------------------------------------------------
// Ranges, `in`, and `for` over a range (step 3b)
// ---------------------------------------------------------------------------

#[test]
fn a_for_over_a_range_becomes_a_counted_loop() {
    assert_eq!(
        desugared(indoc! {"
            total = 0
            for i in 1..3
                total += i
            total
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0 i: int
            body
              write_global g0 total
                constant 0: int
              counted_loop l0 i
                range inclusive: range
                  constant 1: int
                  constant 3: int
                body
                  write_global g0 total
                    add: int
                      read_global g0 total: int
                      read_local l0 i: int
            result
              read_global g0 total: int
        "}
    );
}

#[test]
fn a_range_keeps_its_kind_and_only_an_explicit_step() {
    let cases = [
        (
            "7 in 17..>1:-2",
            indoc! {"
                in: bool
                  constant 7: int
                  range down_exclusive: range
                    constant 17: int
                    constant 1: int
                    negate: int
                      constant 2: int
            "},
        ),
        (
            "s = 3\n7 in 1..10:s",
            indoc! {"
                in: bool
                  constant 7: int
                  range inclusive: range
                    constant 1: int
                    constant 10: int
                    read_global g0 s: int
            "},
        ),
        (
            "2 in 1..<4",
            indoc! {"
                in: bool
                  constant 2: int
                  range up_exclusive: range
                    constant 1: int
                    constant 4: int
            "},
        ),
    ];
    for (src, expected) in cases {
        assert_eq!(result_of(src), expected, "{src}");
    }
}

#[test]
fn boolean_bounds_and_members_convert_to_int() {
    assert_eq!(
        result_of("true in false..3"),
        indoc! {"
            in: bool
              convert: int
                constant true: bool
              range inclusive: range
                convert: int
                  constant false: bool
                constant 3: int
        "}
    );
}

#[test]
fn a_range_is_a_value_that_can_be_stored_and_reused() {
    assert_eq!(
        desugared(indoc! {"
            r = 1..<4
            total = 0
            for i in r
                total += i
            3 in r
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0 i: int
            body
              write_global g0 r
                range up_exclusive: range
                  constant 1: int
                  constant 4: int
              write_global g1 total
                constant 0: int
              counted_loop l0 i
                read_global g0 r: range
                body
                  write_global g1 total
                    add: int
                      read_global g1 total: int
                      read_local l0 i: int
            result
              in: bool
                constant 3: int
                read_global g0 r: range
        "}
    );
}

#[test]
fn nested_loops_each_get_their_own_index_and_body_locals() {
    assert_eq!(
        desugared(indoc! {"
            grid(n: int): int is
                count = 0
                for i in 0..<n
                    for j in 0..<i
                        cell = i + j
                        count += cell
                count
            grid(4)
        "}),
        indoc! {"
            function f0 grid (1 parameter), structured
            locals
              l0 n: int
              l1 count: int
              l2 i: int
              l3 j: int
              l4 cell: int
            body
              write_local l1 count
                constant 0: int
              counted_loop l2 i
                range up_exclusive: range
                  constant 0: int
                  read_local l0 n: int
                body
                  counted_loop l3 j
                    range up_exclusive: range
                      constant 0: int
                      read_local l2 i: int
                    body
                      write_local l4 cell
                        add: int
                          read_local l2 i: int
                          read_local l3 j: int
                      write_local l1 count
                        add: int
                          read_local l1 count: int
                          read_local l4 cell: int
            result
              read_local l1 count: int

            function f1 (0 parameters), structured
            body
            result
              call f0 grid: int
                constant 4: int
        "}
    );
}

#[test]
fn a_range_bound_left_of_hoisted_code_is_saved_first() {
    assert_eq!(
        desugared(indoc! {"
            c = true
            x = 1
            for i in x..(5 if c else 6)
                x += i
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
              l1: int
              l2 i: int
            body
              write_global g0 c
                constant true: bool
              write_global g1 x
                constant 1: int
              write_local l1
                read_global g1 x: int
              if
                read_global g0 c: bool
                then
                  write_local l0
                    constant 5: int
                else
                  write_local l0
                    constant 6: int
              counted_loop l2 i
                range inclusive: range
                  read_local l1: int
                  read_local l0: int
                body
                  write_global g1 x
                    add: int
                      read_global g1 x: int
                      read_local l2 i: int
        "}
    );
}

#[test]
fn a_counted_loop_is_spanned_by_what_it_iterates_over() {
    let src = "t = 0\nfor i in 1..3\n    t += i\nt";
    assert_eq!(
        texts_of(src, |operation| matches!(
            operation,
            Operation::CountedLoop { .. }
        )),
        ["1..3"]
    );
}

// ---------------------------------------------------------------------------
// Arrays (step 3c)
// ---------------------------------------------------------------------------

#[test]
fn an_array_literal_converts_every_element_to_the_element_type() {
    assert_eq!(
        result_of("[1, 2.5, true]"),
        indoc! {"
            array_new: float[3]
              convert: float
                constant 1: int
              constant 2.5: float
              convert: float
                constant true: bool
        "}
    );
}

#[test]
fn a_variable_index_counts_negatives_from_the_end_then_is_checked() {
    // `i < 0 ? i + 3 : i`, as a `select`; the access itself checks the result.
    assert_eq!(
        result_of("a = [10, 20, 30]\ni = 1\na[i]"),
        indoc! {"
            element_get checked: int
              read_global g0 a: int[3]
              select: int
                less: bool
                  read_global g1 i: int
                  constant 0: int
                add: int
                  read_global g1 i: int
                  constant 3: int
                read_global g1 i: int
        "}
    );
}

#[test]
fn an_index_that_is_not_a_plain_read_is_computed_once() {
    // Computing the index is a statement, so the array read ahead of it is saved first.
    assert_eq!(
        desugared(indoc! {"
            a = [10, 20, 30]
            i = 1
            a[i + 1]
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
              l1: int[3]
            body
              write_global g0 a
                array_new: int[3]
                  constant 10: int
                  constant 20: int
                  constant 30: int
              write_global g1 i
                constant 1: int
              write_local l1
                read_global g0 a: int[3]
              write_local l0
                add: int
                  read_global g1 i: int
                  constant 1: int
            result
              element_get checked: int
                read_local l1: int[3]
                select: int
                  less: bool
                    read_local l0: int
                    constant 0: int
                  add: int
                    read_local l0: int
                    constant 3: int
                  read_local l0: int
        "}
    );
}

#[test]
fn a_literal_index_sema_proved_in_range_is_neither_checked_nor_wrapped() {
    // Sema rejects an out-of-range literal, so the access needs no check, and a negative literal
    // is counted from the end here, once.
    assert_eq!(
        result_of("a = [10, 20, 30]\na[-1] + a[0]"),
        indoc! {"
            add: int
              element_get: int
                read_global g0 a: int[3]
                constant 2: int
              element_get: int
                read_global g0 a: int[3]
                constant 0: int
        "}
    );
}

#[test]
fn a_slice_keeps_its_literal_range() {
    assert_eq!(
        result_of("a = [10, 20, 30]\na[0..1]"),
        indoc! {"
            slice: int[2]
              read_global g0 a: int[3]
              range inclusive: range
                constant 0: int
                constant 1: int
        "}
    );
}

#[test]
fn array_members_are_constants_and_the_array_is_not_evaluated() {
    assert_eq!(
        result_of("a = [1, 2]\na.len + a.ndim"),
        "add: int\n  constant 2: int\n  constant 1: int\n"
    );
}

#[test]
fn an_element_assignment_converts_to_the_element_type() {
    assert_eq!(
        desugared(indoc! {"
            a = [1.5, 2.5]
            i = 1
            a[i] = 3
            a
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            body
              write_global g0 a
                array_new: float[2]
                  constant 1.5: float
                  constant 2.5: float
              write_global g1 i
                constant 1: int
              element_set checked
                read_global g0 a: float[2]
                select: int
                  less: bool
                    read_global g1 i: int
                    constant 0: int
                  add: int
                    read_global g1 i: int
                    constant 2: int
                  read_global g1 i: int
                convert: float
                  constant 3: int
            result
              read_global g0 a: float[2]
        "}
    );
}

#[test]
fn an_element_assignment_saves_the_array_and_index_before_hoisted_code() {
    assert_eq!(
        desugared(indoc! {"
            a = [1, 2]
            i = 0
            c = true
            a[i] = 3 if c else 4
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
              l1: int[2]
              l2: int
            body
              write_global g0 a
                array_new: int[2]
                  constant 1: int
                  constant 2: int
              write_global g1 i
                constant 0: int
              write_global g2 c
                constant true: bool
              write_local l1
                read_global g0 a: int[2]
              write_local l2
                select: int
                  less: bool
                    read_global g1 i: int
                    constant 0: int
                  add: int
                    read_global g1 i: int
                    constant 2: int
                  read_global g1 i: int
              if
                read_global g2 c: bool
                then
                  write_local l0
                    constant 3: int
                else
                  write_local l0
                    constant 4: int
              element_set checked
                read_local l1: int[2]
                read_local l2: int
                read_local l0: int
        "}
    );
}

#[test]
fn a_compound_element_assignment_evaluates_the_array_and_index_once() {
    // The index is wrapped once; the get checks it and the set reuses it, so only the get is checked.
    assert_eq!(
        desugared(indoc! {"
            a = [1, 2]
            i = 0
            a[i] += 5
            a
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int[2]
              l1: int
            body
              write_global g0 a
                array_new: int[2]
                  constant 1: int
                  constant 2: int
              write_global g1 i
                constant 0: int
              write_local l0
                read_global g0 a: int[2]
              write_local l1
                select: int
                  less: bool
                    read_global g1 i: int
                    constant 0: int
                  add: int
                    read_global g1 i: int
                    constant 2: int
                  read_global g1 i: int
              element_set
                read_local l0: int[2]
                read_local l1: int
                add: int
                  element_get checked: int
                    read_local l0: int[2]
                    read_local l1: int
                  constant 5: int
            result
              read_global g0 a: int[2]
        "}
    );
}

#[test]
fn a_compound_element_assignment_reads_the_element_before_hoisted_code() {
    let printed = desugared(indoc! {"
        f = [1.5]
        c = true
        f[0] += 1 if c else 2
        f
    "});
    assert_eq!(
        printed,
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: float[1]
              l1: int
              l2: int
              l3: float
            body
              write_global g0 f
                array_new: float[1]
                  constant 1.5: float
              write_global g1 c
                constant true: bool
              write_local l0
                read_global g0 f: float[1]
              write_local l1
                constant 0: int
              write_local l3
                element_get: float
                  read_local l0: float[1]
                  read_local l1: int
              if
                read_global g1 c: bool
                then
                  write_local l2
                    constant 1: int
                else
                  write_local l2
                    constant 2: int
              element_set
                read_local l0: float[1]
                read_local l1: int
                add: float
                  read_local l3: float
                  convert: float
                    read_local l2: int
            result
              read_global g0 f: float[1]
        "}
    );
}

#[test]
fn a_slice_assignment_fills_the_slice_with_one_value() {
    assert_eq!(
        desugared(indoc! {"
            a = [1, 2, 3, 4]
            a[1..2] = true
            a
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int
              l1: int
            body
              write_global g0 a
                array_new: int[4]
                  constant 1: int
                  constant 2: int
                  constant 3: int
                  constant 4: int
              write_local l0
                convert: int
                  constant true: bool
              counted_loop l1
                range inclusive: range
                  constant 1: int
                  constant 2: int
                body
                  element_set
                    read_global g0 a: int[4]
                    read_local l1: int
                    read_local l0: int
            result
              read_global g0 a: int[4]
        "}
    );
}

#[test]
fn a_for_over_an_array_reads_each_element_into_the_loop_variable() {
    assert_eq!(
        desugared(indoc! {"
            total = 0
            for v in [3, 4]
                total += v
            total
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int[2]
              l1: int
              l2 v: int
            body
              write_global g0 total
                constant 0: int
              write_local l0
                array_new: int[2]
                  constant 3: int
                  constant 4: int
              counted_loop l1
                range up_exclusive: range
                  constant 0: int
                  constant 2: int
                body
                  write_local l2 v
                    element_get: int
                      read_local l0: int[2]
                      read_local l1: int
                  write_global g0 total
                    add: int
                      read_global g0 total: int
                      read_local l2 v: int
            result
              read_global g0 total: int
        "}
    );
}

#[test]
fn a_for_with_an_index_binder_counts_with_it() {
    assert_eq!(
        desugared(indoc! {"
            a = [5, 6]
            s = 0
            for i, v in a
                s += i * v
            s
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int[2]
              l1 i: int
              l2 v: int
            body
              write_global g0 a
                array_new: int[2]
                  constant 5: int
                  constant 6: int
              write_global g1 s
                constant 0: int
              write_local l0
                read_global g0 a: int[2]
              counted_loop l1 i
                range up_exclusive: range
                  constant 0: int
                  constant 2: int
                body
                  write_local l2 v
                    element_get: int
                      read_local l0: int[2]
                      read_local l1 i: int
                  write_global g1 s
                    add: int
                      read_global g1 s: int
                      multiply: int
                        read_local l1 i: int
                        read_local l2 v: int
            result
              read_global g1 s: int
        "}
    );
}

#[test]
fn an_array_from_a_range_is_filled_by_a_counted_loop() {
    assert_eq!(
        desugared("[1..5:2]"),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int[3]
              l1: int
              l2: int
            body
              write_local l0
                array_allocate: int[3]
              write_local l1
                constant 0: int
              counted_loop l2
                range inclusive: range
                  constant 1: int
                  constant 5: int
                  constant 2: int
                body
                  element_set
                    read_local l0: int[3]
                    read_local l1: int
                    read_local l2: int
                  write_local l1
                    add: int
                      read_local l1: int
                      constant 1: int
            result
              read_local l0: int[3]
        "}
    );
}

#[test]
fn a_comprehension_loops_with_its_variable_as_the_index() {
    assert_eq!(
        desugared("[i * i for i in 1..3]"),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int[3]
              l1: int
              l2 i: int
            body
              write_local l0
                array_allocate: int[3]
              write_local l1
                constant 0: int
              counted_loop l2 i
                range inclusive: range
                  constant 1: int
                  constant 3: int
                body
                  element_set
                    read_local l0: int[3]
                    read_local l1: int
                    multiply: int
                      read_local l2 i: int
                      read_local l2 i: int
                  write_local l1
                    add: int
                      read_local l1: int
                      constant 1: int
            result
              read_local l0: int[3]
        "}
    );
}

#[test]
fn a_float_comprehension_variable_is_the_converted_index() {
    assert_eq!(
        desugared("[x * 2 for x: float in 0..<3]"),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: float[3]
              l1: int
              l2: int
              l3 x: float
            body
              write_local l0
                array_allocate: float[3]
              write_local l1
                constant 0: int
              counted_loop l2
                range up_exclusive: range
                  constant 0: int
                  constant 3: int
                body
                  write_local l3 x
                    convert: float
                      read_local l2: int
                  element_set
                    read_local l0: float[3]
                    read_local l1: int
                    multiply: float
                      read_local l3 x: float
                      convert: float
                        constant 2: int
                  write_local l1
                    add: int
                      read_local l1: int
                      constant 1: int
            result
              read_local l0: float[3]
        "}
    );
}

#[test]
fn code_made_for_a_for_over_an_array_is_spanned_by_the_array() {
    let src = "t = 0\nfor v in [3, 4]\n    t += v\nt";
    assert_eq!(
        texts_of(src, |operation| matches!(
            operation,
            Operation::CountedLoop { .. } | Operation::ElementGet { .. } | Operation::Range { .. }
        )),
        ["[3, 4]", "[3, 4]", "[3, 4]"]
    );
}

// ---------------------------------------------------------------------------
// Matrices (step 3d)
// ---------------------------------------------------------------------------

#[test]
fn a_matrix_literal_is_its_elements_row_by_row() {
    assert_eq!(
        result_of("[1, 2.5; 3, 4]"),
        indoc! {"
            array_new: float[2x2]
              convert: float
                constant 1: int
              constant 2.5: float
              convert: float
                constant 3: int
              convert: float
                constant 4: int
        "}
    );
}

#[test]
fn each_variable_matrix_index_wraps_against_its_own_dimension() {
    assert_eq!(
        result_of("m = [1, 2, 3; 4, 5, 6]\nr = 1\nc = 0\nm[r, c]"),
        indoc! {"
            element_get checked: int
              read_global g0 m: int[2x3]
              select: int
                less: bool
                  read_global g1 r: int
                  constant 0: int
                add: int
                  read_global g1 r: int
                  constant 2: int
                read_global g1 r: int
              select: int
                less: bool
                  read_global g2 c: int
                  constant 0: int
                add: int
                  read_global g2 c: int
                  constant 3: int
                read_global g2 c: int
        "}
    );
}

#[test]
fn literal_matrix_indices_are_resolved_and_need_no_check_unless_one_is_variable() {
    assert_eq!(
        result_of("m = [1, 2, 3; 4, 5, 6]\nm[-1, 0]"),
        indoc! {"
            element_get: int
              read_global g0 m: int[2x3]
              constant 1: int
              constant 0: int
        "}
    );
    assert_eq!(
        result_of("m = [1, 2, 3; 4, 5, 6]\nr = 1\nm[r, -1]"),
        indoc! {"
            element_get checked: int
              read_global g0 m: int[2x3]
              select: int
                less: bool
                  read_global g1 r: int
                  constant 0: int
                add: int
                  read_global g1 r: int
                  constant 2: int
                read_global g1 r: int
              constant 2: int
        "}
    );
}

#[test]
fn a_row_slice_wraps_its_row_and_spells_out_a_full_extent() {
    // `m[-1, :]` is the last row. (Today's codegen rejects it at run time; see the notebook.)
    assert_eq!(
        result_of("m = [1, 2; 3, 4]\nm[-1, :]"),
        indoc! {"
            slice: int[2]
              read_global g0 m: int[2x2]
              constant 1: int
              range up_exclusive: range
                constant 0: int
                constant 2: int
        "}
    );
}

#[test]
fn a_column_slice_wraps_a_variable_column() {
    assert_eq!(
        result_of("m = [1, 2; 3, 4]\nc = 1\nm[0..1, c]"),
        indoc! {"
            slice: int[2]
              read_global g0 m: int[2x2]
              range inclusive: range
                constant 0: int
                constant 1: int
              select: int
                less: bool
                  read_global g1 c: int
                  constant 0: int
                add: int
                  read_global g1 c: int
                  constant 2: int
                read_global g1 c: int
        "}
    );
}

#[test]
fn a_submatrix_has_two_ranges() {
    assert_eq!(
        result_of("m = [1, 2; 3, 4]\nm[:, 0..<1]"),
        indoc! {"
            slice: int[2x1]
              read_global g0 m: int[2x2]
              range up_exclusive: range
                constant 0: int
                constant 2: int
              range up_exclusive: range
                constant 0: int
                constant 1: int
        "}
    );
}

#[test]
fn matrix_members_are_constants() {
    assert_eq!(
        result_of("m = [1, 2, 3; 4, 5, 6]\nm.rows * 10 + m.cols + m.len + m.ndim"),
        indoc! {"
            add: int
              add: int
                add: int
                  multiply: int
                    constant 2: int
                    constant 10: int
                  constant 3: int
                constant 6: int
              constant 2: int
        "}
    );
}

#[test]
fn a_matrix_element_assignment_wraps_both_indices() {
    assert_eq!(
        desugared(indoc! {"
            m = [1.5, 2.5; 3.5, 4.5]
            r = 0
            m[r, -1] = 7
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            body
              write_global g0 m
                array_new: float[2x2]
                  constant 1.5: float
                  constant 2.5: float
                  constant 3.5: float
                  constant 4.5: float
              write_global g1 r
                constant 0: int
              element_set checked
                read_global g0 m: float[2x2]
                select: int
                  less: bool
                    read_global g1 r: int
                    constant 0: int
                  add: int
                    read_global g1 r: int
                    constant 2: int
                  read_global g1 r: int
                constant 1: int
                convert: float
                  constant 7: int
        "}
    );
}

#[test]
fn a_compound_matrix_element_assignment_evaluates_the_matrix_and_indices_once() {
    assert_eq!(
        desugared(indoc! {"
            m = [1, 2; 3, 4]
            m[1, 0] *= 5
            m
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int[2x2]
              l1: int
              l2: int
            body
              write_global g0 m
                array_new: int[2x2]
                  constant 1: int
                  constant 2: int
                  constant 3: int
                  constant 4: int
              write_local l0
                read_global g0 m: int[2x2]
              write_local l1
                constant 1: int
              write_local l2
                constant 0: int
              element_set
                read_local l0: int[2x2]
                read_local l1: int
                read_local l2: int
                multiply: int
                  element_get: int
                    read_local l0: int[2x2]
                    read_local l1: int
                    read_local l2: int
                  constant 5: int
            result
              read_global g0 m: int[2x2]
        "}
    );
}

#[test]
fn a_for_over_a_matrix_nests_a_row_loop_and_a_column_loop() {
    assert_eq!(
        desugared(indoc! {"
            m = [1, 2; 3, 4]
            total = 0
            for v in m
                total += v
            total
        "}),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: int[2x2]
              l1: int
              l2: int
              l3 v: int
            body
              write_global g0 m
                array_new: int[2x2]
                  constant 1: int
                  constant 2: int
                  constant 3: int
                  constant 4: int
              write_global g1 total
                constant 0: int
              write_local l0
                read_global g0 m: int[2x2]
              counted_loop l1
                range up_exclusive: range
                  constant 0: int
                  constant 2: int
                body
                  counted_loop l2
                    range up_exclusive: range
                      constant 0: int
                      constant 2: int
                    body
                      write_local l3 v
                        element_get: int
                          read_local l0: int[2x2]
                          read_local l1: int
                          read_local l2: int
                      write_global g1 total
                        add: int
                          read_global g1 total: int
                          read_local l3 v: int
            result
              read_global g1 total: int
        "}
    );
}

#[test]
fn a_for_with_row_and_column_binders_counts_with_them() {
    let printed = desugared(indoc! {"
        m = [1, 2; 3, 4]
        s = 0
        for r, c, v in m
            s += r * c * v
        s
    "});
    assert!(
        printed.contains("  l1 r: int\n  l2 c: int\n  l3 v: int\n"),
        "{printed}"
    );
    assert!(printed.contains("counted_loop l1 r\n"), "{printed}");
    assert!(printed.contains("counted_loop l2 c\n"), "{printed}");
}

#[test]
fn identity_zeroes_a_matrix_and_sets_its_diagonal_in_a_loop() {
    assert_eq!(
        desugared("identity(3, float)"),
        indoc! {"
            function f0 (0 parameters), structured
            locals
              l0: float[3x3]
              l1: int
            body
              write_local l0
                array_allocate zeroed: float[3x3]
              counted_loop l1
                range up_exclusive: range
                  constant 0: int
                  constant 3: int
                body
                  element_set
                    read_local l0: float[3x3]
                    read_local l1: int
                    read_local l1: int
                    constant 1.0: float
            result
              read_local l0: float[3x3]
        "}
    );
}

// ---------------------------------------------------------------------------
// Strings and built-ins (step 3e)
// ---------------------------------------------------------------------------

#[test]
fn a_string_literal_is_a_constant() {
    assert_eq!(result_of("'hi'"), "constant \"hi\": str\n");
}

#[test]
fn a_concatenation_chain_is_one_join_with_scalars_converted() {
    assert_eq!(
        desugared("s = 'a'\ns + 1 + 'b'"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: str
              l1: str
              l2: str
            body
              evaluate
                runtime_call str_free
                  read_global g0 s: str
              write_global g0 s
                constant "a": str
              write_local l0
                convert: str
                  constant 1: int
              write_local l1
                constant "b": str
              write_local l2
                runtime_call str_concat: str
                  read_global g0 s: str
                  read_local l0: str
                  read_local l1: str
              evaluate
                runtime_call str_free
                  read_local l0: str
              evaluate
                runtime_call str_free
                  read_local l1: str
              evaluate
                runtime_call str_free
                  read_global g0 s: str
            result
              read_local l2: str
        "#}
    );
}

#[test]
fn a_parenthesised_concatenation_on_the_right_is_one_part() {
    assert_eq!(
        desugared("'a' + ('b' + 'c')"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: str
              l1: str
              l2: str
              l3: str
              l4: str
            body
              write_local l0
                constant "b": str
              write_local l1
                constant "c": str
              write_local l2
                runtime_call str_concat: str
                  read_local l0: str
                  read_local l1: str
              evaluate
                runtime_call str_free
                  read_local l0: str
              evaluate
                runtime_call str_free
                  read_local l1: str
              write_local l3
                constant "a": str
              write_local l4
                runtime_call str_concat: str
                  read_local l3: str
                  read_local l2: str
              evaluate
                runtime_call str_free
                  read_local l3: str
              evaluate
                runtime_call str_free
                  read_local l2: str
            result
              read_local l4: str
        "#}
    );
}

#[test]
fn strings_compare_without_conversion() {
    assert_eq!(
        desugared("'a' < 'b'"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: str
              l1: str
              l2: bool
            body
              write_local l0
                constant "a": str
              write_local l1
                constant "b": str
              write_local l2
                less: bool
                  read_local l0: str
                  read_local l1: str
              evaluate
                runtime_call str_free
                  read_local l0: str
              evaluate
                runtime_call str_free
                  read_local l1: str
            result
              read_local l2: bool
        "#}
    );
}

#[test]
fn str_converts_a_scalar_and_leaves_a_string_as_it_is() {
    let cases = [
        ("str(2.5)", "convert: str\n  constant 2.5: float\n"),
        ("str(true)", "convert: str\n  constant true: bool\n"),
        ("str('x')", "constant \"x\": str\n"),
    ];
    for (src, expected) in cases {
        assert_eq!(result_of(src), expected, "{src}");
    }
}

#[test]
fn an_f_string_joins_its_literal_runs_and_rendered_holes() {
    assert_eq!(
        desugared("x = 3\nf'x={x}!'"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: str
              l1: str
              l2: str
              l3: str
            body
              write_global g0 x
                constant 3: int
              write_local l0
                constant "x=": str
              write_local l1
                convert: str
                  read_global g0 x: int
              write_local l2
                constant "!": str
              write_local l3
                runtime_call str_concat: str
                  read_local l0: str
                  read_local l1: str
                  read_local l2: str
              evaluate
                runtime_call str_free
                  read_local l0: str
              evaluate
                runtime_call str_free
                  read_local l1: str
              evaluate
                runtime_call str_free
                  read_local l2: str
            result
              read_local l3: str
        "#}
    );
}

#[test]
fn f_strings_of_one_part_or_none() {
    // A string hole alone still makes a new string, as today; the join of nothing is the empty
    // string.
    let joined =
        "  write_local l0\n    runtime_call str_concat: str\n      read_global g0 s: str\n";
    assert!(desugared("s = 'a'\nf'{s}'").contains(joined));
    assert_eq!(result_of("f'hi'"), "constant \"hi\": str\n");
    assert_eq!(result_of("f''"), "runtime_call str_concat: str\n");
}

#[test]
fn an_f_string_hole_can_name_a_global_inside_a_function() {
    let printed = desugared(indoc! {"
        count = 7
        report(): str is f'n={::count}'
        report()
    "});
    let rendered = "  write_local l1\n    convert: str\n      read_global g0 count: int\n";
    assert!(printed.contains(rendered), "{printed}");
}

#[test]
fn a_string_length_is_asked_of_the_runtime() {
    assert_eq!(
        desugared("s = 'abc'\ns.len"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: int
            body
              evaluate
                runtime_call str_free
                  read_global g0 s: str
              write_global g0 s
                constant "abc": str
              write_local l0
                runtime_call str_length: int
                  read_global g0 s: str
              evaluate
                runtime_call str_free
                  read_global g0 s: str
            result
              read_local l0: int
        "#}
    );
}

#[test]
fn meminfo_is_a_runtime_call_evaluated_for_effect() {
    assert_eq!(
        desugared("meminfo()"),
        indoc! {"
            function f0 (0 parameters), structured
            body
              evaluate
                runtime_call meminfo
        "}
    );
}

#[test]
fn a_for_value_binder_is_written_with_its_own_span() {
    let src = "t = 0\nfor v in [3, 4]\n    t += v\nt";
    let writes = texts_of(src, |operation| {
        matches!(operation, Operation::WriteLocal { .. })
    });
    assert_eq!(
        writes,
        ["[3, 4]", "v"],
        "the saved array, then the value binder"
    );
}

#[test]
fn a_matrix_value_binder_is_written_with_its_own_span() {
    let src = "m = [1, 2; 3, 4]\nt = 0\nfor r, c, value in m\n    t += value\nt";
    let writes = texts_of(src, |operation| {
        matches!(operation, Operation::WriteLocal { .. })
    });
    assert_eq!(writes.last(), Some(&"value"));
}

#[test]
fn a_float_comprehension_variable_is_written_with_its_own_span() {
    let src = "[x * 2.0 for x: float in 0..3]";
    let writes = texts_of(src, |operation| {
        matches!(operation, Operation::WriteLocal { .. })
    });
    assert_eq!(
        writes.iter().filter(|&&text| text == "x").count(),
        1,
        "{writes:?}"
    );
}

// ---------------------------------------------------------------------------
// String ownership (0017-F)
// ---------------------------------------------------------------------------

#[test]
fn a_string_store_frees_the_old_value_first() {
    assert_eq!(
        desugared("s = 'abc'\n0"),
        indoc! {r#"
            function f0 (0 parameters), structured
            body
              evaluate
                runtime_call str_free
                  read_global g0 s: str
              write_global g0 s
                constant "abc": str
              evaluate
                runtime_call str_free
                  read_global g0 s: str
            result
              constant 0: int
        "#}
    );
}

#[test]
fn storing_a_borrowed_string_copies_it() {
    assert_eq!(
        desugared("a = 'abc'\nb = a\nb.len"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: int
            body
              evaluate
                runtime_call str_free
                  read_global g0 a: str
              write_global g0 a
                constant "abc": str
              evaluate
                runtime_call str_free
                  read_global g1 b: str
              write_global g1 b
                runtime_call str_copy: str
                  read_global g0 a: str
              write_local l0
                runtime_call str_length: int
                  read_global g1 b: str
              evaluate
                runtime_call str_free
                  read_global g0 a: str
              evaluate
                runtime_call str_free
                  read_global g1 b: str
            result
              read_local l0: int
        "#}
    );
}

#[test]
fn a_value_reading_its_own_target_is_computed_before_the_old_value_is_freed() {
    assert_eq!(
        desugared("s = 'a'\ns = s + s\ns.len"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: str
              l1: int
            body
              evaluate
                runtime_call str_free
                  read_global g0 s: str
              write_global g0 s
                constant "a": str
              write_local l0
                runtime_call str_concat: str
                  read_global g0 s: str
                  read_global g0 s: str
              evaluate
                runtime_call str_free
                  read_global g0 s: str
              write_global g0 s
                read_local l0: str
              write_local l1
                runtime_call str_length: int
                  read_global g0 s: str
              evaluate
                runtime_call str_free
                  read_global g0 s: str
            result
              read_local l1: int
        "#}
    );
}

#[test]
fn an_owned_operand_is_freed_once_consumed_and_a_discarded_string_at_once() {
    assert_eq!(
        desugared("'abc' + 'def'\n0"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: str
              l1: str
              l2: str
            body
              write_local l0
                constant "abc": str
              write_local l1
                constant "def": str
              write_local l2
                runtime_call str_concat: str
                  read_local l0: str
                  read_local l1: str
              evaluate
                runtime_call str_free
                  read_local l0: str
              evaluate
                runtime_call str_free
                  read_local l1: str
              evaluate
                runtime_call str_free
                  read_local l2: str
            result
              constant 0: int
        "#}
    );
}

#[test]
fn a_function_frees_its_string_locals_and_copies_a_borrowed_result() {
    let printed = desugared("f(): str is\n    s = 'abc'\n    s\nf()");
    let function = printed.split("\nfunction f1").next().unwrap();
    assert_eq!(
        function,
        indoc! {r#"
            function f0 f (0 parameters), structured
            locals
              l0 s: str
              l1: str
            body
              evaluate
                runtime_call str_free
                  read_local l0 s: str
              write_local l0 s
                constant "abc": str
              write_local l1
                runtime_call str_copy: str
                  read_local l0 s: str
              evaluate
                runtime_call str_free
                  read_local l0 s: str
            result
              read_local l1: str
        "#}
    );
}

#[test]
fn an_owned_result_is_moved_out_uncopied() {
    let printed = desugared("g(): str is 'x' + 'y'\ng()");
    assert!(!printed.contains("str_copy"), "{printed}");
}

#[test]
fn the_entry_copies_a_global_it_returns_before_freeing_it() {
    assert_eq!(
        desugared("g = 'abc'\ng"),
        indoc! {r#"
            function f0 (0 parameters), structured
            locals
              l0: str
            body
              evaluate
                runtime_call str_free
                  read_global g0 g: str
              write_global g0 g
                constant "abc": str
              write_local l0
                runtime_call str_copy: str
                  read_global g0 g: str
              evaluate
                runtime_call str_free
                  read_global g0 g: str
            result
              read_local l0: str
        "#}
    );
}

#[test]
fn a_discarded_call_result_is_freed() {
    let printed = desugared("g(): str is 'x'\ng()\n0");
    let freed = "  evaluate\n    runtime_call str_free\n      call f0 g: str\n";
    assert!(printed.contains(freed), "{printed}");
}

#[test]
fn a_borrowed_operand_before_a_call_is_copied_so_the_call_cannot_free_it() {
    // `f` rebinds `::g`, freeing the value the concatenation was about to read; the read is
    // therefore copied before the call runs.
    let printed = desugared("g = 'a'\nf(): str is\n    ::g = 'b'\n    'c'\ng + f()");
    let entry = printed.split("function f1").nth(1).unwrap();
    let copy = entry.find("str_copy").expect("the read of `g` is copied");
    let call = entry.find("call f0 f").expect("the call");
    assert!(copy < call, "copied before the call:\n{entry}");
}

#[test]
fn borrowed_operands_need_no_copy_when_nothing_after_them_has_an_effect() {
    let printed = desugared("a = 'x'\nb = a + 'y'\nb.len");
    assert_eq!(printed.matches("str_copy").count(), 0, "{printed}");
}

#[test]
fn every_string_operation_leaves_the_program_verifiable() {
    for src in [
        "c = true\ns = 'a' if c else 'b'\ns.len",
        "s = 'a'\nn = 0\nwhile s != 'aaa'\n    s += 'a'\n    n += 1\nn",
        "x = 3\nf'x={x} and {x}'",
        "a, b = 'x', 'y'\na, b = b, a\na + b",
        "s = str(1.5) + str(true)\ns",
        "x = 1\ng(): str is f'{::x}'\nh(): str is g() + g()\nh()",
    ] {
        desugared(src);
    }
}

#[test]
fn a_loop_body_ending_in_an_if_keeps_the_if() {
    for src in [
        indoc! {"
            g = 0
            n = 0
            while n < 3
                n += 1
                if n == 2
                    ::g = 5
            g
        "},
        indoc! {"
            g = 0
            n = 0
            loop
                n += 1
                if n == 2
                    ::g = 5
            until n > 3
            g
        "},
    ] {
        let printed = desugared(src);
        assert!(
            printed.contains("constant 5: int"),
            "the trailing if is desugared:\n{printed}"
        );
    }
}
