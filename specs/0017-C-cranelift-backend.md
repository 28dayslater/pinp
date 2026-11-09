[Human+AI]

# The Cranelift backend — notebook

Part C of milestone [0017](0017-ir-rework.md): *emit* — `Function<Flat>` → Cranelift IR — and
`PinpJit` switching to it, with the LLVM backend kept as the oracle of a differential run. Same
form as [part B](0017-B-flat-level.md): entries appended, a later one overriding an earlier one.
Direction: the plan's [0017-C section](../dev-docs/ir-rework-plan.md).

---

## Entry 1 — requirements analysis and design draft (2026-10-24)

### What the part must deliver

From the plan: today's codegen moved aside unchanged as the oracle; an emitter from the flat level;
`PinpJit` on Cranelift; a differential run of the e2e corpus on both backends that must agree on
every value and every runtime error message; the release binary size measured with `llvm` off;
`llvm` no longer a default feature. Division gets its decided runtime errors (KI-0004).

### Findings that shape the design

- **The runtime is LLVM-gated.** `build.rs` builds mimalloc and the shim only for the `llvm`
  feature, and `src/runtime` is compiled only with it. Cranelift code calls the same `pinp_*`
  symbols, so the runtime becomes unconditional.
- **Cranelift resolves host symbols itself.** `cranelift-jit` looks up undeclared functions in the
  process (`dlsym`), so the `pinp_*` names `build.rs` already exports, libc's `memset`, and libm's
  `pow` resolve with no generator of our own. LLVM's `llvm.pow.f64` lowers to that same `pow`, so
  `^` gives identical bits.
- **A runtime error is a `longjmp` through JIT frames.** `pinp_run`'s `setjmp` frame works for
  Cranelift code as it does for LLVM's: nothing to unwind, registers restored from the `jmp_buf`.
- **Cranelift traps where LLVM has undefined behaviour.** `sdiv`/`srem` by zero and `i64::MIN div -1`
  trap; `fcvt_to_sint` traps on NaN and out of range. A trap kills the process (no handler), so
  the emitter must never reach one: checked divisions test first (below); int `^` converts
  saturating (question 4).
- **The corpus avoids the known LLVM defects.** No e2e program divides by zero, slices a matrix
  at a negative scalar index, or reaches float comprehension values past 2⁵³, so KI-0004..0006
  do not block a strict differential run. Tests of the new division errors run on Cranelift only.
- **Nothing produces `ElementAddress`, `Load`, `Store`, or `Switch` yet** (no lowering passes, no
  ADTs).

### Design

**Values.** One IR value is one or more Cranelift values: `int` → `i64`, `float` → `f64`, `bool` →
`i8` (0 or 1; Cranelift has no `i1`), an array or matrix → its heap pointer, `str` → its 16-byte
descriptor as two `i64` words, a range → four values (start, stop, step, inclusive), as Entry 1 of
part B decided. A local is one frontend `Variable` per word — Cranelift's frontend builds SSA from
them; a global is a zero-initialised data object, read and written through its address.

**Calls.** pinp functions use the platform calling convention with the same word split: a `str`
argument is two parameters, a `str` result two return values. The entry keeps the uniform
`__pinp_main(out)` form, writing the result into the caller's buffer, so `PinpJit::run` and the
`pinp_run` trampoline are unchanged.

**Checks.** At a node that `may_trap`, the emitter tests the condition, branches to a cold block
that calls `pinp_runtime_error`, and continues in a fresh block — Entry 1 of part B. Messages are
today's, plus the decided "Division by zero." and "Division overflow." for `div` and `mod` alike
(`i64::MIN mod -1` too, as in Rust). Message and string-literal bytes are read-only data objects of
the JIT module.

