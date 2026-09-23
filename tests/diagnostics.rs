// SPDX-License-Identifier: MIT

//! End-to-end diagnostics: a bad program in, a *placed* message out.
//!
//! Every layer reports through one [`pinp::diagnostic::Diagnostic`] and one renderer, so these
//! assert the shared contract rather than each layer's private error type: the span points at the
//! offending source text, and the rendered line reads the way a compiler's does.
//!
//! Each case asserts `(line, column, the-text-the-span-covers)`. Naming the covered text rather
//! than a byte offset is what makes a wrong span visible — an off-by-one that still lands on the
//! right line shows up as the wrong character.
//!
//! No `llvm` feature: reporting an error never reaches the backend.

use indoc::indoc;
use pinp::diagnostic::{Diagnose, Severity};
use pinp::lexer::{LineIndex, Span, lex};
use pinp::parser::{ParseErrorKind, parse};
use pinp::sema::analyze;

/// Where a lexer error landed, as `(line, column, covered text)`.
fn lex_failure(src: &str) -> (u32, u32, String) {
    let error = lex(src).expect_err("source should not lex");
    place(src, error.span)
}

/// Where a parser error landed, as `(line, column, covered text)`.
fn parse_failure(src: &str) -> (u32, u32, String) {
    place(src, parse_error(src).span)
}

/// The parser's error for `src`. Not `expect_err`: `ProgramAst` is not `Debug`, and a bespoke
/// panic message says more than a derived one would anyway.
fn parse_error(src: &str) -> pinp::parser::ParseError {
    match parse(src) {
        Err(error) => error,
        Ok(_) => panic!("Source should not parse."),
    }
}

/// Where a semantic error landed, as `(line, column, covered text)`.
fn sema_failure(src: &str) -> (u32, u32, String) {
    let mut ast = parse(src).expect("source should parse");
    let error = analyze(&mut ast).expect_err("source should not analyze");
    place(src, error.span)
}

fn place(src: &str, span: Span) -> (u32, u32, String) {
    assert!(!span.is_unknown(), "diagnostic carries no position");
    let (line, column) = LineIndex::new(src).locate(span.start);
    (line, column, span.text(src).to_string())
}

// --- lexer -----------------------------------------------------------------------------

