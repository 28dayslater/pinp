// SPDX-License-Identifier: MIT

//! Unreachable code: statements no execution can arrive at.
//!
//! A forward analysis over the simplest fact there is — reached, or not. Every block starts knowing
//! nothing (`false`), the entry is seeded as reached, and anything still `false` at fixpoint has no
//! path leading to it.
//!
//! What makes anything unreachable in a language with no `return` or `break` is a condition that
//! cannot vary. Only a **literal** `true`/`false` counts here, and an empty literal range: `while
//! 1 > 2` is not folded, because doing constant arithmetic by hand would duplicate what constant
//! propagation will do properly later. That is a deliberate first-iteration limit, not an oversight.

use crate::analysis::cfg::{Action, BlockId, Cfg, Terminator};
use crate::analysis::dataflow::{Analysis, Direction, ReachabilityFact, solve};
use crate::analysis::diagnostic::{Diagnostic, DiagnosticCode};
use crate::lexer::Span;
use crate::parser::{ExprId, Node, ProgramAst, RangeKind, Stmt};

/// Reports every statement that cannot run.
pub fn check(ast: &ProgramAst, cfg: &Cfg) -> Vec<Diagnostic> {
    let analysis = Reachability { ast };
    let solution = solve(cfg, &analysis);

    let mut diagnostics = Vec::new();
    for block_id in cfg.block_ids() {
        if solution.before(block_id).is_reached() {
            continue;
        }
        // One finding per unreachable block, at its first action: reporting every statement in a
        // skipped body would bury the one fact the reader needs.
        if let Some(span) = first_span(ast, cfg, block_id) {
            diagnostics.push(Diagnostic::new(
                DiagnosticCode::UnreachableCode,
                span,
                "This code can never run.",
            ));
        }
    }
    diagnostics
}

/// Where to point a finding about `block_id` — the span of its first action that has one.
fn first_span(ast: &ProgramAst, cfg: &Cfg, block_id: BlockId) -> Option<Span> {
    cfg.block(block_id).actions.iter().find_map(|action| {
        let span = match action {
            Action::Statement(stmt) => stmt_span(ast, stmt),
            Action::Evaluate(expr_id) => ast.span_of(*expr_id),
            Action::Bind(_) => Span::UNKNOWN,
        };
        (!span.is_unknown()).then_some(span)
    })
}

/// A statement's position, taken from a representative expression. Statements are not arena
/// entries, so they have no span of their own; this is the compromise recorded in the spec.
fn stmt_span(ast: &ProgramAst, stmt: &Stmt) -> Span {
    match stmt {
        Stmt::Assign {
            target_spans,
            values,
            ..
        } => target_spans
            .first()
            .and_then(|group| group.first().copied())
            .filter(|span| !span.is_unknown())
            .or_else(|| values.first().map(|value| ast.span_of(*value)))
            .unwrap_or(Span::UNKNOWN),
        Stmt::Expr(expr_id) => ast.span_of(*expr_id),
        Stmt::While { cond, .. } => ast.span_of(*cond),
        Stmt::Loop { cond, .. } => ast.span_of(*cond),
        Stmt::For { source, .. } | Stmt::ForArray { source, .. } => ast.span_of(*source),
        // An indexed target is a `Place`, which carries no span; the index expression is the
        // nearest thing with a position.
        Stmt::IndexedAssign { index, .. } => ast.span_of(*index),
        Stmt::IndexedAssign2D { row, .. } => ast.span_of(*row),
    }
}

struct Reachability<'a, 'ast> {
    ast: &'a ProgramAst<'ast>,
}

impl<'ast> Analysis<'ast> for Reachability<'_, 'ast> {
    type Fact = ReachabilityFact;
    const DIRECTION: Direction = Direction::Forward;

    fn boundary(&self) -> ReachabilityFact {
        ReachabilityFact::reached()
    }

    fn transfer_action(&self, _action: &Action<'ast>, _fact: &mut ReachabilityFact) {
        // Nothing a block does can make the rest of it unreachable — pinp has no diverging
        // statement. Only a terminator decides where control goes next.
    }

    fn transfer_terminator(&self, _terminator: &Terminator, _fact: &mut ReachabilityFact) {}

    /// The whole checker, in one override: an edge out of a decided condition is never taken, so
    /// nothing beyond it is reached.
    fn live_successors(&self, cfg: &Cfg<'ast>, block_id: BlockId) -> Vec<BlockId> {
        match cfg.block(block_id).terminator {
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => match self.constant_condition(cond) {
                Some(true) => vec![then_block],
                Some(false) => vec![else_block],
                None => vec![then_block, else_block],
            },
            _ => cfg.successors(block_id),
        }
    }
}