**Operations** are translated as today's codegen does: array literals and allocation through
`pinp_alloc` (zeroed via `memset`), element access with a bounds check on each index, slices as a
copying loop, membership as the same closed-form arithmetic, string operations as `pinp_str_*`
calls, float comparisons ordered (so `NaN != NaN` is false, as today). Integer `+ - *` wrap.
Operations nothing produces yet stop with `unimplemented!` naming the operation, until a pass
produces them (question 5). Strings leak on this path until 0017-F (decided 2026-09-30).

**Pipeline.** `PinpJit::new`: parse, analyse, desugar, flatten each function, emit each into one
`JITModule`, finalize; the Cranelift verifier runs in debug builds; `opt_level = speed`.

### Open questions

1. **Layout.** The plan's: today's codegen moves unchanged to `src/codegen/llvm/`, the emitter is
   `src/codegen/cranelift/` (`emit_*` methods), and `src/codegen/mod.rs` keeps the backend-neutral
   `PinpJit`, `PinpValue`, and result reading, with a `Backend` choice. *Alternative:* `src/emit/`
   beside `src/desugar/` and `src/flatten/`, named by its verb. Recommended: the plan's — the
   module holds a backend, and a second emitter (LLVM AOT from the flat level) would sit beside it.
2. **Features.** Cranelift a plain dependency, the runtime unconditional, `llvm` optional and still
   default until the end of this part, when it stops being default (plan step 6). The pinned
   version is the current 0.137.
3. **Differential mode.** `tests/common::eval` runs each program on both backends (with `llvm` on)
   and asserts equal values or equal error messages — strict, no exception list, which the corpus
   permits today. KI-0004..0006 stay on their own branches off `main` as the ledger says, or are
   folded into this part since everything is to land in one commit.
4. **Int `^` out of range** (`2 ^ 64`): today LLVM's `fptosi` result is poison; x86 yields
   `i64::MIN`. Recommended: saturate (`fcvt_to_sint_sat`: `i64::MAX`,
   `i64::MIN`, NaN → 0) and record it in the spec; alternatively a runtime error "Integer overflow.".
5. **Unproduced operations** (`ElementAddress`, `Load`, `Store`, `Switch`): `unimplemented!` until a
   pass makes them (YAGNI, recommended), or translated now and tested on hand-built IR.

Awaiting answers; the tests follow (Entry 2).

---

## Entry 2 — answers; tests (2026-10-24)

**Answers** (human, 2026-10-24), each the recommended option: (1) the plan's layout,
`src/codegen/{llvm,cranelift}/` with the backend-neutral harness in `src/codegen/mod.rs`;
(3) KI-0004..0006 stay on their own branches — the strict differential run does not need them;
(4) int `^` saturates — `i64::MAX` / `i64::MIN`, as Rust's `as`; (5) unproduced operations stop
with `unimplemented!`. Questions 2 and 3's differential mode taken as drafted.

**Harness API.** `pub enum Backend { Cranelift, Llvm }` (`Llvm` with the `llvm` feature);
`PinpJit::with_backend(src, backend)`; `PinpJit::new`/`eval` use Cranelift.

**Tests** (written first, for review):

- `src/codegen/cranelift/tests.rs`, 46 tests on Cranelift alone: the word split of every type;
  every result type through the out-buffer (heap and inline strings, bool matrices); the division
  errors (zero divisor and `i64::MIN div -1`, `div` and `mod`, compound `div=`/`mod=` on a variable
  and an element), exact divisions beside the trapping ones, truncation toward zero, and short
  circuits that skip a trapping operand; checks in evaluation order (index before division and the
  reverse), in a `while` condition, through two frames and through 10,000 recursive frames, and a
  run after an error; wrapping `+ - *` and unary minus at `i64::MIN`; signed comparisons; saturating
  int `^` (±2⁶³, `0 ^ -1`, `2 ^ -1`); float `^` bit-equal to libm's `pow`; NaN comparisons ordered
  (`NaN != NaN` false, as today); `-0.0` kept; int/float comparison after conversion past 2⁵³;
  ranges from variables (inclusion, direction, empty, negative variable step), the zero step where
  the range is built, membership on and off step, past both ends, at an exclusive stop; wrapped and
  bounds-checked indices for reads, writes, and each matrix dimension; one-byte bool element writes;
  the KI-0005 and KI-0006 cases with the IR's (correct) results; `identity` zeroing; a 10,000-element
  array; a `str` result returned from a function and read from a global; string comparison and
  length; `meminfo()`; recursion, a call to a later function, 19 parameters (past the argument
  registers), int-to-float argument promotion, a bool result in a branch, globals shared both ways;
  an if-expression in a loop condition; 200 nested `if`s; 5,000 statements.
