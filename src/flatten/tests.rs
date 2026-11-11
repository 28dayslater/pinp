// SPDX-License-Identifier: MIT

use indoc::indoc;

use super::*;
use crate::desugar::desugar;
use crate::ir::{FunctionId, Printer, verify};
use crate::parser::parse;
use crate::sema::analyze;

/// Parses, analyses, desugars, and flattens `src`; checks every flat function verifies; returns
/// them all printed in id order (the entry last), separated by blank lines.
fn flattened(src: &str) -> String {
    let mut ast = parse(src).expect("The test program parses.");
    analyze(&mut ast).expect("The test program type-checks.");
    let mut program = desugar(&ast);
    let mut flat_functions = Vec::new();
    for (function_id, function) in program.functions.iter() {
        let flat = flatten(function.clone(), &mut program.tables);
        assert_eq!(verify(&flat), Ok(()), "function {function_id:?}");
        flat_functions.push(flat);
    }
    let printer = Printer::new(&program.tables, &ast.names, &ast.string_literals);
    let printed: Vec<String> = flat_functions
        .iter()
        .map(|flat| printer.function(flat))
        .collect();
    printed.join("\n")
}

#[test]
fn straight_line_code_is_one_block_ending_in_the_result() {
    assert_eq!(
        flattened("x = 1\nx + 2"),
        indoc! {"
            function f0 (0 parameters), flat
            block0 (entry)
              write_global g0 x
                constant 1: int
              return
                add: int
                  read_global g0 x: int
                  constant 2: int
        "}
    );
}

#[test]
fn a_program_without_a_result_returns_nothing() {
    assert_eq!(
        flattened("x = 1\nwhile false\n    x = 2\n")
            .lines()
            .rev()
            .take(2)
            .collect::<Vec<_>>(),
        ["  return", "block3"]
    );
}

#[test]
fn an_if_else_branches_to_two_blocks_that_meet_again() {
    assert_eq!(
        flattened(indoc! {"
            c = true
            x = 0
            if c
                x = 1
            else
                x = 2
            x
        "}),
        indoc! {"
            function f0 (0 parameters), flat
            block0 (entry)
              write_global g0 c
                constant true: bool
              write_global g1 x
                constant 0: int
              branch block1 block2
                read_global g0 c: bool
            block1
              write_global g1 x
                constant 1: int
              goto block3
            block2
              write_global g1 x
                constant 2: int
              goto block3
            block3
              return
                read_global g1 x: int
        "}
    );
}

#[test]
fn an_if_without_else_branches_straight_to_where_it_ends() {
    assert_eq!(
        flattened(indoc! {"
            c = true
            x = 0
            if c
                x = 1
            x
        "}),
        indoc! {"
            function f0 (0 parameters), flat
            block0 (entry)
              write_global g0 c
                constant true: bool
              write_global g1 x
                constant 0: int
              branch block1 block2
                read_global g0 c: bool
            block1
              write_global g1 x
                constant 1: int
              goto block2
            block2
              return
                read_global g1 x: int
        "}
    );
}

#[test]
fn an_or_with_an_empty_then_body_still_gets_its_own_block() {
    // `a or b` desugars to an `If` whose `then` is empty; the block is empty too.
    let printed = flattened("a = true\nb = false\na or b");
    assert!(
        printed.contains(indoc! {"
            branch block1 block2
                read_local l0: bool
            block1
              goto block3
            block2
              write_local l0
                read_global g1 b: bool
              goto block3
            block3
              return
                read_local l0: bool
        "}),
        "{printed}"
    );
}

#[test]
fn a_while_loop_tests_in_a_header_block_and_jumps_back_to_it() {
    assert_eq!(
        flattened(indoc! {"
            i = 0
            while i < 3
                i += 1
            i
        "}),
        indoc! {"
            function f0 (0 parameters), flat
            block0 (entry)
              write_global g0 i
                constant 0: int
              goto block1
            block1
              branch block2 block3
                less: bool
                  read_global g0 i: int
                  constant 3: int
            block2
              write_global g0 i
                add: int
                  read_global g0 i: int
                  constant 1: int
              goto block1
            block3
              return
                read_global g0 i: int
        "}
    );
}

/// A loop creates its header, body, and exit blocks before flattening what is inside them, so
/// an `if` in the header gets the next numbers.
#[test]
fn a_while_header_runs_before_each_test() {
    let printed = flattened(indoc! {"
        f(n: int): int is
            i = 0
            while i < n and i < 10
                i += 1
            i
        f(3)
    "});
    assert!(
        printed.contains(indoc! {"
            block1
              write_local l2
                less: bool
                  read_local l1 i: int
                  read_local l0 n: int
              branch block4 block5
                read_local l2: bool
            block2
              write_local l1 i
                add: int
                  read_local l1 i: int
                  constant 1: int
              goto block1
            block3
              return
                read_local l1 i: int
            block4
              write_local l2
                less: bool
                  read_local l1 i: int
                  constant 10: int
              goto block5
            block5
              branch block2 block3
                read_local l2: bool
        "}),
        "{printed}"
    );
}

#[test]
fn a_loop_runs_its_body_first_and_until_leaves_when_true() {
    assert_eq!(
        flattened(indoc! {"
            f(): int is
                i = 0
                loop
                    i += 1
                until i >= 3
                i
            f()
        "}),
        indoc! {"
            function f0 f (0 parameters), flat
            locals
              l0 i: int
            block0 (entry)
              write_local l0 i
                constant 0: int
              goto block1
            block1
              write_local l0 i
                add: int
                  read_local l0 i: int
                  constant 1: int
              branch block2 block1
                greater_equal: bool
                  read_local l0 i: int
                  constant 3: int
            block2
              return
                read_local l0 i: int

            function f1 (0 parameters), flat
            block0 (entry)
              return
                call f0 f: int
        "}
    );
}

#[test]
fn a_loop_while_repeats_when_true() {
    let printed = flattened(indoc! {"
        f(): int is
            i = 0
            loop
                i += 1
            while i < 3
            i
        f()
    "});
    assert!(printed.contains("  branch block1 block2\n"), "{printed}");
}

#[test]
fn a_counted_loop_steps_its_index_through_its_range() {
    // An inclusive range with no written step runs up or down depending on its bounds, decided
    // when the range is built — so the test looks at the step's sign.
    assert_eq!(
        flattened(indoc! {"
            t = 0
            for i in 1..3
                t += i
            t
        "}),
        indoc! {"
            function f0 (0 parameters), flat
            locals
              l0 i: int
              l1: range
            block0 (entry)
              write_global g0 t
                constant 0: int
              write_local l1
                range inclusive: range
                  constant 1: int
                  constant 3: int
              write_local l0 i
                range_field start: int
                  read_local l1: range
              goto block1
            block1
              branch block2 block3
                select: bool
                  greater: bool
                    range_field step: int
                      read_local l1: range
                    constant 0: int
                  less_equal: bool
                    read_local l0 i: int
                    range_field stop: int
                      read_local l1: range
                  greater_equal: bool
                    read_local l0 i: int
                    range_field stop: int
                      read_local l1: range
            block2
              write_global g0 t
                add: int
                  read_global g0 t: int
                  read_local l0 i: int
              write_local l0 i
                add: int
                  read_local l0 i: int
                  range_field step: int
                    read_local l1: range
              goto block1
            block3
              return
                read_global g0 t: int
        "}
    );
}

#[test]
fn an_exclusive_range_written_in_place_needs_no_direction_test() {
    let printed = flattened(indoc! {"
        t = 0
        for i in 5..>0
            t += i
        t
    "});
    assert!(
        printed.contains(indoc! {"
            block1
              branch block2 block3
                greater: bool
                  read_local l0 i: int
                  range_field stop: int
                    read_local l1: range
        "}),
        "{printed}"
    );
}

#[test]
fn a_range_from_a_variable_decides_direction_and_inclusion_at_run_time() {
    let printed = flattened(indoc! {"
        r = 1..<4
        t = 0
        for i in r
            t += i
        t
    "});
    assert!(
        printed.contains(indoc! {"
            block1
              branch block2 block3
                select: bool
                  greater: bool
                    range_field step: int
                      read_local l1: range
                    constant 0: int
                  select: bool
                    range_field inclusive: bool
                      read_local l1: range
                    less_equal: bool
                      read_local l0 i: int
                      range_field stop: int
                        read_local l1: range
                    less: bool
                      read_local l0 i: int
                      range_field stop: int
                        read_local l1: range
                  select: bool
                    range_field inclusive: bool
                      read_local l1: range
                    greater_equal: bool
                      read_local l0 i: int
                      range_field stop: int
                        read_local l1: range
                    greater: bool
                      read_local l0 i: int
                      range_field stop: int
                        read_local l1: range
        "}),
        "{printed}"
    );
    assert!(
        printed.contains("  write_local l1\n    read_global g0 r: range\n"),
        "{printed}"
    );
}

#[test]
fn a_void_function_returns_nothing() {
    let printed = flattened(indoc! {"
        count = 0
        bump() is
            if true
                ::count += 1
        bump()
    "});
    assert!(
        printed.starts_with(indoc! {"
            function f0 bump (0 parameters), flat
            block0 (entry)
              branch block1 block2
                constant true: bool
            block1
              write_global g0 count
                add: int
                  read_global g0 count: int
                  constant 1: int
              goto block2
            block2
              return
        "}),
        "{printed}"
    );
}

#[test]
fn nested_control_flow_numbers_blocks_as_it_meets_them() {
    assert_eq!(
        flattened(indoc! {"
            f(n: int): int is
                s = 0
                i = 0
                while i < n
                    if i > 1
                        s += i
                    i += 1
                s
            f(4)
        "})
        .split("\n\n")
        .next()
        .unwrap_or_default(),
        indoc! {"
            function f0 f (1 parameter), flat
            locals
              l0 n: int
              l1 s: int
              l2 i: int
            block0 (entry)
              write_local l1 s
                constant 0: int
              write_local l2 i
                constant 0: int
              goto block1
            block1
              branch block2 block3
                less: bool
                  read_local l2 i: int
                  read_local l0 n: int
            block2
              branch block4 block5
                greater: bool
                  read_local l2 i: int
                  constant 1: int
            block3
              return
                read_local l1 s: int
            block4
              write_local l1 s
                add: int
                  read_local l1 s: int
                  read_local l2 i: int
              goto block5
            block5
              write_local l2 i
                add: int
                  read_local l2 i: int
                  constant 1: int
              goto block1
        "}
        .trim_end()
    );
}

#[test]
fn flattening_keeps_every_node_id_and_side_table_entry() {
    let mut ast = parse("x = 1.5\nx + 2").expect("parses");
    analyze(&mut ast).expect("type-checks");
    let mut program = desugar(&ast);
    let structured = program.functions[program.entry].clone();
    let flat = flatten(structured.clone(), &mut program.tables);
    for (node_id, operation) in structured.arena.operations.iter() {
        assert_eq!(flat.arena.operations[node_id], *operation);
        assert_eq!(flat.arena.types[node_id], structured.arena.types[node_id]);
        assert_eq!(flat.arena.spans[node_id], structured.arena.spans[node_id]);
    }
}

/// The entry function of `src`, flattened.
fn flat_entry(src: &str) -> Function<Flat> {
    let mut ast = parse(src).expect("The test program parses.");
    analyze(&mut ast).expect("The test program type-checks.");
    let program = flatten_program(desugar(&ast));
    program.functions[program.entry].clone()
}

/// Each recorded counted-loop header with the operation of the range it tests, in block order.
fn loop_ranges(flat: &Function<Flat>) -> Vec<(u32, Operation)> {
    flat.shape
        .blocks
        .iter()
        .filter_map(|(block_id, _)| {
            let range = flat.shape.counted_loop_ranges[block_id]?;
            Some((block_id.index(), flat.arena.operations[range]))
        })
        .collect()
}

#[test]
fn a_counted_loop_records_the_range_its_header_tests() {
    let flat = flat_entry("t = 0\nfor i in 1..3\n    t += i\nt");
    let recorded = loop_ranges(&flat);
    assert_eq!(recorded.len(), 1);
    let (header, range) = recorded[0];
    assert_eq!(
        header, 1,
        "the header is the first block made after the entry"
    );
    assert!(matches!(range, Operation::Range { .. }), "{range:?}");
}

#[test]
fn a_range_from_a_variable_is_recorded_as_its_read() {
    let flat = flat_entry("r = 1..3\nt = 0\nfor i in r\n    t += i\nt");
    let recorded = loop_ranges(&flat);
    assert_eq!(recorded.len(), 1);
    assert!(
        matches!(recorded[0].1, Operation::ReadGlobal(_)),
        "{:?}",
        recorded[0].1
    );
}

#[test]
fn other_loops_and_branches_record_no_range() {
    let flat = flat_entry(indoc! {"
        n = 0
        while n < 3
            n += 1
        loop
            n -= 1
        until n == 0
        if n == 0
            n = 1
        n
    "});
    assert_eq!(loop_ranges(&flat), []);
}

#[test]
fn each_of_nested_counted_loops_records_its_own_header() {
    let flat = flat_entry(indoc! {"
        m = [1, 2; 3, 4]
        t = 0
        for v in m
            t += v
        t
    "});
    let headers: Vec<u32> = loop_ranges(&flat)
        .iter()
        .map(|&(header, _)| header)
        .collect();
    assert_eq!(headers.len(), 2, "rows outside, columns inside");
    assert!(headers[0] < headers[1], "the row loop is met first");
}

#[test]
fn flattening_a_program_keeps_every_function_in_order_and_its_entry() {
    let mut ast = parse("f(n: int): int is n\ng(n: int): int is n\nf(1) + g(2)").expect("parses");
    analyze(&mut ast).expect("type-checks");
    let program = desugar(&ast);
    let entry = program.entry;
    let ids: Vec<FunctionId> = program.functions.iter().map(|(id, _)| id).collect();
    let flat = flatten_program(program);
    assert_eq!(flat.entry, entry);
    let flat_ids: Vec<FunctionId> = flat
        .functions
        .iter()
        .map(|(_, function)| function.id)
        .collect();
    assert_eq!(flat_ids, ids);
    for (_, function) in flat.functions.iter() {
        assert_eq!(verify(function), Ok(()));
    }
}

#[test]
fn a_written_step_decides_an_exclusive_ranges_direction() {
    // `5..>1:2` steps up from 5, already past its stop: no iterations, as the step's sign says —
    // not a descent, which the `..>` alone would suggest.
    let printed = flattened("t = 0\nfor i in 5..>1:2\n    t += i\nt");
    assert!(
        printed.contains(indoc! {"
            block1
              branch block2 block3
                less: bool
        "}),
        "{printed}"
    );
}

#[test]
fn a_written_negative_step_counts_an_upward_exclusive_range_down() {
    let printed = flattened("t = 0\nfor i in 1..<5:-2\n    t += i\nt");
    assert!(
        printed.contains(indoc! {"
            block1
              branch block2 block3
                greater: bool
        "}),
        "{printed}"
    );
}

#[test]
fn a_step_from_a_variable_decides_direction_at_run_time() {
    let printed = flattened("s = 2\nt = 0\nfor i in 5..>1:s\n    t += i\nt");
    assert!(printed.contains("select: bool"), "{printed}");
}

// --- return (0018) -----------------------------------------------------------------------

/// The printed flat function `index` of `src`.
fn flat_function(src: &str, index: usize) -> String {
    flattened(src)
        .split("function f")
        .nth(index + 1)
        .map(|text| format!("function f{}\n", text.trim_end()))
        .expect("the function exists")
}

/// The lines of block `label` (`"block3"`), without the label itself.
fn block_lines<'text>(text: &'text str, label: &str) -> Vec<&'text str> {
    text.lines()
        .skip_while(|line| *line != label && !line.starts_with(&format!("{label} ")))
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .collect()
}

/// How many lines of `text` are exactly `line`.
fn count_lines(text: &str, line: &str) -> usize {
    text.lines()
        .filter(|candidate| candidate.trim() == line)
        .count()
}

/// The terminator line of every block, in order.
fn terminators(text: &str) -> Vec<String> {
    // A block's own lines are indented by two; only its terminator starts with one of these words.
    text.lines()
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
        .map(str::trim)
        .filter(|line| {
            ["return", "goto", "branch", "unreachable", "runtime_error"]
                .iter()
                .any(|word| line.starts_with(word))
        })
        .map(str::to_string)
        .collect()
}

#[test]
fn a_return_ends_its_block_and_code_after_it_lands_in_a_block_nothing_enters() {
    assert_eq!(
        flat_function(
            indoc! {"
                f(n: int): int is
                    return n
                    n + 1
                f(1)
            "},
            0
        ),
        indoc! {"
            function f0 f (1 parameter), flat
            locals
              l0 n: int
            block0 (entry)
              return
                read_local l0 n: int
            block1
              evaluate
                add: int
                  read_local l0 n: int
                  constant 1: int
              unreachable
        "}
    );
}

#[test]
fn a_bare_return_ends_its_block_without_an_operand() {
    let text = flat_function(
        indoc! {"
            g = 0
            f(n: int) is
                if n > 0
                    return
                ::g = n
            f(1)
        "},
        0,
    );
    assert_eq!(block_lines(&text, "block1"), ["  return"], "{text}");
}

#[test]
fn a_function_whose_every_path_returns_ends_in_unreachable() {
    let text = flat_function(
        indoc! {"
            abs(n: int): int is
                if n < 0
                    return -n
                else
                    return n
            abs(1)
        "},
        0,
    );
    let ends = terminators(&text);
    assert_eq!(
        ends.first().map(String::as_str),
        Some("branch block1 block2"),
        "{text}"
    );
    // Blocks 1 and 2 return; the join, block 3, is where nothing arrives.
    assert_eq!(block_lines(&text, "block3"), ["  unreachable"], "{text}");
    assert_eq!(
        ends.iter().filter(|end| *end == "return").count(),
        2,
        "{text}"
    );
}

#[test]
fn a_function_with_an_early_return_still_returns_its_result() {
    let text = flat_function(
        indoc! {"
            sign(n: int): int is
                if n < 0
                    return -1
                1
            sign(1)
        "},
        0,
    );
    assert!(
        block_lines(&text, "block2") == ["  return", "    constant 1: int"],
        "{text}"
    );
    assert!(!text.contains("unreachable"), "{text}");
}

#[test]
fn a_counted_loop_whose_body_ends_in_return_does_not_step() {
    let text = flat_function(
        indoc! {"
            f(n: int): int is
                for i in 1..n
                    return i
                0
            f(3)
        "},
        0,
    );
    // Only the initial write of `i`; the step after the return could never run.
    assert_eq!(count_lines(&text, "write_local l1 i"), 1, "{text}");
}

#[test]
fn a_counted_loop_whose_body_returns_on_one_path_still_steps() {
    let text = flat_function(
        indoc! {"
            f(n: int): int is
                for i in 1..n
                    if i > 2
                        return i
                0
            f(3)
        "},
        0,
    );
    assert_eq!(count_lines(&text, "write_local l1 i"), 2, "{text}");
}

#[test]
fn a_counted_loop_after_an_early_return_steps_as_usual() {
    let text = flat_function(
        indoc! {"
            f(n: int): int is
                t = 0
                if n < 0
                    return 0
                for i in 1..n
                    t += i
                t
            f(3)
        "},
        0,
    );
    assert_eq!(count_lines(&text, "write_local l2 i"), 2, "{text}");
    assert!(!text.contains("unreachable"), "{text}");
}

#[test]
fn a_counted_loop_whose_body_returns_on_both_branches_does_not_step() {
    let text = flat_function(
        indoc! {"
            f(n: int): int is
                for i in 1..n
                    if i > 2
                        return i
                    else
                        return 0
                0
            f(3)
        "},
        0,
    );
    assert_eq!(count_lines(&text, "write_local l1 i"), 1, "{text}");
}

#[test]
fn a_do_while_whose_body_returns_leaves_the_function_end_unreached_but_returning() {
    // The result is still returned at the end: sema does not treat loops as diverging, and the
    // signature wants a value.
    let text = flat_function(
        indoc! {"
            f(n: int): int is
                loop
                    return n
                until n > 0
                0
            f(1)
        "},
        0,
    );
    assert!(
        block_lines(&text, "block2") == ["  return", "    constant 0: int"],
        "{text}"
    );
}

#[test]
fn a_void_function_that_always_returns_ends_in_unreachable() {
    let text = flat_function(
        indoc! {"
            g = 0
            f(n: int) is
                ::g = n
                return
            f(1)
        "},
        0,
    );
    assert_eq!(block_lines(&text, "block1"), ["  unreachable"], "{text}");
}

#[test]
fn a_return_inside_a_while_leaves_the_loop_exit_reachable() {
    let text = flat_function(
        indoc! {"
            f(n: int): int is
                while n > 0
                    return n
                0
            f(1)
        "},
        0,
    );
    assert!(
        block_lines(&text, "block3") == ["  return", "    constant 0: int"],
        "{text}"
    );
    assert_eq!(
        terminators(&text)
            .iter()
            .filter(|end| *end == "return")
            .count(),
        2,
        "{text}"
    );
}
