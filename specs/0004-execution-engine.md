[Human]

An execution engine is required to perform end-to-end tests. We are using LLVM's JIT.

Requirements:

- Execute the *existing* language (arithmetic, functions, calls, globals, compound assignment) and
  return the resulting value to the caller. No new language constructs in this iteration.
- The backend must be **optional**: the lexer and parser must still build and test on a machine with
  no LLVM installed. Only the execution path needs LLVM.
- Provide a small, layperson-readable harness that takes a source string, runs it, and returns a
  value — this is what runtime unit/integration tests are written against.
- Errors (lex/parse/type) must surface the normal way (a Rust `Result`), without reaching code
  generation.

[AI]

## What was added

A feature-gated LLVM backend plus a JIT harness. Nothing in the lexer/parser changed; this iteration
is pure plumbing under the existing typed AST.

### Build configuration

- `inkwell` (a safe-ish Rust wrapper over the LLVM C API) is an **optional** dependency behind a new
  `llvm` cargo feature, off by default:

  ```toml
  [features]
  llvm = ["dep:inkwell"]

  [dependencies]
  inkwell = { version = "0.9", features = ["llvm22-1"], optional = true }
  ```

- The `llvm22-1` feature pins the bindings to **LLVM 22.x**; `inkwell` refuses to build against a
  mismatched LLVM, so compatibility is self-enforcing (no vendoring needed). The actual LLVM is a
  system dependency (on Arch: the `llvm` + `llvm-libs` packages).
- `cargo test` runs the front-end with no LLVM; `cargo test --features llvm` adds the backend and the
  end-to-end suite.

### `src/codegen.rs` (gated behind `llvm`)

- **`Jit`** — a thin, safe wrapper over LLVM's **ORCv2 LLJIT** C API. `inkwell` 0.9 ships no ORC
  bindings, so this reaches the C API through `inkwell::llvm_sys` (no extra dependency, no version
  skew). The `unsafe` FFI is quarantined in `new` / `add_module` / `lookup` / `Drop`; call sites stay
  readable:

  ```rust
  let jit = Jit::new()?;
  jit.add_module(module)?;
  let f: extern "C" fn() -> i64 = unsafe { jit.lookup("jitted")? };
  ```

- **`CodeGen`** — lowers a parsed `Ast` to an LLVM module. Each pinp function becomes an LLVM
  function; top-level globals become module globals; top-level statements are emitted into a
  synthetic `__pinp_main` whose return value is the program's final expression. Covers the whole
  current language: literals, unary minus, the binary operators (`+ - * / div mod ^`), variables and
  function-local allocas, globals, calls, and `Int -> Float` promotion. `^` is lowered via
  `llvm.pow.f64`.
- **`PinpValue`** — the program's result: `Int(i64)`, `Float(f64)`, or `Void`.
- **`PinpJit`** — the harness: source string in, executed, value out.

  ```rust
  let value = PinpJit::eval("2 + 3 * 4")?;   // PinpValue::Int(14)
  ```

  `PinpJit::new` parses, type-checks, and JIT-compiles; `run` executes and returns a `PinpValue`
  chosen from the statically inferred result type. Lex/parse/type errors are returned as
  `Err(String)` before code generation is reached.

### Test organization

End-to-end behaviour is verified via `PinpJit` in `tests/` (one binary per language area), with
shared helpers in `tests/common/mod.rs`. `tests/expr.rs` is the first such file (expression
evaluation). Component-level facts (token output, AST shape, type errors) stay as in-module unit
tests.

## Known caveats (for future iterations)

- **Throwaway `ThreadSafeContext`.** LLVM 22 removed `LLVMOrcThreadSafeContextGetContext`, so each
  module is wrapped with a fresh `ThreadSafeContext` purely for ORC's locking. Harmless while the JIT
  is single-threaded; revisit before any concurrent compilation.
- **Context lifetime.** The inkwell `Context` a module was built in must outlive the `Jit` that owns
  the module (ORC frees the module on dispose). `PinpJit` enforces this by field drop order.
- **`^` precision.** Integer exponentiation goes through `f64` (`llvm.pow.f64`); exact for the ranges
  in tests, but very large integer powers would lose precision. A dedicated integer-power path is
  future work.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
