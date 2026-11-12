// SPDX-License-Identifier: MIT

//! Unreachable code: statements no execution can arrive at.
//!
//! A forward analysis over the simplest lattice there is — reached, or not. Every block starts at
//! bottom (`false`), the entry is seeded as reached, and anything still `false` at fixpoint has no
//! path leading to it. Terms shared across this layer are defined in [`crate::analysis`].
//!
//! Vocabulary particular to reachability:
//!
//! * Reached: some path from the entry arrives at this point. Joining is "reached from anywhere",
//!   so a point stays unreached only when no path whatsoever leads to it.
//! * Unreachable: the diagnostic — a statement no execution can arrive at.
//!
//! Two things make code unreachable. A `return` is one: the code after it in the same body lands
//! in a block no edge enters. The other is a condition that cannot vary. Only a **constant**
//! `true`/`false` counts here, and a counted loop over a range of two integer constants that is
//! provably empty: `while 1 > 2` is not folded, because doing constant arithmetic by hand would
//! duplicate what constant propagation will do properly later.

use crate::analysis::dataflow::{Analysis, Direction, ReachabilityLattice, solve};
use crate::diagnostic::{Diagnostic, DiagnosticCode};
use crate::ir::{BlockId, Constant, Flat, Function, NodeId, Operation, ProgramTables, Terminator};
use crate::lexer::Span;
use crate::parser::{RangeKind, UnOp};

/// Reports the first statement of every block no execution can reach.
pub fn check_reachability(function: &Function<Flat>, tables: &ProgramTables) -> Vec<Diagnostic> {
    let analysis = Reachability { function, tables };
    let solution = solve(function, &analysis);
    function
        .shape
        .blocks
        .iter()
        .filter(|&(block_id, _)| !solution.before(block_id).is_reached())
        .filter_map(|(block_id, _)| first_span(function, block_id))
        .map(|span| {
            Diagnostic::new(
                DiagnosticCode::UnreachableCode,
                span,
                "This code can never run.",
            )
        })
        .collect()
}

/// Where a block's code begins in the source: its first statement, or what its terminator reads.
fn first_span(function: &Function<Flat>, block_id: BlockId) -> Option<Span> {
    let block = &function.shape.blocks[block_id];
    let arena = &function.arena;
    let statements = arena.items(block.statements).iter();
    statements
        .map(|&statement| match arena.operations[statement] {
            // A compiler temporary has no source text of its own; its value does.
            Operation::WriteLocal { local_id, value } if arena.locals[local_id].name.is_none() => {
                arena.spans[value]
            }
            _ => arena.spans[statement],
        })
        .chain(
            block
                .terminator
                .operand()
                .map(|operand| arena.spans[operand]),
        )
        .find(|span| !span.is_unknown())
}

struct Reachability<'function> {
    function: &'function Function<Flat>,
    tables: &'function ProgramTables,
}

impl Analysis for Reachability<'_> {
    type Lattice = ReachabilityLattice;
    const DIRECTION: Direction = Direction::Forward;

    fn boundary(&self) -> ReachabilityLattice {
        ReachabilityLattice::reached()
    }

    fn height(&self, _function: &Function<Flat>) -> usize {
        1
    }

    // Reaching a statement says nothing about the next one: a block runs whole once entered.
    fn transfer_statement(&self, _statement: NodeId, _state: &mut ReachabilityLattice) {}

    fn transfer_terminator(&self, _terminator: &Terminator, _state: &mut ReachabilityLattice) {}

    fn feasible_successors(&self, function: &Function<Flat>, block_id: BlockId) -> Vec<BlockId> {
        let Terminator::Branch {
            cond,
            then_block,
            else_block,
        } = function.shape.blocks[block_id].terminator
        else {
            return function.successors(block_id);
        };
        let empty_loop = function.shape.counted_loop_ranges[block_id]
            .is_some_and(|range| self.is_empty_range(range));
        match self.constant_bool(cond) {
            Some(true) => vec![then_block],
            Some(false) => vec![else_block],
            None if empty_loop => vec![else_block],
            None => vec![then_block, else_block],
        }
    }
}

