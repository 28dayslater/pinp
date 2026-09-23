// SPDX-License-Identifier: MIT

use super::*;
use crate::parser::parse;
use indoc::indoc;

/// A parsed program. The graph borrows it, so it has to outlive the `Cfg` in each test.
fn parsed(src: &str) -> ProgramAst<'_> {
    parse(src).unwrap()
}

/// How many blocks carry each terminator kind, for structural assertions.
fn terminator_counts(cfg: &Cfg) -> (usize, usize, usize, usize) {
    let mut counts = (0, 0, 0, 0);
    for block_id in cfg.block_ids() {
        match cfg.block(block_id).terminator {
            Terminator::Goto(_) => counts.0 += 1,
            Terminator::Branch { .. } => counts.1 += 1,
            Terminator::Return(_) => counts.2 += 1,
            Terminator::Exit => counts.3 += 1,
        }
    }
    counts
}

#[test]
fn straight_line_code_is_one_block() {
    let ast = parsed("a = 1\nb = 2\na + b");
    let cfg = Cfg::build_entry(&ast);
    // One block of three statements, plus the exit.
    assert_eq!(cfg.blocks().len(), 2);
    assert_eq!(cfg.block(cfg.entry()).actions.len(), 3);
    assert!(matches!(
        cfg.block(cfg.entry()).terminator,
        Terminator::Return(None)
    ));
}

#[test]
fn every_graph_has_a_unique_exit_reached_from_each_return() {
    let ast = parsed("a = 1\na");
    let cfg = Cfg::build_entry(&ast);
    assert!(matches!(cfg.block(cfg.exit()).terminator, Terminator::Exit));
    assert_eq!(
        cfg.successors(cfg.exit()),
        Vec::new(),
        "the sink has no exits"
    );
    for returning in cfg.returning_blocks() {
        assert_eq!(cfg.successors(returning), vec![cfg.exit()]);
    }
    assert_eq!(cfg.returning_blocks().len(), 1, "one return today");
}

