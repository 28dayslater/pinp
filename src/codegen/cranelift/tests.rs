// SPDX-License-Identifier: MIT

use indoc::indoc;

use super::*;
use crate::codegen::{PinpJit, PinpValue};

fn run(src: &str) -> Result<PinpValue, String> {
    PinpJit::eval(src)
}

fn value(src: &str) -> PinpValue {
    run(src).expect("The program runs.")
}

fn error(src: &str) -> String {
    run(src).expect_err("The program raises a runtime error.")
}

fn int(src: &str) -> i64 {
    match value(src) {
        PinpValue::Int(value) => value,
        other => panic!("Expected Int, got {other:?}."),
    }
}

fn float(src: &str) -> f64 {
    match value(src) {
        PinpValue::Float(value) => value,
        other => panic!("Expected Float, got {other:?}."),
    }
}

fn boolean(src: &str) -> bool {
    match value(src) {
        PinpValue::Bool(value) => value,
        other => panic!("Expected Bool, got {other:?}."),
    }
}

fn ints(values: &[i64]) -> PinpValue {
    PinpValue::Array(values.iter().map(|&value| PinpValue::Int(value)).collect())
}

// --- Words: how one IR value is held -------------------------------------------------------------

#[test]
fn each_type_is_held_in_its_words() {
    use crate::parser::{ArrayElementType, PinpType};
    use cranelift_codegen::ir::types::{F64, I8, I64};
    assert_eq!(words(PinpType::Int), [I64]);
    assert_eq!(words(PinpType::Float), [F64]);
    assert_eq!(words(PinpType::Bool), [I8]);
    assert_eq!(words(PinpType::Str), [I64, I64]);
    assert_eq!(words(PinpType::Range), [I64, I64, I64, I8]);
    assert_eq!(words(PinpType::Array(ArrayElementType::Bool, 3)), [I64]);
    assert_eq!(
        words(PinpType::Matrix(ArrayElementType::Float, 2, 2)),
        [I64]
    );
    assert_eq!(words(PinpType::Void), []);
}

// --- Program results -----------------------------------------------------------------------------

#[test]
fn every_result_type_reaches_the_host() {
    assert_eq!(value("true"), PinpValue::Bool(true));
    assert_eq!(value("40 + 2"), PinpValue::Int(42));
    assert_eq!(value("1.5"), PinpValue::Float(1.5));
    assert_eq!(value("'short'"), PinpValue::Str("short".into()));
    assert_eq!(value("x = 1"), PinpValue::Int(1));
    assert_eq!(value("x = 1\nwhile false\n    x = 2\n"), PinpValue::Void);
    assert_eq!(value("[1, 2, 3]"), ints(&[1, 2, 3]));
    assert_eq!(
        value("[1.5, 2.5; 3.5, 4.5]"),
        PinpValue::Matrix {
            rows: 2,
            cols: 2,
            elements: [1.5, 2.5, 3.5, 4.5].map(PinpValue::Float).to_vec(),
        }
    );
}

#[test]
fn a_heap_string_result_keeps_both_descriptor_words() {
    let long = "a string well past the sixteen-byte inline limit";
    assert_eq!(value(&format!("'{long}'")), PinpValue::Str(long.into()));
}

#[test]
fn a_bool_matrix_is_one_byte_per_element() {
    assert_eq!(
        value("[true, false; false, true]"),
        PinpValue::Matrix {
            rows: 2,
            cols: 2,
            elements: [true, false, false, true].map(PinpValue::Bool).to_vec(),
        }
    );
}

// --- Integer division: the decided runtime errors ------------------------------------------------

#[test]
fn dividing_by_zero_is_a_runtime_error() {
    assert_eq!(error("z = 0\n7 div z"), "Division by zero.");
    assert_eq!(error("z = 0\n7 mod z"), "Division by zero.");
}

#[test]
fn the_overflowing_quotient_is_a_runtime_error() {
    let min = "m = 0 - 9223372036854775807 - 1\nd = 0 - 1\n";
    assert_eq!(error(&format!("{min}m div d")), "Division overflow.");
    assert_eq!(error(&format!("{min}m mod d")), "Division overflow.");
}