- **Differential run:** `tests/common::eval_result` runs every program on Cranelift and, with
  `llvm`, on LLVM, asserting equal printed results (so NaN equals NaN and `-0.0` differs from
  `0.0`); `eval` unwraps it. The error tests that called `PinpJit::eval` directly (arrays,
  matrices, ranges) go through it too, so error messages are compared as well. The e2e files lose
  their `llvm` gate; `tests/string_leaks.rs` keeps it and pins `Backend::Llvm`.

Awaiting review before implementation.

---

## Entry 3 — implemented; Cranelift is the default (2026-10-24)

**Layout.** `src/codegen/llvm/` is today's codegen, moved unchanged apart from its entry point
(`compile`); `src/codegen/cranelift/` is the emitter — `mod.rs` (module, declarations, function
setup, statements, terminators, checks), `value.rs`, `array.rs`, `string.rs`; `src/codegen/mod.rs`
keeps `PinpJit`, `PinpValue`, result reading, and `Backend`. The runtime and `codegen` are no
longer behind the `llvm` feature, and `llvm` is no longer a default feature: `cargo test` runs on
Cranelift alone, `cargo test --features llvm` adds the differential run. Pinned: Cranelift 0.137.

**Found while implementing:**

- *Three test mistakes, corrected:* pinp has no recursion and no calls to a later function yet,
  and the parser stops at 256 nesting levels — the recursion tests went, the frame test calls
  through three functions, the nesting test uses 100 levels (46 tests became 44). A program ending
  in an assignment evaluates to the assigned value, so the `Void` result test now ends in a loop.
- *Runtime bookkeeping is per thread.* The allocation counters and `pinp_run`'s error frame were
  process-wide statics. Tests run one at a time (`.cargo/config.toml`), but each on a fresh thread,
  and the strings Cranelift-compiled programs leak accumulated across a test binary — so a test
  asserting absolute counts failed. Both are `_Thread_local` in `shim.c` now (a program runs on one
  thread), which also keeps the error frame correct should tests ever run in parallel;
  `tests/string_leaks.rs` lost the mutex that serialised its measurements.
  With that, `.cargo/config.toml`'s one-test-at-a-time limit went too: both configurations
  passed 13 parallel runs of the full suite.
- *libm is linked only when referenced.* Without LLVM nothing loaded `pow`, so `dlsym` failed; the
  emitter references libm's `pow` and registers its address with the JIT.
- *Compile errors in the differential run:* the IR check in `tests/common` now skips a program that
  does not parse or type-check, as the error tests go through it too.
- `flatten`'s helpers `emit`/`emit_write` were renamed `append`/`append_write`: *emit* is now the
  Cranelift pass's verb (CLAUDE.md's pass table gained *desugar*, *flatten*, *emit*).
- `IdVec::into_items` added (with a test), so `compile` can flatten functions it owns.

**Measured** (targets, not gates): stripped release binary without LLVM **5.5 MB** (10 MB target);
source → JIT → result for a 1,000-iteration loop **~2.5 ms** per process, start-up included
(100 ms target).

**Status:** the 44 emitter tests pass; full suite 1,130 tests on Cranelift alone and 1,167 with
`--features llvm`, where the whole e2e corpus agrees on both backends (a probe on the KI-0005
program confirmed the comparison fires); clippy (both configurations) and rustfmt clean. Next,
per the plan's priority: pipeline timing and the known issues on their own branches, then 0017-D.