impl Reachability<'_> {
    fn constant_bool(&self, node_id: NodeId) -> Option<bool> {
        match self.constant(node_id)? {
            Constant::Bool(value) => Some(value),
            _ => None,
        }
    }

    fn constant(&self, node_id: NodeId) -> Option<Constant> {
        match self.function.arena.operations[node_id] {
            Operation::Constant(constant_id) => Some(self.tables.constants.get(constant_id)),
            _ => None,
        }
    }

    fn constant_int(&self, node_id: NodeId) -> Option<i64> {
        match self.constant(node_id)? {
            Constant::Int(value) => Some(value),
            _ => None,
        }
    }

    /// The sign of a step written as an integer constant, negated or not; `None` when not written
    /// so, or zero (which is a runtime error, not an empty range).
    fn step_sign(&self, step: NodeId) -> Option<i64> {
        let (operand, negated) = match self.function.arena.operations[step] {
            Operation::Unary {
                op: UnOp::Neg,
                operand,
            } => (operand, true),
            _ => (step, false),
        };
        let sign = self.constant_int(operand)?.signum();
        match (sign, negated) {
            (0, _) => None,
            (sign, false) => Some(sign),
            (sign, true) => Some(-sign),
        }
    }

    /// Whether `range` is two integer constants that give no value at all. A written step decides
    /// the direction, as when the loop runs; without one an exclusive range runs its spelled way,
    /// and an inclusive range counts toward its stop, so it always holds at least its start.
    fn is_empty_range(&self, range: NodeId) -> bool {
        let Operation::Range {
            kind,
            start,
            stop,
            step,
        } = self.function.arena.operations[range]
        else {
            return false;
        };
        let (Some(start), Some(stop)) = (self.constant_int(start), self.constant_int(stop)) else {
            return false;
        };
        let upward = match (kind, step) {
            (_, Some(step)) => match self.step_sign(step) {
                Some(sign) => sign > 0,
                None => return false,
            },
            (RangeKind::UpExclusive, None) => true,
            (RangeKind::DownExclusive, None) => false,
            (RangeKind::Inclusive, None) => return false,
        };
        match (upward, kind == RangeKind::Inclusive) {
            (true, true) => start > stop,
            (true, false) => start >= stop,
            (false, true) => start < stop,
            (false, false) => start <= stop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desugar::desugar;
    use crate::flatten::flatten_program;
    use crate::parser::parse;
    use crate::sema::analyze;
    use indoc::indoc;

    /// The text of each diagnostic in every function of `src`, in source order.
    fn diagnostics(src: &str) -> Vec<String> {
        let mut ast = parse(src).unwrap();
        analyze(&mut ast).unwrap();
        let program = flatten_program(desugar(&ast));
        let mut found: Vec<Diagnostic> = program
            .functions
            .iter()
            .flat_map(|(_, function)| check_reachability(function, &program.tables))
            .collect();
        found.sort_by_key(|diagnostic| diagnostic.span.start);
        found
            .into_iter()
            .map(|diagnostic| diagnostic.span.text(src).to_string())
            .collect()
    }

    fn loop_over(range: &str) -> String {
        format!("total = 0\nfor idx in {range}\n    total += idx\ntotal\n")
    }

    #[test]
    fn a_while_false_body_never_runs() {
        let src = indoc! {"
            n = 1
            while false
                n = 2
            n
        "};
        assert_eq!(diagnostics(src), vec!["n"], "the body's assignment to `n`");
    }

    #[test]
    fn a_variable_condition_is_never_assumed() {
        let src = indoc! {"
            c = false
            n = 1
            while c
                n = 2
            n
        "};
        assert!(diagnostics(src).is_empty());
    }

    #[test]
    fn an_arithmetic_condition_is_not_folded() {
        let src = indoc! {"
            n = 1
            while 1 > 2
                n = 2
            n
        "};
        assert!(diagnostics(src).is_empty(), "deliberately not folded");
    }

    #[test]
    fn a_false_if_arm_never_runs() {
        let src = indoc! {"
            n = 1
            if false
                n = 2
            n
        "};
        assert_eq!(diagnostics(src), vec!["n"]);
    }

    #[test]
    fn a_true_if_arm_leaves_the_else_unreachable() {
        let src = indoc! {"
            n = 1
            if true
                n = 2
            else
                n = 3
            n
        "};
        assert_eq!(diagnostics(src), vec!["n"], "only the else arm");
    }

    #[test]
    fn a_false_elif_arm_never_runs() {
        let src = indoc! {"
            n = 1
            if n > 0
                n = 2
            elif false
                n = 3
            n
        "};
        assert_eq!(diagnostics(src), vec!["n"]);
    }

    #[test]
    fn an_if_expression_with_a_constant_condition_has_a_dead_arm() {
        // Newly reported: the AST-borrowing graph treated an if-expression as atomic.
        let src = "n = 1\nx = 10 if false else 20\nn + x";
        assert_eq!(diagnostics(src), vec!["10"]);
    }

    #[test]
    fn a_constant_false_left_operand_is_not_followed_through_and() {
        // `and` becomes a branch on a temporary holding the left operand; reachability folds
        // constant conditions only, not values carried through variables.
        assert!(diagnostics("n = 1\nb = false and n > 0\nb").is_empty());
    }

    #[test]
    fn an_empty_literal_range_never_enters_its_body() {
        // Sema rejects a literal range whose spelling contradicts its bounds (`1..>5`), so the
        // empty ones are equal bounds with an exclusive stop — or a written step leading away.
        for range in ["1..<1", "3..>3", "5..>1:2", "1..<5:-2"] {
            assert_eq!(
                diagnostics(&loop_over(range)).len(),
                1,
                "`{range}` is empty"
            );
        }
    }

    #[test]
    fn a_non_empty_literal_range_is_fine() {
        for range in [
            "1..3", "1..<2", "5..>1", "1..1", "1..5:2", "1..1:7", "5..1:-2",
        ] {
            assert!(
                diagnostics(&loop_over(range)).is_empty(),
                "`{range}` runs at least once"
            );
        }
    }

    #[test]
    fn an_inclusive_range_without_a_step_counts_toward_its_stop() {
        // `5..1` runs 5, 4, 3, 2, 1 — today's checker took it for empty.
        assert!(diagnostics(&loop_over("5..1")).is_empty());
    }

    #[test]
    fn a_variable_range_bound_is_never_assumed() {
        let src = indoc! {"
            stop = 0
            total = 0
            for idx in 1..stop
                total += idx
            total
        "};
        assert!(diagnostics(src).is_empty());
        assert!(diagnostics("r = 1..<1\nt = 0\nfor i in r\n    t += i\nt").is_empty());
    }

    #[test]
    fn a_loop_over_an_array_always_enters_its_body() {
        assert!(diagnostics("a = [1, 2]\nt = 0\nfor v in a\n    t += v\nt").is_empty());
    }

    #[test]
    fn code_after_an_infinite_loop_is_unreachable() {
        // `while true` has no exit — pinp has no `break` — so whatever follows it cannot run.
        let src = indoc! {"
            n = 1
            while true
                n = 2
            n
        "};
        assert_eq!(diagnostics(src), vec!["n"], "the trailing `n`");
    }

    #[test]
    fn each_dead_block_of_nested_code_is_reported() {
        // The body, the `if` arm inside it, and the code after that `if`: three blocks, as before.
        let src = indoc! {"
            n = 1
            while false
                n = 2
                if n > 1
                    n = 3
                n = 4
            n
        "};
        assert_eq!(diagnostics(src), vec!["n", "n", "n"]);
    }

    #[test]
    fn ordinary_code_reports_nothing() {
        let src = indoc! {"
            total = 0
            for idx in 1..5
                if idx > 2
                    total += idx
                else
                    total += 1
            total
        "};
        assert!(diagnostics(src).is_empty());
    }

    #[test]
    fn a_function_body_is_checked_too() {
        let src = indoc! {"
            f(n: int): int is
                m = n
                if false
                    m = 0
                m
            f(1)
        "};
        assert_eq!(diagnostics(src), vec!["m"]);
    }

    #[test]
    fn a_dead_string_assignment_is_reported_at_its_target() {
        // The free of the old value comes first in the block, and carries the target's position.
        assert_eq!(diagnostics("s = 'a'\nif false\n    s = 'b'\ns"), vec!["s"]);
    }

    // --- return (0018) ---------------------------------------------------------------------

    #[test]
    fn code_after_a_return_is_reported_once_at_its_first_statement() {
        let src = indoc! {"
            f(n: int): int is
                return n
                m = n + 1
                m = m + 2
                m
            f(1)
        "};
        assert_eq!(diagnostics(src), vec!["m"]);
    }

    #[test]
    fn a_trailing_expression_after_a_return_is_reported() {
        assert_eq!(
            diagnostics(indoc! {"
                f(n: int): int is
                    return n
                    n + 1
                f(1)
            "}),
            vec!["n + 1"]
        );
    }

    #[test]
    fn code_after_an_if_else_that_returns_on_both_arms_is_reported() {
        let src = indoc! {"
            f(n: int): int is
                if n < 0
                    return -n
                else
                    return n
                n * 2
            f(1)
        "};
        assert_eq!(diagnostics(src), vec!["n * 2"]);
    }

    #[test]
    fn code_after_returning_ifs_without_else_is_reachable() {
        let src = indoc! {"
            sign(n: int): int is
                if n < 0
                    return -1
                if n == 0
                    return 0
                1
            sign(1)
        "};
        assert!(diagnostics(src).is_empty());
    }

    #[test]
    fn a_return_ending_a_loop_body_reports_nothing() {
        // The loop step after the return is the compiler's, not the user's: it is not generated.
        for src in [
            indoc! {"
                f(n: int): int is
                    for i in 1..n
                        return i
                    0
                f(1)
            "},
            indoc! {"
                f(n: int): int is
                    for i, v in [1, 2]
                        return v
                    0
                f(1)
            "},
            indoc! {"
                f(n: int): int is
                    while n > 0
                        return n
                    0
                f(1)
            "},
            indoc! {"
                f(n: int): int is
                    for i in 1..n
                        if i > 1
                            return i
                        else
                            return 0
                    0
                f(1)
            "},
        ] {
            assert!(diagnostics(src).is_empty(), "in:\n{src}");
        }
    }

    #[test]
    fn a_function_that_always_returns_reports_nothing_at_its_end() {
        for src in [
            indoc! {"
                abs(n: int): int is
                    if n < 0
                        return -n
                    else
                        return n
                abs(1)
            "},
            indoc! {"
                f(n: int): str is
                    s = str(n)
                    t = s + 'x'
                    if t.len > 3
                        return t
                    else
                        return s
                f(12)
            "},
            indoc! {"
                g = 0
                f(n: int) is
                    ::g = n
                    return
                f(1)
            "},
        ] {
            assert!(diagnostics(src).is_empty(), "in:\n{src}");
        }
    }

    #[test]
    fn a_do_while_whose_body_returns_reports_its_condition() {
        // The condition after the body can never be evaluated.
        let src = indoc! {"
            f(n: int): int is
                loop
                    return n
                until n > 0
                0
            f(1)
        "};
        assert_eq!(diagnostics(src)[0], "n > 0");
    }

    // --- break and continue (0019) ---------------------------------------------------------

    #[test]
    fn code_after_break_or_continue_is_reported_once_at_its_first_statement() {
        for keyword in ["break", "continue"] {
            let src = indoc! {"
                f(n: int): int is
                    t = 0
                    for i in 1..n
                        KEYWORD
                        t += i
                        t += 2
                    t
                f(1)
            "}
            .replace("KEYWORD", keyword);
            assert_eq!(diagnostics(&src), vec!["t"], "in:\n{src}");
        }
    }

    #[test]
    fn code_after_an_if_else_that_leaves_the_loop_on_both_arms_is_reported() {
        let src = indoc! {"
            for i in 1..3
                if i > 1
                    break
                else
                    continue
                i * 2
        "};
        assert_eq!(diagnostics(src), vec!["i * 2"]);
    }

    #[test]
    fn code_after_a_loop_left_only_by_break_is_reachable() {
        // Only the `until` condition, which the break skips, is reported.
        for src in [
            indoc! {"
                g = 0
                f() is
                    loop
                        ::g += 1
                        break
                    until true
                    ::g += 2
                f()
            "},
            indoc! {"
                g = 0
                f() is
                    for i in 1..3 @outer
                        loop
                            break @outer
                        until true
                    ::g += 2
                f()
            "},
        ] {
            assert_eq!(diagnostics(src), vec!["true"], "in:\n{src}");
        }
    }

    #[test]
    fn the_condition_of_a_loop_until_whose_body_always_breaks_is_reported() {
        let src = indoc! {"
            g = 0
            f() is
                loop
                    ::g += 1
                    break
                until ::g > 3
            f()
        "};
        assert_eq!(diagnostics(src), vec!["::g > 3"]);
    }

    #[test]
    fn the_condition_of_a_loop_until_reached_by_continue_is_not_reported() {
        let src = indoc! {"
            g = 0
            f() is
                loop
                    ::g += 1
                    continue
                until ::g > 3
            f()
        "};
        assert!(diagnostics(src).is_empty());
    }

    #[test]
    fn break_or_continue_ending_a_loop_body_reports_nothing() {
        // The counted loop's step after a `break` is not generated; after a `continue` it is
        // reached by the jump.
        for src in [
            indoc! {"
                for i in 1..3
                    break
            "},
            indoc! {"
                for i in 1..3
                    continue
            "},
            indoc! {"
                for i in 1..3
                    if i == 2
                        continue
                    else
                        break
            "},
            indoc! {"
                for v in [1, 2; 3, 4]
                    if v == 2
                        break
                    continue
            "},
            indoc! {"
                for i in 1..3 @outer
                    for j in 1..3
                        continue @outer
            "},
        ] {
            assert!(diagnostics(src).is_empty(), "in:\n{src}");
        }
    }
}