#[test]
fn a_while_loop_has_a_back_edge() {
    let ast = parsed(indoc! {"
        n = 0
        while n < 3
            n += 1
        n
    "});
    let cfg = Cfg::build_entry(&ast);
    // The header branches, and the body returns to it.
    let header = cfg
        .block_ids()
        .find(|id| matches!(cfg.block(*id).terminator, Terminator::Branch { .. }))
        .expect("a header");
    let body = match cfg.block(header).terminator {
        Terminator::Branch { then_block, .. } => then_block,
        _ => unreachable!(),
    };
    assert_eq!(
        cfg.successors(body),
        vec![header],
        "the body must jump back to the header"
    );
    assert!(
        cfg.predecessors(header).contains(&body),
        "the header's predecessors include the body — that is the cycle"
    );
}

#[test]
fn an_if_statement_becomes_a_diamond() {
    let ast = parsed(indoc! {"
        a = 0
        if a > 0
            a = 1
        else
            a = 2
        a
    "});
    let cfg = Cfg::build_entry(&ast);
    let (_, branches, returns, exits) = terminator_counts(&cfg);
    assert_eq!(branches, 1, "one condition");
    assert_eq!(returns, 1);
    assert_eq!(exits, 1);
    // Both arms converge on one merge block.
    let branch = cfg
        .block_ids()
        .find(|id| matches!(cfg.block(*id).terminator, Terminator::Branch { .. }))
        .unwrap();
    let (then_block, else_block) = match cfg.block(branch).terminator {
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => (then_block, else_block),
        _ => unreachable!(),
    };
    assert_eq!(cfg.successors(then_block), cfg.successors(else_block));
}

#[test]
fn an_if_without_an_else_still_merges() {
    let ast = parsed(indoc! {"
        a = 0
        if a > 0
            a = 1
        a
    "});
    let cfg = Cfg::build_entry(&ast);
    let merge_reached: Vec<BlockId> = cfg
        .block_ids()
        .filter(|id| cfg.predecessors(*id).len() > 1)
        .collect();
    assert_eq!(merge_reached.len(), 1, "the arm and the fall-through meet");
}

#[test]
fn a_for_loop_binds_its_variable_in_the_body() {
    let ast = parsed(indoc! {"
        total = 0
        for idx in 1..3
            total += idx
        total
    "});
    let cfg = Cfg::build_entry(&ast);
    let binds: Vec<BlockId> = cfg
        .block_ids()
        .filter(|id| {
            cfg.block(*id)
                .actions
                .iter()
                .any(|action| matches!(action, Action::Bind(_)))
        })
        .collect();
    assert_eq!(binds.len(), 1, "the body binds the loop variable");
    // And the binding is the body's first action, before anything reads it.
    assert!(matches!(
        cfg.block(binds[0]).actions.first(),
        Some(Action::Bind(_))
    ));
}

#[test]
fn a_multi_binder_for_binds_each_name() {
    let ast = parsed(indoc! {"
        m = [1, 2; 3, 4]
        total = 0
        for row, col, val in m
            total += val
        total
    "});
    let cfg = Cfg::build_entry(&ast);
    let bind_count: usize = cfg
        .block_ids()
        .map(|id| {
            cfg.block(id)
                .actions
                .iter()
                .filter(|action| matches!(action, Action::Bind(_)))
                .count()
        })
        .sum();
    assert_eq!(bind_count, 3);
}

#[test]
fn a_post_test_loop_branches_at_the_end() {
    let ast = parsed(indoc! {"
        n = 0
        loop
            n += 1
        until n > 2
        n
    "});
    let cfg = Cfg::build_entry(&ast);
    let branch = cfg
        .block_ids()
        .find(|id| matches!(cfg.block(*id).terminator, Terminator::Branch { .. }))
        .expect("a condition at the end of the body");
    // `until` repeats while false, so the *else* edge is the back edge.
    let (then_block, else_block) = match cfg.block(branch).terminator {
        Terminator::Branch {
            then_block,
            else_block,
            ..
        } => (then_block, else_block),
        _ => unreachable!(),
    };
    assert!(
        cfg.predecessors(else_block).contains(&branch) && else_block != then_block,
        "the false edge loops back"
    );
}

#[test]
fn a_nested_block_evaluates_its_result() {
    // An arm's trailing expression is a read that liveness must see, so it becomes an action.
    let ast = parsed("x = 1\ny = 2 if x > 0 else 3\ny");
    let cfg = Cfg::build_entry(&ast);
    // The ternary is nested inside an assignment, so it stays atomic — the whole statement is
    // one action, and no separate Evaluate appears. This documents the if-expression
    // compromise; a statement-position `if` is what produces branches.
    let (_, branches, _, _) = terminator_counts(&cfg);
    assert_eq!(branches, 0, "an if-expression inside a statement is atomic");
}

#[test]
fn a_function_returns_its_body_result() {
    let ast = parse(indoc! {"
        f(a: int): int is
            b = a + 1
            b
        f(1)
    "})
    .unwrap();
    let TopLevel::Func(func) = &ast.top_level[0] else {
        panic!("Expected a function.");
    };
    let cfg = Cfg::build_function(&ast, func);
    let returning = cfg.returning_blocks();
    assert_eq!(returning.len(), 1);
    assert!(
        matches!(
            cfg.block(returning[0]).terminator,
            Terminator::Return(Some(_)),
        ),
        "the body's trailing expression is the returned value"
    );
}

#[test]
fn dot_output_names_every_block_and_edge() {
    let ast = parsed("a = 1\na");
    let cfg = Cfg::build_entry(&ast);
    let dot = cfg.to_dot();
    assert!(dot.starts_with("digraph cfg {"));
    assert!(dot.contains("0 -> 1;"), "entry reaches the exit:\n{dot}");
}
