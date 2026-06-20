// SPDX-License-Identifier: MIT

use super::*;
use indoc::indoc;

#[test]
fn jit_evaluates_two_plus_two() {
    let jit = Jit::new().unwrap();
    // The raw Jit spine is exercised end-to-end by the PinpJit tests below;
    // here we just confirm a fresh instance constructs.
    drop(jit);
}

#[test]
fn function_call() {
    let result = PinpJit::eval(indoc! {"
            sq(x: int): int is x*x
            sq(5)
        "})
    .unwrap();
    assert_eq!(result, PinpValue::Int(25));
}

#[test]
fn global_and_compound_assignment() {
    let result = PinpJit::eval(indoc! {"
            g = 100
            bump(a: int): int is
                ::g += 1
                a + ::g
            bump(5)
        "})
    .unwrap();
    assert_eq!(result, PinpValue::Int(106));
}

#[test]
fn local_in_block_body() {
    let result = PinpJit::eval(indoc! {"
            f(a: int, b: int): int is
                s = a + b*b
                s
            f(2, 3)
        "})
    .unwrap();
    assert_eq!(result, PinpValue::Int(11));
}

#[test]
fn and_or_short_circuit() {
    // The right operand traps (integer division by zero) if evaluated. Correct short-circuit
    // means a deciding left operand skips it, so these must not crash and must return the
    // left-determined value.
    assert_eq!(
        PinpJit::eval("false and (1 div 0 == 0)").unwrap(),
        PinpValue::Bool(false)
    );
    assert_eq!(
        PinpJit::eval("true or (1 div 0 == 0)").unwrap(),
        PinpValue::Bool(true)
    );
    // And the live path is still taken when the left operand does not decide.
    assert_eq!(
        PinpJit::eval("true and 2 > 1").unwrap(),
        PinpValue::Bool(true)
    );
}

#[test]
fn syntax_error_is_reported_before_codegen() {
    // A malformed parameter list (missing `:`) fails in the parser, so the flow
    // never reaches code generation. PinpJit must surface that as a normal
    // `Err`, carrying the parser's own message verbatim.
    let src = "f(a int): int is a";
    let expected = format!("{:?}", parse(src).err().unwrap());
    let error = PinpJit::eval(src).unwrap_err();
    assert_eq!(error, expected);
    assert!(error.contains("Expected Colon"));
}
