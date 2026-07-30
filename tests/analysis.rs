// SPDX-License-Identifier: MIT

//! End-to-end static analysis: source in, findings out.
//!
//! These go through the public [`pinp::analysis::check`] rather than a single checker, so they
//! cover what a user of the layer actually sees — every checker over every function, findings in
//! source order, each resolved to a line and column.
//!
//! No `llvm` feature: the analysis layer reads a typed AST and knows nothing about the backend.

use pinp::analysis::{Diagnostic, check};
use pinp::lexer::LineIndex;
use pinp::parser::parse;
use pinp::sema::analyze;

/// Analyses `src`, returning each finding as `(code, line, column, text-it-points-at)`.
fn findings(src: &str) -> Vec<(&'static str, u32, u32, String)> {
    let mut ast = parse(src).expect("program should parse");
    analyze(&mut ast).expect("program should type-check");
    let line_index = LineIndex::new(src);
    check(&ast)
        .into_iter()
        .map(|diagnostic: Diagnostic| {
            let (line, column) = line_index.locate(diagnostic.span.start);
            (
                diagnostic.code.code(),
                line,
                column,
                diagnostic.span.text(src).to_string(),
            )
        })
        .collect()
}

#[test]
fn a_clean_program_reports_nothing() {
    let src = "\
total = 0
for idx in 1..5
    total += idx
total
";
    assert_eq!(findings(src), Vec::new());
}

#[test]
fn an_unused_binding_is_located() {
    let src = "\
kept = 1
spare = 2
kept
";
    assert_eq!(
        findings(src),
        vec![("PINP0103", 2, 1, "spare".to_string())],
        "line 2, column 1 — where `spare` is written"
    );
}

#[test]
fn a_dead_store_is_located() {
    let src = "\
a = 1
a = 2
a
";
    assert_eq!(findings(src), vec![("PINP0102", 1, 1, "a".to_string())]);
}

#[test]
fn unreachable_code_is_located() {
    let src = "\
n = 1
while false
    n = 2
n
";
    assert_eq!(findings(src), vec![("PINP0101", 3, 5, "n".to_string())]);
}

#[test]
fn every_finding_is_reported_in_one_run_in_source_order() {
    // The batch-reporting property: sema stops at its first error, and this layer deliberately does
    // not. The order is the file's, not the checkers' or the graphs'.
    let src = "\
first = 1
first = 2
never_read = 3
if false
    first = 4
first
";
    let found = findings(src);
    let lines: Vec<u32> = found.iter().map(|(_, line, _, _)| *line).collect();
    assert!(
        lines.windows(2).all(|pair| pair[0] <= pair[1]),
        "findings must be ordered by position, got {found:?}"
    );
    let codes: Vec<&str> = found.iter().map(|(code, _, _, _)| *code).collect();
    assert!(codes.contains(&"PINP0102"), "the overwritten `first`");
    assert!(codes.contains(&"PINP0103"), "`never_read`");
    assert!(codes.contains(&"PINP0101"), "the dead `if` arm");
}

#[test]
fn findings_inside_a_function_are_reported_too() {
    let src = "\
compute(n: int): int is
    spare = n * 2
    n + 1
compute(3)
";
    assert_eq!(
        findings(src),
        vec![("PINP0103", 2, 5, "spare".to_string())],
        "the function's own graph is checked"
    );
}

#[test]
fn findings_from_several_functions_are_merged_in_source_order() {
    let src = "\
one(): int is
    a = 1
    2
two(): int is
    b = 1
    3
one() + two()
";
    let found = findings(src);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].3, "a");
    assert_eq!(found[1].3, "b");
    assert!(
        found[0].1 < found[1].1,
        "`a` is on an earlier line than `b`"
    );
}

#[test]
fn a_rendered_finding_reads_as_a_diagnostic() {
    let src = "spare = 1\n0\n";
    let mut ast = parse(src).unwrap();
    analyze(&mut ast).unwrap();
    let line_index = LineIndex::new(src);
    let rendered: Vec<String> = check(&ast)
        .iter()
        .map(|diagnostic| diagnostic.render(&line_index))
        .collect();
    assert_eq!(
        rendered,
        vec!["1:1: warning[PINP0103]: Binding `spare` is never read.".to_string()]
    );
}

#[test]
fn analysis_never_reports_on_the_language_test_programs() {
    // A spot-check against false positives on ordinary code: a handful of programs drawn from the
    // shapes the rest of the suite exercises should all come back clean.
    for src in [
        "1 + 2 * 3",
        "a, b = 1, 2\na + b",
        "m = [1, 2; 3, 4]\nm[1, 1]",
        "s = 'hello'\ns.len",
        "f(n: int): int is n * n\nf(4)",
        "total = 0\nn = 0\nwhile n < 3\n    total += n\n    n += 1\ntotal",
        "g = 1\nbump(): int is ::g + 1\nbump()",
        "arr = [idx * 2 for idx in 1..4]\narr[0]",
    ] {
        assert_eq!(findings(src), Vec::new(), "false positive on:\n{src}");
    }
}