#[test]
fn divisions_next_to_the_trapping_ones_are_exact() {
    let min = "m = 0 - 9223372036854775807 - 1\n";
    assert_eq!(int(&format!("{min}m div 1")), i64::MIN);
    assert_eq!(int(&format!("{min}m mod 1")), 0);
    assert_eq!(int(&format!("{min}d = 0 - 2\nm div d")), 1 << 62);
    assert_eq!(
        int("a = 9223372036854775807\nd = 0 - 1\na div d"),
        -i64::MAX
    );
}

#[test]
fn division_truncates_toward_zero() {
    assert_eq!(int("a = 0 - 7\na div 2"), -3);
    assert_eq!(int("a = 0 - 7\na mod 2"), -1);
    assert_eq!(int("d = 0 - 2\n7 div d"), -3);
    assert_eq!(int("d = 0 - 2\n7 mod d"), 1);
}

#[test]
fn compound_division_checks_too() {
    assert_eq!(error("x = 7\nz = 0\nx div= z\nx"), "Division by zero.");
    assert_eq!(
        error("a = [7]\nz = 0\na[0] mod= z\na[0]"),
        "Division by zero."
    );
}

#[test]
fn a_skipped_operand_never_traps() {
    assert!(!boolean("z = 0\nfalse and (1 div z == 0)"));
    assert!(boolean("z = 0\ntrue or (1 div z == 0)"));
}

// --- Checks inside expressions: evaluation order and propagation ---------------------------------

#[test]
fn the_first_failing_check_in_evaluation_order_wins() {
    let setup = "z = 0\na = [1, 2]\ni = 5\n";
    assert_eq!(
        error(&format!("{setup}a[i] + 1 div z")),
        "Array index out of bounds."
    );
    assert_eq!(
        error(&format!("{setup}1 div z + a[i]")),
        "Division by zero."
    );
}

