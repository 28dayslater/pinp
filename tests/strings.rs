// SPDX-License-Identifier: MIT
#![cfg(feature = "llvm")]

//! End-to-end strings: literals, `.len`, concatenation, f-string interpolation, the six
//! comparisons, the `str(x)` conversion, and the `meminfo()` diagnostic — all round-tripped through
//! the JIT so the 16-byte `PinpStr` crosses the host boundary on every check. Requires the `llvm`
//! feature.
//!
//! Deterministic freeing is a later step, so nothing here asserts on allocation counts; these tests
//! pin down *values*. Multiline literals are a later step too, so every literal below is
//! single-line.

mod common;
use common::{eval, eval_bool, eval_int, eval_str};
use indoc::indoc;
use pinp::codegen::PinpValue;

/// The largest content that still fits the small-string optimisation: 15 bytes go inline, 16 spill
/// to the heap. Both sides of that boundary must round-trip identically.
const INLINE_MAX: usize = 15;

// --- literals and the host round-trip -------------------------------------------------------

#[test]
fn single_and_double_quotes_are_the_same_string() {
    assert_eq!(eval_str("'hello'"), "hello");
    assert_eq!(eval_str("\"hello\""), "hello");
}

#[test]
fn empty_literal_round_trips() {
    assert_eq!(eval_str("''"), "");
    assert_eq!(eval_str("\"\""), "");
}

#[test]
fn literal_keeps_spaces_and_punctuation() {
    assert_eq!(
        eval_str("'  a, b; c!  ?  '"),
        "  a, b; c!  ?  ",
        "leading, interior, and trailing whitespace all survive"
    );
    assert_eq!(eval_str("'0123456789'"), "0123456789");
}

#[test]
fn the_other_quote_is_ordinary_content() {
    // Only a literal's own delimiter is special, so the opposite quote needs no escape.
    assert_eq!(eval_str("'say \"hi\"'"), "say \"hi\"");
    assert_eq!(eval_str("\"it's\""), "it's");
}

#[test]
fn inline_and_heap_literals_round_trip_identically() {
    // The SSO boundary is the runtime's one representation switch; a caller must not be able to
    // tell which side of it a string landed on.
    let inline = "a".repeat(INLINE_MAX);
    let heap = "a".repeat(INLINE_MAX + 1);
    assert_eq!(eval_str(&format!("'{inline}'")), inline);
    assert_eq!(eval_str(&format!("'{heap}'")), heap);
}

#[test]
fn long_heap_literal_round_trips() {
    let long: String = (0..500)
        .map(|index| char::from(b'a' + (index % 26) as u8))
        .collect();
    assert_eq!(eval_str(&format!("'{long}'")), long);
}

#[test]
fn non_ascii_bytes_are_carried_opaquely() {
    // Encoding is ASCII-only by design; a non-ASCII byte is data, not an error. It must survive
    // the round-trip untouched, and count as its bytes rather than its characters.
    assert_eq!(eval_str("'café'"), "café");
    assert_eq!(eval_int("'café'.len"), 5, "é is two bytes");
}

// --- .len ------------------------------------------------------------------------------------

#[test]
fn len_of_literals() {
    assert_eq!(eval_int("'hello'.len"), 5);
    assert_eq!(eval_int("''.len"), 0);
}

#[test]
fn len_across_the_inline_boundary() {
    let inline = "x".repeat(INLINE_MAX);
    let heap = "x".repeat(INLINE_MAX + 1);
    assert_eq!(eval_int(&format!("'{inline}'.len")), INLINE_MAX as i64);
    assert_eq!(eval_int(&format!("'{heap}'.len")), INLINE_MAX as i64 + 1);
}