#[test]
fn an_unexpected_character_points_at_that_character() {
    let src = indoc! {"
        x = 1
        y = x $ 2
    "};
    assert_eq!(lex_failure(src), (2, 7, "$".into()));
}

#[test]
fn a_tab_in_indentation_points_at_the_tab() {
    // Not `indoc!`: the tab is the subject of the test, and dedenting would obscure it.
    assert_eq!(lex_failure("f() is\n\tx = 1\n"), (2, 1, "\t".into()));
}

#[test]
fn an_unterminated_string_points_at_the_opening_quote() {
    let (line, column, _) = lex_failure("'abc");
    assert_eq!((line, column), (1, 1));
}

#[test]
fn inconsistent_indentation_keeps_its_position() {
    // Pre-existing placement, preserved by this iteration rather than corrected: the report lands
    // on the newline ending the *previous* line, not on the offending indentation. See KI-0001.
    let src = indoc! {"
        if true
            x = 1
          y = 2
    "};
    let (line, column, _) = lex_failure(src);
    assert_eq!((line, column), (2, 10));
}

// --- parser ----------------------------------------------------------------------------

#[test]
fn an_unexpected_token_points_at_that_token() {
    let src = indoc! {"
        x = 1
        y = * 2
    "};
    assert_eq!(parse_failure(src), (2, 5, "*".into()));
}

#[test]
fn a_lex_failure_keeps_its_position_through_the_parser() {
    // The defect this iteration exists to close: `parse` used to keep the lexer's message and
    // throw its position away. Same input, same place, whichever layer the caller stopped at.
    let src = indoc! {"
        x = 1
        y = x $ 2
    "};
    let error = parse_error(src);
    assert!(matches!(error.kind, ParseErrorKind::Lex(_)));
    assert_eq!(parse_failure(src), lex_failure(src));
}

#[test]
fn a_misaligned_continuation_points_at_the_misaligned_token() {
    // `2` does not align to `1`'s column, so `2` is what the reader must move.
    let src = indoc! {"
        a, b = 1,
          2
    "};
    assert_eq!(parse_failure(src), (2, 3, "2".into()));
}

#[test]
fn a_comma_starting_a_line_points_at_the_comma() {
    let src = indoc! {"
        a, b
        , c = 1, 2, 3
    "};
    assert_eq!(parse_failure(src), (2, 1, ",".into()));
}

#[test]
fn runaway_nesting_points_into_the_nesting() {
    // The exact column is the depth cap's, an implementation detail; the line and the character
    // are the contract.
    let (line, _, text) = parse_failure(&"(".repeat(300));
    assert_eq!((line, text.as_str()), (1, "("));
}

#[test]
fn an_overlong_if_ladder_points_at_an_arm() {
    let mut src = String::from(indoc! {"
        if a > 0
            1
    "});
    for _ in 0..300 {
        src.push_str(indoc! {"
            elif a > 0
                1
        "});
    }
    src.push_str(indoc! {"
        else
            1
    "});
    let (_, column, text) = parse_failure(&src);
    assert_eq!((column, text.as_str()), (1, "elif"));
}

// --- sema ------------------------------------------------------------------------------

#[test]
fn an_unknown_symbol_points_at_the_name() {
    let src = indoc! {"
        x = 1
        y = zzz + 1
    "};
    assert_eq!(sema_failure(src), (2, 5, "zzz".into()));
}

#[test]
fn a_type_error_covers_the_whole_offending_expression() {
    // Not just the operator: a composite node spans `left.start .. right.end`, so the reader sees
    // both operands that failed to combine.
    let src = indoc! {"
        x = 1
        y = 'a' - 2
    "};
    assert_eq!(sema_failure(src), (2, 5, "'a' - 2".into()));
}

// --- sema: statement-level rules ----------------------------------------------------------
//
// A rule that fires from a *statement* rather than from inside an expression walk has to name
// what it blames: by the time it runs, its operands have been typed and `analyze_expr` has put
// the position back on the enclosing expression — which, for a statement, is nothing. These
// pin that each such rule points at the part of the statement the reader has to change.

#[test]
fn a_bad_element_assignment_points_at_the_value() {
    let src = indoc! {"
        xs = [1, 2, 3]
        xs[0] = 1.5
    "};
    assert_eq!(sema_failure(src), (2, 9, "1.5".into()));
}

#[test]
fn a_bad_slice_assignment_points_at_the_value() {
    let src = indoc! {"
        xs = [1, 2, 3]
        xs[0..1] = 1.5
    "};
    assert_eq!(sema_failure(src), (2, 12, "1.5".into()));
}

#[test]
fn a_bad_matrix_element_assignment_points_at_the_value() {
    let src = indoc! {"
        m = [1, 2; 3, 4]
        m[0, 0] = 1.5
    "};
    assert_eq!(sema_failure(src), (2, 11, "1.5".into()));
}

#[test]
fn a_non_int_index_points_at_the_index() {
    let src = indoc! {"
        xs = [1, 2, 3]
        xs[1.5] = 2
    "};
    assert_eq!(sema_failure(src), (2, 4, "1.5".into()));
}

#[test]
fn an_out_of_bounds_index_points_at_the_index() {
    let src = indoc! {"
        xs = [1, 2, 3]
        xs[9] = 2
    "};
    assert_eq!(sema_failure(src), (2, 4, "9".into()));
}

#[test]
fn indexing_a_non_array_points_at_the_target() {
    // The index and the value are both fine; the target is what the reader has to fix.
    let src = indoc! {"
        x = 1
        x[0] = 2
    "};
    assert_eq!(sema_failure(src), (2, 1, "x".into()));
}

#[test]
fn an_unknown_array_target_points_at_the_name() {
    assert_eq!(sema_failure("zz[0] = 2\n"), (1, 1, "zz".into()));
}

#[test]
fn a_non_bool_while_condition_points_at_the_condition() {
    let src = indoc! {"
        x = 1
        while 1
            x = 2
    "};
    assert_eq!(sema_failure(src), (2, 7, "1".into()));
}

#[test]
fn a_body_that_yields_the_wrong_type_points_at_the_result() {
    let src = indoc! {"
        f(): int is
            1.5
    "};
    assert_eq!(sema_failure(src), (2, 5, "1.5".into()));
}

#[test]
fn iterating_a_non_iterable_points_at_the_source() {
    let src = indoc! {"
        for i in 1
            x = 1
    "};
    assert_eq!(sema_failure(src), (1, 10, "1".into()));
}

#[test]
fn an_unknown_global_target_points_at_the_target() {
    // Not at the value: `1` is a perfectly good Int, and `::g` is the name that is not bound.
    assert_eq!(sema_failure("::g = 1\n"), (1, 1, "::g".into()));
}

#[test]
fn assigning_to_a_loop_variable_points_at_the_target() {
    let src = indoc! {"
        for i in 1..3
            i = 5
    "};
    assert_eq!(sema_failure(src), (2, 5, "i".into()));
}

#[test]
fn a_rebinding_type_mismatch_points_at_the_value() {
    // Here the target is fine and the value is the wrong type, so the blame flips.
    let src = indoc! {"
        x = 1
        x = 1.5
    "};
    assert_eq!(sema_failure(src), (2, 5, "1.5".into()));
}

#[test]
fn a_bad_declaration_points_at_the_function_name() {
    // Header rules run before any expression exists; the name is the anchor the reader needs.
    assert_eq!(
        sema_failure("noop(a: void): int is 1\n"),
        (1, 1, "noop".into())
    );
}

// --- multi-byte source ------------------------------------------------------------------
//
// A column is a *byte* offset into its line, which is what `LineIndex::locate` has always
// computed for the analysis layer. These pin that the error layers agree with it rather than
// counting characters, and that slicing a span out of a line with non-ASCII in it stays on a
// character boundary.

#[test]
fn a_column_past_non_ascii_counts_bytes_like_line_index() {
    // `é` is two bytes, so the `$` sits at character 13 but byte column 14. Byte columns are the
    // convention; the point is that every layer answers the same number.
    let src = "y = 'héllo' $ 2\n";
    let (line, column, text) = lex_failure(src);
    assert_eq!((line, text.as_str()), (1, "$"));
    assert_eq!(column, 14);
    assert_eq!(
        column,
        LineIndex::new(src).locate(src.find('$').unwrap() as u32).1
    );
}

#[test]
fn a_span_over_non_ascii_slices_on_a_character_boundary() {
    // A composite span covers `left.start .. right.end`; if either end landed mid-character,
    // `text` would panic rather than return the expression.
    let src = indoc! {"
        x = 1
        y = 'héllo' - 2
    "};
    assert_eq!(sema_failure(src), (2, 5, "'héllo' - 2".into()));
}

// --- rendering -------------------------------------------------------------------------

#[test]
fn an_error_renders_with_its_position_and_no_code() {
    // Errors from the three older layers carry no `DiagnosticCode`, so nothing is bracketed.
    let src = indoc! {"
        x = 1
        y = x $ 2
    "};
    let error = lex(src).expect_err("source should not lex");
    assert_eq!(
        error.render(&LineIndex::new(src)),
        "2:7: error: Unexpected character `$`."
    );
}

#[test]
fn a_parse_error_renders_with_its_position() {
    let src = indoc! {"
        x = 1
        y = * 2
    "};
    let rendered = parse_error(src).render(&LineIndex::new(src));
    assert!(
        rendered.starts_with("2:5: error: "),
        "rendered as {rendered:?}"
    );
}

#[test]
fn a_sema_error_renders_with_its_position() {
    let src = indoc! {"
        x = 1
        y = zzz + 1
    "};
    let mut ast = parse(src).expect("source should parse");
    let rendered = analyze(&mut ast)
        .expect_err("source should not analyze")
        .render(&LineIndex::new(src));
    assert_eq!(rendered, "2:5: error: Unknown symbol `zzz`.");
}

#[test]
fn errors_are_error_severity_and_carry_no_code() {
    let diagnostic = lex("x = $\n")
        .expect_err("source should not lex")
        .to_diagnostic();
    assert_eq!(diagnostic.severity, Severity::Error);
    assert_eq!(diagnostic.code, None);
}

#[test]
fn to_diagnostic_preserves_the_message_verbatim() {
    let error = lex("x = $\n").expect_err("source should not lex");
    assert_eq!(error.to_diagnostic().message, error.message);
}
