[Human+AI]

# Retiring LLVM — notebook

Part G of milestone [0017](0017-ir-rework.md): the LLVM backend, kept as the oracle since C, is
deleted. Direction: the plan's [0017-G section](../dev-docs/ir-rework-plan.md).

---

## Entry 1 — done (2026-10-27)

**Removed:** `src/codegen/llvm/` (the AST → LLVM IR code generator, its ORC JIT wrapper, and its
37 unit tests), the `inkwell` dependency, and the `llvm` feature. With one backend left, the
`Backend` choice went too: `PinpJit::new` compiles with Cranelift, and the end-to-end helpers and
the leak suite run each program once.

**Ledger:** KI-0004 (division undefined behaviour), KI-0005 (matrix slice index), KI-0006 (float
comprehension rounding), and KI-0008 (a freed string read in a join) were defects of the LLVM
backend alone — Cranelift has each case right, with a test — so their rows are deleted. KI-0007
(counted loops at the integer limits) remains, at its `flatten` site.

**Tooling and docs:** the pre-commit hook lints without `--features llvm`; CLAUDE.md's pass table
lost the *generate* row; `dev-docs/testing-strategies.md` records that the differential run ended
with the backend; the plan's status marks the milestone complete. The README already described
Cranelift.

**Status:** 1,206 tests; clippy and rustfmt clean. Milestone 0017 is complete.