#[test]
fn a_check_in_a_loop_condition_fires() {
    assert_eq!(
        error(indoc! {"
            z = 0
            n = 0
            while 10 div z > n
                n += 1
            n
        "}),
        "Division by zero."
    );
}

#[test]
fn a_runtime_error_leaves_through_every_frame() {
    assert_eq!(
        error(indoc! {"
            f(n: int): int is 10 div n
            g(n: int): int is f(n) + 1
            h(n: int): int is 2 * g(n)
            h(0)
        "}),
        "Division by zero."
    );
}

#[test]
fn the_runtime_recovers_after_an_error() {
    assert!(run("z = 0\n1 div z").is_err());
    assert_eq!(int("1 + 1"), 2);
}

// --- Integer and float arithmetic ----------------------------------------------------------------

#[test]
fn integer_arithmetic_wraps() {
    assert_eq!(int("a = 9223372036854775807\na + 1"), i64::MIN);
    assert_eq!(
        int("m = 0 - 9223372036854775807 - 1\nd = 0 - 1\nm * d"),
        i64::MIN
    );
    assert_eq!(int("m = 0 - 9223372036854775807 - 1\n-m"), i64::MIN);
}

#[test]
fn integer_comparisons_are_signed() {
    assert!(boolean("a = 0 - 1\na < 1"));
    assert!(boolean("m = 0 - 9223372036854775807 - 1\nm < 0"));
}

#[test]
fn an_integer_power_saturates_out_of_range() {
    assert_eq!(int("2 ^ 10"), 1024);
    assert_eq!(int("2 ^ 64"), i64::MAX);
    assert_eq!(int("b = 0 - 2\nb ^ 63"), i64::MIN);
    assert_eq!(int("b = 0 - 2\nb ^ 65"), i64::MIN);
    assert_eq!(int("e = 0 - 1\n0 ^ e"), i64::MAX);
    assert_eq!(int("e = 0 - 1\n2 ^ e"), 0);
}

#[test]
fn a_float_power_is_libm_pow() {
    assert_eq!(float("2.0 ^ 0.5").to_bits(), 2f64.powf(0.5).to_bits());
    assert_eq!(float("10.0 ^ 0.3").to_bits(), 10f64.powf(0.3).to_bits());
}

#[test]
fn float_comparisons_are_ordered() {
    let nan = "n = 0.0 / 0.0\n";
    assert!(!boolean(&format!("{nan}n == n")));
    assert!(!boolean(&format!("{nan}n != n")));
    assert!(!boolean(&format!("{nan}n < 1.0")));
    assert!(!boolean(&format!("{nan}n >= 1.0")));
    assert!(float(&format!("{nan}n")).is_nan());
}

#[test]
fn negative_zero_keeps_its_sign() {
    let result = float("z = 0.0\nz * (0.0 - 1.0)");
    assert!(result == 0.0 && result.is_sign_negative());
}

#[test]
fn an_int_compared_with_a_float_is_converted_first() {
    assert!(boolean("1 == 1.0"));
    assert!(boolean("a = 9007199254740993\na == 9007199254740992.0"));
}

#[test]
fn bools_are_zero_or_one() {
    assert!(!boolean("true xor true"));
    assert!(boolean("true xor false"));
    assert!(boolean("not false"));
    assert!(boolean("true == true"));
}

// --- Ranges --------------------------------------------------------------------------------------

#[test]
fn a_range_from_a_variable_decides_inclusion_and_direction_at_run_time() {
    let sum = |range: &str| {
        int(&format!(
            "r = {range}\ntotal = 0\nfor i in r\n    total += i\ntotal"
        ))
    };
    assert_eq!(sum("1..<4"), 6);
    assert_eq!(sum("1..4"), 10);
    assert_eq!(sum("4..>1"), 9);
    assert_eq!(sum("3..<3"), 0);
}

#[test]
fn a_descending_variable_step_counts_down() {
    assert_eq!(
        int(indoc! {"
            s = 0 - 2
            total = 0
            for i in 10..1:s
                total += i
            total
        "}),
        30
    );
}

#[test]
fn a_zero_step_fails_where_the_range_is_built() {
    assert_eq!(error("s = 0\nr = 1..10:s\n0"), "Range step cannot be zero.");
}

#[test]
fn membership_respects_step_bounds_and_inclusion() {
    let member =
        |range: &str, value: i64| boolean(&format!("s = 0 - 3\nr = {range}\nv = {value}\nv in r"));
    assert!(member("1..10:3", 7));
    assert!(!member("1..10:3", 8));
    assert!(member("1..10:3", 10));
    assert!(!member("1..<10:3", 10));
    assert!(!member("1..10:3", 13));
    assert!(!member("1..10:3", 0 - 2));
    assert!(member("10..1:s", 4));
    assert!(member("10..1:s", 1));
    assert!(!member("10..1:s", 0));
    assert!(!member("10..1:s", 13));
    assert!(!member("3..<3", 3));
}

// --- Arrays and matrices -------------------------------------------------------------------------

#[test]
fn every_index_is_bounds_checked_after_wrapping() {
    let setup = "a = [1, 2, 3]\n";
    assert_eq!(
        error(&format!("{setup}i = 3\na[i]")),
        "Array index out of bounds."
    );
    assert_eq!(
        error(&format!("{setup}i = 0 - 4\na[i]")),
        "Array index out of bounds."
    );
    assert_eq!(int(&format!("{setup}i = 0 - 3\na[i]")), 1);
    assert_eq!(
        error(&format!("{setup}i = 3\na[i] = 0\n0")),
        "Array index out of bounds."
    );
}

#[test]
fn each_matrix_index_is_checked_against_its_own_dimension() {
    let setup = "m = [1, 2, 3; 4, 5, 6]\n";
    assert_eq!(
        error(&format!("{setup}r = 1\nc = 3\nm[r, c]")),
        "Array index out of bounds."
    );
    assert_eq!(
        error(&format!("{setup}r = 2\nc = 0\nm[r, c]")),
        "Array index out of bounds."
    );
    assert_eq!(int(&format!("{setup}r = 0 - 1\nc = 0 - 3\nm[r, c]")), 4);
}

#[test]
fn a_bool_element_write_touches_one_byte() {
    assert_eq!(
        value("a = [true, false, true]\na[1] = true\na[2] = false\na"),
        PinpValue::Array([true, true, false].map(PinpValue::Bool).to_vec())
    );
}

#[test]
fn a_matrix_slice_counts_its_scalar_index_from_the_end() {
    let setup = "m = [1, 2; 3, 4]\n";
    assert_eq!(value(&format!("{setup}m[-1, :]")), ints(&[3, 4]));
    assert_eq!(value(&format!("{setup}m[:, -1]")), ints(&[2, 4]));
}

#[test]
fn a_float_comprehension_converts_each_index_once() {
    // Converted from the integer index, rounding once, not by repeated float additions.
    assert_eq!(
        value("[x for x: float in 9007199254740992..9007199254740995]"),
        PinpValue::Array(
            [
                9007199254740992.0,
                9007199254740992.0,
                9007199254740994.0,
                9007199254740996.0
            ]
            .map(PinpValue::Float)
            .to_vec()
        )
    );
}

#[test]
fn identity_is_zero_off_the_diagonal() {
    assert_eq!(
        value("identity(3, float)"),
        PinpValue::Matrix {
            rows: 3,
            cols: 3,
            elements: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
                .map(PinpValue::Float)
                .to_vec(),
        }
    );
}

#[test]
fn a_large_array_round_trips() {
    assert_eq!(
        int(indoc! {"
            a = [i for i in 0..<10000]
            total = 0
            for v in a
                total += v
            total
        "}),
        49995000
    );
}

// --- Strings -------------------------------------------------------------------------------------

#[test]
fn a_function_returns_a_string_in_two_words() {
    assert_eq!(
        value("greet(): str is 'hello'\ngreet() + ' world'"),
        PinpValue::Str("hello world".into())
    );
    assert_eq!(
        value("g = 'from a global'\nread(): str is ::g\nread()"),
        PinpValue::Str("from a global".into())
    );
}

#[test]
fn strings_compare_by_content() {
    assert!(boolean("'abc' < 'abd'"));
    assert!(boolean("'ab' < 'abc'"));
    assert!(boolean("'ab' + 'c' == 'abc'"));
    assert!(!boolean("'ab' + 'c' != 'abc'"));
}

#[test]
fn a_string_length_counts_bytes() {
    assert_eq!(int("s = 'hello' + ' world'\ns.len"), 11);
}

#[test]
fn meminfo_is_evaluated_for_its_effect() {
    assert_eq!(value("meminfo()"), PinpValue::Void);
}

// --- Functions and globals -----------------------------------------------------------------------

#[test]
fn arguments_beyond_the_registers_go_on_the_stack() {
    assert_eq!(
        float(indoc! {"
            many(a: int, b: float, c: int, d: float, e: int, f: float, g: int, h: float, i: int, j: float, k: int, l: float, m: int, n: float, o: int, p: float, q: int, r: float, s: bool): float is
                a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p + q + r if s else 0.0
            many(1, 2.0, 3, 4.0, 5, 6.0, 7, 8.0, 9, 10.0, 11, 12.0, 13, 14.0, 15, 16.0, 17, 18.0, true)
        "}),
        171.0
    );
}

#[test]
fn an_int_argument_is_promoted_to_a_float_parameter() {
    assert_eq!(float("half(x: float): float is x / 2.0\nhalf(3)"), 1.5);
}

#[test]
fn a_bool_result_drives_a_branch() {
    assert_eq!(
        int("even(n: int): bool is n mod 2 == 0\n1 if even(4) else 2"),
        1
    );
}

#[test]
fn functions_and_the_entry_share_globals() {
    assert_eq!(
        int(indoc! {"
            g = 1
            bump(a: int): int is
                ::g += a
                ::g
            bump(10)
            g * 100 + bump(5)
        "}),
        1116
    );
}

// --- Control flow --------------------------------------------------------------------------------

#[test]
fn an_if_expression_in_a_loop_condition_runs_each_time() {
    assert_eq!(
        int("n = 0\nwhile (1 if n < 3 else 0) == 1\n    n += 1\nn"),
        3
    );
}

#[test]
fn deeply_nested_ifs_compile() {
    let depth = 100;
    let mut src = String::from("x = 0\n");
    for level in 0..depth {
        src += &format!("{}if x == {level}\n", "    ".repeat(level));
        src += &format!("{}x += 1\n", "    ".repeat(level + 1));
    }
    src += "x";
    assert_eq!(int(&src), depth as i64);
}

#[test]
fn a_long_straight_line_program_compiles() {
    let mut src = String::from("x = 0\n");
    for _ in 0..5000 {
        src += "x += 1\n";
    }
    src += "x";
    assert_eq!(int(&src), 5000);
}
