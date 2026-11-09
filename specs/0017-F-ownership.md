[Human+AI]

# String ownership in the IR — notebook

Part F of milestone [0017](0017-ir-rework.md): strings stop leaking on the Cranelift path. Where a
string is copied and freed becomes explicit IR, placed while desugaring, which every backend
translates. Same form as the other parts. Direction: the plan's
[0017-F section](../dev-docs/ir-rework-plan.md).

---

## Entry 1 — scope and design (2026-10-27)

**Part E left the milestone** (human, 2026-10-27): optimisations are future work, an iteration of
their own. The letter stays vacant so references to F and G keep their meaning.

**How frees are placed** (human, 2026-10-27, after weighing three options):

| option | now | later |
|---|---|---|
| frees in the Cranelift emitter, 0014's model | least work | invisible to passes; rewritten when optimisations come |
| **IR operations, placed by desugaring** (chosen) | moderate | passes see every copy and free; copy elision is a pass over them |
| the plan's full transform (scope markers, `place_string_frees` with copy elision) | most work | — |

The chosen middle keeps the plan's direction — ownership visible in the IR, so analyses can check
it and optimisations remove what is redundant — without the editing machinery part E would have
brought. The copy-eliding pass and the scope markers wait for the optimisation work.

**The model** (`src/desugar/ownership.rs`):

- **Owned and borrowed.** A literal, a conversion to `str`, a join, a copy, or a call's result is
  owned; a read of a variable is borrowed.
- **Slots.** Every `str` variable and compiler temporary owns its value. A store frees the old
  value (`str_free`) and copies a borrowed one (`str_copy`), so no two slots share storage — the
  copy-on-borrow semantics of 0014, unchanged. A store whose value reads its own target or calls a
  function computes the value first, since either may need or replace the old one.
- **Consumers** — a join, a string comparison, `.len` — take an owned operand through an *alias*,
  a temporary that moves the string from where it is made to where it is used, and free it once
  they have run; the consumer's result goes to a temporary of its own ahead of the statement. A
  borrowed operand before one that calls a function is copied first: the call might rebind the
  global it read and free the storage.
- **Discarded** owned strings are freed at once.
- **Exit.** A function frees its slots before returning — the top level its string globals too —
  after computing its result: an owned string is moved out, a borrowed one copied first. Slots are
  freed at the function's exit rather than at each scope's end: an overwrite frees the previous
  value, so a loop does not accumulate, and a slot never written holds the empty string, whose
  free is harmless. Scope markers can tighten this later.
- **IR:** `RuntimeFunction::StrCopy` and `RuntimeFunction::StrFree` (a statement:
  `evaluate (runtime_call str_free …)`); `RuntimeFunction::name` spells them for the printer.
- **Analyses:** freeing a string is not reading it — liveness skips the arguments of `str_free`,
  so dead stores and unused variables of type `str` are still found.

---

## Entry 2 — tests (2026-10-27)

Written before the implementation; review moved to the milestone's end.

- `src/desugar/tests.rs` (+11 ownership, 7 string goldens re-pinned): a store frees first; a
  borrowed store copies; a value reading its target is computed before the free; owned operands
  aliased and freed, a discarded string freed; a function frees its locals and copies a borrowed
  result; an owned result moved uncopied; the entry copies a global it returns before freeing it;
  a discarded call result freed; a borrowed operand before a call copied before it; no copy when
  nothing after has an effect; six more shapes verify. The 0017-A string tests now pin the whole
  function, since joins, comparisons, and lengths run ahead of the statement.
- `src/ir/tests.rs` (+1): every runtime function's printed name.
- `liveness.rs` (+1): frees are not reads. `reachability.rs` (+1): a dead string store is reported
  at its target.
- `tests/string_leaks.rs`: **now runs every program on Cranelift**, and with `--features llvm` on
  LLVM too, each balanced and the values equal; +7 cases: a global rebound in a function, a read a
  later operand frees, if-expression strings in a loop, a literal comparison in a loop condition,
  interpolation in a loop, a slot written only on an untaken branch, a swap.

---

## Entry 3 — implemented (2026-10-27)

As designed. The Cranelift emitter translates `str_copy` (`pinp_str_concat_n` of one part) and
`str_free` (`pinp_str_free`); the LLVM backend compiles from the AST and is untouched. One
refinement found on the goldens: an operand already read from an alias is used directly rather
than moved into a second one.

**Found:** the LLVM backend has always read freed storage in `g + f()` when `f` rebinds `::g` —
its join receives the old descriptor after the call freed it. Recorded as **KI-0008**; the leak
test for that shape runs on Cranelift only, and the defect leaves with the LLVM backend in G.

**Status:** full suite 1,206 tests on Cranelift alone and 1,217 with `--features llvm`; the leak
suite balanced on both backends; clippy (both configurations) and rustfmt clean. Next: 0017-G.