impl<'ast> Reachability<'_, 'ast> {
    /// Whether `cond` is decided before the program runs: `Some(true)` when only the then-edge is
    /// ever taken, `Some(false)` when only the else-edge is, `None` when both remain possible.
    ///
    /// A literal `true`/`false` decides both ways. A `for` header is different: its "condition" is
    /// the range, and an empty one decides *against* the body — but a non-empty range decides
    /// nothing, because a loop that runs still finishes and leaves by the exit edge. Claiming
    /// otherwise marks everything after any literal `for` loop unreachable.
    fn constant_condition(&self, cond: ExprId) -> Option<bool> {
        match self.ast.node(cond) {
            Node::Bool(value) => Some(*value),
            Node::Range { .. } => match literal_range_is_empty(self.ast, cond) {
                Some(true) => Some(false),
                Some(false) | None => None,
            },
            _ => None,
        }
    }
}

/// Whether a range with literal bounds is empty — `1..<1` and `5..1` are, `1..3` is not. `None`
/// when a bound is not a literal, so nothing is assumed.
fn literal_range_is_empty(ast: &ProgramAst, range_id: ExprId) -> Option<bool> {
    let Node::Range {
        start, stop, kind, ..
    } = ast.node(range_id)
    else {
        return None;
    };
    let start = literal_int(ast, *start)?;
    let stop = literal_int(ast, *stop)?;
    Some(match kind {
        RangeKind::Inclusive => start > stop,
        RangeKind::UpExclusive => start >= stop,
        RangeKind::DownExclusive => start <= stop,
    })
}

fn literal_int(ast: &ProgramAst, expr_id: ExprId) -> Option<i64> {
    match ast.node(expr_id) {
        Node::Int(value) => Some(*value),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use indoc::indoc;

    /// Every unreachable-code finding in `src`, as the source text it points at.
    fn findings(src: &str) -> Vec<String> {
        let ast = parse(src).unwrap();
        let cfg = Cfg::build_entry(&ast);
        check(&ast, &cfg)
            .into_iter()
            .map(|diagnostic| diagnostic.span.text(src).to_string())
            .collect()
    }

    #[test]
    fn a_while_false_body_never_runs() {
        let src = indoc! {"
            n = 1
            while false
                n = 2
            n
        "};
        assert_eq!(findings(src), vec!["n"], "the body's assignment to `n`");
    }

    #[test]
    fn a_variable_condition_is_never_assumed() {
        // The whole point of the literal-only limit: nothing is folded, so nothing is claimed.
        let src = indoc! {"
            c = false
            n = 1
            while c
                n = 2
            n
        "};
        assert!(findings(src).is_empty());
    }

    #[test]
    fn an_arithmetic_condition_is_not_folded() {
        // `1 > 2` is constantly false, but recognising that is constant propagation's job.
        let src = indoc! {"
            n = 1
            while 1 > 2
                n = 2
            n
        "};
        assert!(findings(src).is_empty(), "deliberately not folded");
    }

    #[test]
    fn a_false_if_arm_never_runs() {
        let src = indoc! {"
            n = 1
            if false
                n = 2
            n
        "};
        assert_eq!(findings(src), vec!["n"]);
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
        assert_eq!(findings(src), vec!["n"], "only the else arm");
    }

    #[test]
    fn an_empty_literal_range_never_enters_its_body() {
        for source in ["1..<1", "5..1", "1..>5"] {
            let src = format!("total = 0\nfor idx in {source}\n    total += idx\ntotal\n");
            assert_eq!(
                findings(&src).len(),
                1,
                "`{source}` is empty, so the body is dead"
            );
        }
    }

    #[test]
    fn a_non_empty_literal_range_is_fine() {
        for source in ["1..3", "1..<2", "5..>1", "1..1"] {
            let src = format!("total = 0\nfor idx in {source}\n    total += idx\ntotal\n");
            assert!(findings(&src).is_empty(), "`{source}` runs at least once");
        }
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
        assert!(findings(src).is_empty());
    }

    #[test]
    fn code_after_an_infinite_loop_is_unreachable() {
        // `while true` has no exit — pinp has no `break` — so whatever follows it genuinely cannot
        // run. Looping for ever is the author's business; the dead tail is worth saying.
        let src = indoc! {"
            n = 1
            while true
                n = 2
            n
        "};
        assert_eq!(findings(src), vec!["n"], "the trailing `n`");
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
        assert!(findings(src).is_empty());
    }

    #[test]
    fn a_function_body_is_checked_too() {
        let ast = parse(indoc! {"
            f(n: int): int is
                m = n
                if false
                    m = 0
                m
            f(1)
        "})
        .unwrap();
        let crate::parser::TopLevel::Func(func) = &ast.top_level[0] else {
            panic!("Expected a function.");
        };
        let cfg = Cfg::build_function(&ast, func);
        assert_eq!(check(&ast, &cfg).len(), 1);
    }
}