#[test]
fn len_of_variable_and_concat() {
    assert_eq!(
        eval_int(indoc! {"
            s = 'abc'
            s.len
        "}),
        3
    );
    assert_eq!(eval_int("('ab' + 'cde').len"), 5);
}

#[test]
fn len_participates_in_arithmetic() {
    // `.len` is an ordinary `Int`, so it composes with the rest of the language.
    assert_eq!(eval_int("'abc'.len * 2 + 1"), 7);
    assert!(eval_bool("'abc'.len > 'ab'.len"));
}

// --- concatenation ---------------------------------------------------------------------------

#[test]
fn concat_pair() {
    assert_eq!(eval_str("'foo' + 'bar'"), "foobar");
}

#[test]
fn concat_with_empty_operands() {
    assert_eq!(eval_str("'' + 'a'"), "a");
    assert_eq!(eval_str("'a' + ''"), "a");
    assert_eq!(eval_str("'' + ''"), "");
}

#[test]
fn concat_chain_of_three_and_more() {
    // The chain flattens to one `concat_n`; the observable contract is that it still concatenates
    // left to right.
    assert_eq!(eval_str("'a' + 'b' + 'c'"), "abc");
    assert_eq!(eval_str("'a' + 'b' + 'c' + 'd' + 'e' + 'f'"), "abcdef");
}

#[test]
fn concat_crosses_the_inline_boundary() {
    // Two inline operands whose result no longer fits inline: the result must be heap-allocated
    // and complete.
    let eight = "a".repeat(8);
    assert_eq!(
        eval_str(&format!("'{eight}' + '{eight}'")),
        "a".repeat(16),
        "8 + 8 = 16 bytes, one past the inline maximum"
    );
    // And exactly at the boundary it stays inline.
    assert_eq!(eval_str("'aaaaaaa' + 'bbbbbbbb'"), "aaaaaaabbbbbbbb");
}

#[test]
fn concat_of_heap_operands() {
    let left = "L".repeat(40);
    let right = "R".repeat(40);
    assert_eq!(
        eval_str(&format!("'{left}' + '{right}'")),
        format!("{left}{right}")
    );
}

#[test]
fn concat_auto_wraps_a_scalar_right_operand() {
    // A `str` on the left implicitly wraps the right operand in `str(...)`.
    assert_eq!(eval_str("'n=' + 5"), "n=5");
    assert_eq!(eval_str("'n=' + -5"), "n=-5");
    assert_eq!(eval_str("'x=' + 1.5"), "x=1.5");
    assert_eq!(eval_str("'b=' + true"), "b=true");
    assert_eq!(eval_str("'b=' + false"), "b=false");
}

#[test]
fn concat_auto_wrap_uses_the_ryu_float_spelling() {
    // A float always reads as a float: ryu keeps the trailing `.0` where `to_string` would drop it.
    assert_eq!(eval_str("'x=' + 2.0"), "x=2.0");
}

#[test]
fn concat_auto_wraps_computed_scalars() {
    assert_eq!(
        eval_str(indoc! {"
            n = 20
            'total: ' + (n * 2 + 2)
        "}),
        "total: 42"
    );
    assert_eq!(eval_str("'cmp: ' + (3 > 2)"), "cmp: true");
}

#[test]
fn concat_chain_mixes_strings_and_scalars() {
    assert_eq!(eval_str("'a' + 1 + 'b' + 2.5 + 'c' + true"), "a1b2.5ctrue");
}

#[test]
fn concat_of_variables_and_calls() {
    assert_eq!(
        eval_str(indoc! {"
            greet(): str is 'hello'
            who = 'world'
            greet() + ', ' + who + '!'
        "}),
        "hello, world!"
    );
}

#[test]
fn concat_result_is_itself_concatenable() {
    assert_eq!(
        eval_str(indoc! {"
            a = 'x' + 'y'
            b = a + a
            b + a
        "}),
        "xyxyxy"
    );
}

// --- f-strings -------------------------------------------------------------------------------

#[test]
fn fstring_without_holes_is_a_plain_string() {
    assert_eq!(eval_str("f'hello'"), "hello");
    assert_eq!(eval_str("f\"hello\""), "hello");
    assert_eq!(eval_str("f''"), "");
}

#[test]
fn fstring_single_hole() {
    assert_eq!(
        eval_str(indoc! {"
            name = 'world'
            f'hello {name}'
        "}),
        "hello world"
    );
}

#[test]
fn fstring_hole_positions() {
    // Leading, trailing, surrounded, and adjacent holes: the segment split must not drop or
    // duplicate a literal run.
    assert_eq!(
        eval_str(indoc! {"
            a = 'A'
            b = 'B'
            f'{a}-tail'
        "}),
        "A-tail"
    );
    assert_eq!(
        eval_str(indoc! {"
            a = 'A'
            f'head-{a}'
        "}),
        "head-A"
    );
    assert_eq!(
        eval_str(indoc! {"
            a = 'A'
            b = 'B'
            f'{a}{b}'
        "}),
        "AB"
    );
    assert_eq!(
        eval_str(indoc! {"
            a = 'A'
            b = 'B'
            f'<{a}|{b}>'
        "}),
        "<A|B>"
    );
}

#[test]
fn fstring_repeats_one_binding() {
    assert_eq!(
        eval_str(indoc! {"
            x = 7
            f'{x}{x}{x}'
        "}),
        "777"
    );
}

#[test]
fn fstring_stringifies_every_scalar_type() {
    assert_eq!(
        eval_str(indoc! {"
            b = true
            i = -7
            d = 1.5
            s = 'x'
            f'{b} {i} {d} {s}'
        "}),
        "true -7 1.5 x"
    );
}

#[test]
fn fstring_float_hole_uses_the_ryu_spelling() {
    assert_eq!(
        eval_str(indoc! {"
            d = 2.0
            f'{d}'
        "}),
        "2.0"
    );
}

#[test]
fn fstring_whitespace_inside_a_hole_is_ignored() {
    assert_eq!(
        eval_str(indoc! {"
            x = 5
            f'{ x }|{x}'
        "}),
        "5|5"
    );
}

#[test]
fn fstring_interpolates_a_global() {
    assert_eq!(
        eval_str(indoc! {"
            g = 42
            f(): str is f'g={::g}'
            f()
        "}),
        "g=42"
    );
}

#[test]
fn fstring_reads_the_current_value_of_a_rebound_binding() {
    assert_eq!(
        eval_str(indoc! {"
            x = 1
            before = f'{x}'
            x = 2
            before + f'{x}'
        "}),
        "12"
    );
}

#[test]
fn fstring_holes_produce_heap_results() {
    // Interpolation that overflows the inline buffer must still be complete.
    assert_eq!(
        eval_str(indoc! {"
            s = '0123456789'
            f'{s}-{s}'
        "}),
        "0123456789-0123456789"
    );
}

#[test]
fn fstring_is_concatenable_and_measurable() {
    assert_eq!(
        eval_str(indoc! {"
            n = 3
            f'n={n}' + '!'
        "}),
        "n=3!"
    );
    assert_eq!(
        eval_int(indoc! {"
            n = 3
            f'n={n}'.len
        "}),
        3
    );
}

// --- comparisons -----------------------------------------------------------------------------

#[test]
fn equality_and_inequality() {
    assert!(eval_bool("'abc' == 'abc'"));
    assert!(!eval_bool("'abc' == 'abd'"));
    assert!(eval_bool("'abc' != 'abd'"));
    assert!(!eval_bool("'abc' != 'abc'"));
    assert!(eval_bool("'' == ''"));
    assert!(!eval_bool("'' == 'a'"));
}

#[test]
fn equality_ignores_the_storage_representation() {
    // The same content compares equal whether it arrived inline, from the heap, or via a concat.
    let heap = "z".repeat(40);
    assert!(
        eval_bool(&format!("'{heap}' == '{heap}'")),
        "two heap strings"
    );
    assert!(eval_bool("'ab' + 'c' == 'abc'"));
    assert!(
        !eval_bool(&format!("'{heap}' == 'z'")),
        "a heap string never equals a shorter inline one"
    );
}

#[test]
fn ordering_is_lexicographic() {
    assert!(eval_bool("'a' < 'b'"));
    assert!(!eval_bool("'b' < 'a'"));
    assert!(eval_bool("'b' > 'a'"));
    assert!(eval_bool("'a' <= 'a'"));
    assert!(eval_bool("'a' >= 'a'"));
    assert!(!eval_bool("'a' < 'a'"));
    assert!(!eval_bool("'a' > 'a'"));
}

#[test]
fn a_prefix_orders_before_its_extension() {
    // The three-way compare must consult length once the common prefix runs out, rather than
    // stopping at the shorter operand.
    assert!(eval_bool("'ab' < 'abc'"));
    assert!(eval_bool("'abc' > 'ab'"));
    assert!(eval_bool("'' < 'a'"));
    assert!(!eval_bool("'' >= 'a'"));
}

#[test]
fn ordering_is_by_unsigned_byte_value() {
    // A high byte must not compare as negative — `memcmp` semantics are unsigned.
    assert!(eval_bool("'é' > 'z'"));
    assert!(eval_bool("'Z' < 'a'"), "uppercase sorts before lower");
}

#[test]
fn comparisons_drive_conditions() {
    assert_eq!(
        eval_int(indoc! {"
            s = 'beta'
            1 if s > 'alpha' else 0
        "}),
        1
    );
    assert_eq!(
        eval_int(indoc! {"
            total = 0
            s = 'a'
            while s != 'aaaa'
                s = s + 'a'
                total += 1
            total
        "}),
        3
    );
}

#[test]
fn comparison_of_computed_strings() {
    assert!(eval_bool(indoc! {"
            n = 5
            f'n={n}' == 'n=' + 5
        "}));
}

// --- str(x) ------------------------------------------------------------------------------------

#[test]
fn str_of_ints() {
    assert_eq!(eval_str("str(0)"), "0");
    assert_eq!(eval_str("str(42)"), "42");
    assert_eq!(eval_str("str(-42)"), "-42");
}

#[test]
fn str_of_the_extreme_ints() {
    // The widest `i64` decimal is 20 bytes, so the minimum also exercises the heap path.
    assert_eq!(eval_str("str(9223372036854775807)"), "9223372036854775807");
    assert_eq!(
        eval_str("str(-9223372036854775807 - 1)"),
        "-9223372036854775808"
    );
}

#[test]
fn str_of_floats() {
    assert_eq!(eval_str("str(1.5)"), "1.5");
    assert_eq!(eval_str("str(-0.25)"), "-0.25");
    assert_eq!(eval_str("str(2.0)"), "2.0", "a whole float keeps its `.0`");
}

#[test]
fn str_of_non_finite_floats() {
    // ryu's spellings, which happen to match std's.
    assert_eq!(eval_str("str(1.0/0.0)"), "inf");
    assert_eq!(eval_str("str(-1.0/0.0)"), "-inf");
    assert_eq!(eval_str("str(0.0/0.0)"), "NaN");
}

#[test]
fn str_of_a_huge_float_is_scientific() {
    // Scientific notation for extreme magnitudes is what keeps ryu's buffer stack-bounded.
    assert_eq!(eval_str("str(1.0e300)"), "1e300");
}

#[test]
fn str_of_bools() {
    assert_eq!(eval_str("str(true)"), "true");
    assert_eq!(eval_str("str(false)"), "false");
}

#[test]
fn str_of_a_string_is_the_same_string() {
    assert_eq!(eval_str("str('already')"), "already");
    assert_eq!(
        eval_str(&format!("str('{}')", "q".repeat(40))),
        "q".repeat(40)
    );
}

#[test]
fn str_of_an_expression() {
    assert_eq!(
        eval_str(indoc! {"
            n = 4
            str(n * n)
        "}),
        "16"
    );
    assert_eq!(eval_int("str(12345).len"), 5);
}

// --- bindings, scopes, and functions ----------------------------------------------------------

#[test]
fn local_binding_round_trips() {
    assert_eq!(
        eval_str(indoc! {"
            s = 'stored'
            s
        "}),
        "stored"
    );
}

#[test]
fn rebinding_replaces_the_value() {
    assert_eq!(
        eval_str(indoc! {"
            s = 'first'
            s = 'second'
            s
        "}),
        "second"
    );
    // Including across the inline/heap boundary in both directions.
    assert_eq!(
        eval_str(indoc! {"
            s = 'short'
            s = 'a string well past fifteen bytes'
            s = 'short again'
            s
        "}),
        "short again"
    );
}

#[test]
fn compound_concat_assignment() {
    assert_eq!(
        eval_str(indoc! {"
            s = 'a'
            s += 'b'
            s += 'c'
            s
        "}),
        "abc"
    );
}

#[test]
fn global_string_is_visible_inside_a_function() {
    assert_eq!(
        eval_str(indoc! {"
            g = 'global'
            read(): str is ::g
            read()
        "}),
        "global"
    );
}

#[test]
fn function_returns_a_string() {
    assert_eq!(
        eval_str(indoc! {"
            greet(): str is 'hi'
            greet()
        "}),
        "hi"
    );
}

#[test]
fn function_returns_a_computed_heap_string() {
    // The returned `PinpStr` is moved out of the callee, so a heap result must still be intact
    // (and complete) at the call site.
    assert_eq!(
        eval_str(indoc! {"
            build(n: int): str is 'value is ' + n + ' exactly'
            build(1234)
        "}),
        "value is 1234 exactly"
    );
}

#[test]
fn function_with_a_string_local() {
    assert_eq!(
        eval_str(indoc! {"
            build(n: int): str is
                head = 'n='
                body = str(n)
                head + body
            build(9)
        "}),
        "n=9"
    );
}

#[test]
fn function_returns_the_length_of_a_local_string() {
    assert_eq!(
        eval_int(indoc! {"
            width(n: int): int is
                s = f'{n}'
                s.len
            width(12345)
        "}),
        5
    );
}

#[test]
fn strings_flow_through_conditionals() {
    assert_eq!(eval_str("'yes' if 1 < 2 else 'no'"), "yes");
    assert_eq!(eval_str("'yes' if 1 > 2 else 'no'"), "no");
    assert_eq!(
        eval_str(indoc! {"
            pick(n: int): str is
                s = 'small'
                if n > 100
                    s = 'a decidedly large number'
                s
            pick(1000)
        "}),
        "a decidedly large number"
    );
}

#[test]
fn a_loop_builds_a_string() {
    assert_eq!(
        eval_str(indoc! {"
            s = ''
            for idx in 1..5
                s += str(idx)
            s
        "}),
        "12345"
    );
    assert_eq!(
        eval_str(indoc! {"
            s = 'x'
            n = 0
            while n < 3
                s = s + s
                n += 1
            s
        "}),
        "xxxxxxxx"
    );
}

#[test]
fn a_string_survives_an_unrelated_allocation() {
    // Arrays allocate through the same shim; a string binding must be unaffected by traffic
    // around it.
    assert_eq!(
        eval_str(indoc! {"
            s = 'kept safe across allocations'
            a = [1, 2, 3]
            b = [idx * 2 for idx in 1..50]
            s
        "}),
        "kept safe across allocations"
    );
}

// --- meminfo() -------------------------------------------------------------------------------

#[test]
fn meminfo_is_a_void_statement() {
    // It prints to stderr and evaluates to nothing, so it is only ever a statement's value.
    assert_eq!(eval("meminfo()"), PinpValue::Void);
}

#[test]
fn meminfo_does_not_disturb_the_program() {
    assert_eq!(
        eval_str(indoc! {"
            s = 'still here'
            meminfo()
            s
        "}),
        "still here"
    );
}
