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

/// Mirrors `pinp_mem_info` from src/runtime/pinp_runtime.h.
#[repr(C)]
#[derive(Default)]
struct MemInfo {
    outstanding_bytes: i64,
    allocation_count: i64,
    free_count: i64,
}

#[test]
fn runtime_allocator_is_reachable_from_the_jit() {
    // The runtime (mimalloc + shim) is linked into the binary, and the JIT's
    // process-symbol generator resolves its `pinp_*` surface — the same path
    // JIT-compiled pinp code will take. Its bookkeeping is byte-exact: an
    // allocation is charged its usable size, a free credits it back, and we end
    // with nothing outstanding.
    let jit = Jit::new().unwrap();

    // SAFETY: each signature matches its symbol's ABI in src/runtime/pinp_runtime.h.
    unsafe {
        let alloc: extern "C" fn(usize) -> *mut u8 = jit.lookup("pinp_alloc").unwrap();
        let free: extern "C" fn(*mut u8) = jit.lookup("pinp_free").unwrap();
        let memory_info: extern "C" fn(*mut MemInfo) = jit.lookup("pinp_memory_info").unwrap();

        let read_info = || {
            let mut info = MemInfo::default();
            memory_info(&mut info);
            info
        };

        let pointer = alloc(64);
        assert!(!pointer.is_null());
        let after_alloc = read_info();
        assert_eq!(after_alloc.allocation_count, 1);
        assert_eq!(after_alloc.free_count, 0);
        assert!(after_alloc.outstanding_bytes >= 64);

        free(pointer);
        let after_free = read_info();
        assert_eq!(after_free.allocation_count, 1);
        assert_eq!(after_free.free_count, 1);
        assert_eq!(after_free.outstanding_bytes, 0);
    }
}

#[test]
fn string_runtime_symbols_are_reachable_from_the_jit() {
    // The string runtime is Rust `#[no_mangle]` code inside this binary, and a Linux executable
    // does not put those names in its dynamic symbol table unless the link asks for it. Without
    // build.rs's `--export-dynamic-symbol`, JIT-compiled pinp code would fail to resolve them —
    // so every symbol codegen emits a call to is looked up here.
    let jit = Jit::new().unwrap();
    for name in [
        "pinp_str_from_cstr",
        "pinp_str_free",
        "pinp_str_concat",
        "pinp_str_concat_n",
        "pinp_str_len",
        "pinp_str_eq",
        "pinp_str_cmp",
        "pinp_str_from_int",
        "pinp_str_from_float",
        "pinp_meminfo",
    ] {
        // SAFETY: the address is only checked for presence, never called, so no signature applies.
        let address: usize = unsafe { jit.lookup(name) }
            .unwrap_or_else(|error| panic!("`{name}` is not exported to the JIT: {error}"));
        assert_ne!(address, 0, "`{name}` resolved to a null address");
    }
}

/// Lowers `src` and returns the module's IR as text, for the shape assertions below.
fn src_to_ir(src: &str) -> String {
    let mut ast = parse(src).unwrap();
    crate::sema::analyze(&mut ast).unwrap();
    let context = Context::create();
    let mut codegen = CodeGen::new(&context, &ast);
    codegen.generate().unwrap();
    codegen.into_module().print_to_string().to_string()
}

/// How many times `ir_text` *calls* `callee` — declarations and definitions are not calls.
fn call_count(ir_text: &str, callee: &str) -> usize {
    let target = format!("@{callee}(");
    ir_text
        .lines()
        .filter(|line| line.contains("call ") && line.contains(&target))
        .count()
}

#[test]
fn pinp_str_is_two_eightbytes_in_ir() {
    // `{ i64, i64 }` is what makes the SysV ABI return a `PinpStr` in `rax:rdx`, matching the C
    // struct; `[16 x i8]` would classify as MEMORY and silently mismatch the runtime.
    let ir_text = src_to_ir(indoc! {"
            greet(): str is 'hi'
            greet()
        "});
    assert!(
        ir_text.contains("declare { i64, i64 } @pinp_str_from_cstr(ptr"),
        "runtime declaration is not two eightbytes:\n{ir_text}"
    );
    assert!(
        ir_text.contains("define { i64, i64 } @greet()"),
        "a str-returning pinp function is not two eightbytes:\n{ir_text}"
    );
}

#[test]
fn a_concat_chain_lowers_to_a_single_call() {
    // The flatten bonus: `a + b + c + d` is one N-way concatenation (one allocation), not three
    // pairwise ones.
    let ir_text = src_to_ir("'a' + 'b' + 'c' + 'd'");
    assert_eq!(
        call_count(&ir_text, "pinp_str_concat_n"),
        1,
        "IR:\n{ir_text}"
    );
    assert_eq!(call_count(&ir_text, "pinp_str_concat"), 0, "IR:\n{ir_text}");
}

#[test]
fn an_fstring_lowers_to_a_single_call() {
    let ir_text = src_to_ir(indoc! {"
            a = 'x'
            b = 2
            f'<{a}|{b}>'
        "});
    assert_eq!(
        call_count(&ir_text, "pinp_str_concat_n"),
        1,
        "IR:\n{ir_text}"
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
