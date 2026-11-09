[Human+AI]

# The flat level — notebook

Part B of milestone [0017](0017-ir-rework.md); continues [part A](0017-A-structured-level.md) in
the same form: a running notebook, entries appended, a later entry overriding an earlier one.
Direction and evidence: [dev-docs/ir-rework-plan.md](../dev-docs/ir-rework-plan.md).

Per the priority set on 2026-09-29 (AST → IR → Cranelift JIT → output first), this iteration
starts with what Cranelift needs — `flatten` — and ports the static analyses onto the flat level
only after programs run through Cranelift (0017-C).

---

## Entry 1 — the shortest path to Cranelift: `flatten` only (2026-09-30)

**Decision: checks stay in expressions.** The plan had `flatten` turn bounds, zero-step and
division checks into branches to error blocks. A check sits *inside* an expression
(`x = f() + a[g()]` checks after `f()` and `g()`, before the load), and a branch can only sit
between statements — so that would mean pulling expressions apart, with the operand saving
desugaring already needs, in every pass. Instead a check stays where it is — the `checked` flags,
the `Range` zero-step rule — and the Cranelift emitter splits its block at the check (Cranelift's
IR is flat SSA; a block can end anywhere), jumping to a cold block that raises the runtime error.
Same machine code, no reordering. For the flat-level analyses, a trapping node is an implicit exit:
the program ends there.

**Consequence: no lowering passes before Cranelift.** They existed to break big operations into
small ones inside the IR; the emitter can do that itself, as today's LLVM codegen does. So:

- **Level change.** Only structured control flow — `If`, `While`, `DoWhile`, `CountedLoop` — is
  barred from the flat level; every value operation (element access, array literals and
  allocation, slices, ranges, membership) is legal at both, the backend translating it.
- **`RangeField { range, field }`** (start, stop, step, inclusive): a flattened counted loop reads
  its range through it. A range value stays one IR value; the emitter holds it as four machine
  values.
- **No editing API yet**: `flatten` reuses every node in place and only builds block lists. The API
  arrives with the first optimisation that edits.

**`flatten`** (`src/flatten/`) consumes a `Function<Structured>` and returns the `Function<Flat>`
over the same arena: every node keeps its id, type, and span.

- Statements go into the current block in order. `If` branches to a `then` block and to an `else`
  block (or straight to the join when there is none); both end in a `goto` to the join, where
  flattening continues.
- `While`: `goto header`; the header runs its statements, then branches to the body or the exit;
  the body ends with `goto header`. `DoWhile`: `goto body`; the body ends with the test — `until`
  leaves when true, otherwise repeats when true.
- `CountedLoop`: the range evaluated once into a temporary; the index set to its start; the header
  tests the index against the stop; the body ends by adding the step. The test is as specific as
  the range allows: a range written in place has a known kind, so no run-time test of inclusion,
  and an exclusive one a known direction too (`..<` up, `..>` down); a range from a variable
  decides both at run time from its fields.
- The function's result becomes the final `return`.
- Blocks are numbered as they are met: an `If` creates then / else / join; a loop creates its
  header / body / exit **before** flattening what is inside them.

**Tests** (written first, for review): 15 in `src/flatten/tests.rs` — golden output per construct,
nesting, block numbering, node identity kept — plus IR test changes (the level table; three tests
of "a structured value in a flat function" removed, their premise gone; `RangeField`; every value
operation legal at the flat level). The corpus check in `tests/common` now also flattens and
re-verifies every function of every end-to-end program.

Awaiting review before implementation. *(Implemented — Entry 3.)*

---

## Entry 2 — which level analyses run on (2026-09-30)

With checks and array operations kept, the flat level loses only statement nesting. Flow analyses
(liveness, reachability, dead stores, constants, definite assignment, string ownership) run on it:
one dataflow framework over blocks handles any control flow, where structured analysis needs a
rule per construct — and `break`, `continue`, early `return`, `match`, and coroutines are coming.
It is Rust's approach too: its borrow checker runs on MIR, a flat control-flow graph that keeps
high-level operations. Analyses that need loop structure (bounds, trip counts, loop optimisations)
run on the structured level, as the plan already has bounds-check elimination.

Compared with Rust's MIR, pinp's flat level has what pinp's analyses need, except **scopes**:
MIR marks where each variable's storage ends (`StorageDead`). Scope markers arrive with 0017-F,
where freeing first needs them. Rust's reference/borrow information has no pinp counterpart and
none is planned — pinp will not do borrow checking (human, 2026-09-30).

Deferred to the analyses port, where the first consumer is: a **structure map** recorded by
`flatten` (per loop its blocks and original range and index; per block its construct), so flat
checkers report at the right construct without rediscovering loops.

`flatten` takes the program tables as well (`flatten(function, &mut tables)`): the loop test
creates `bool` and `int` nodes whose types must be interned.

---

## Entry 3 — `flatten` implemented (2026-09-30)

`src/flatten/mod.rs`: one recursive walk building per-block statement lists and terminators, then
assembling `Function<Flat>` over the same arena. `RangeField` and the narrowed level rule are in
`src/ir`. A counted loop's test specialises on what its range node shows: kind always when written
in place, direction for the exclusive kinds and for a constant step; a range from a variable is
tested at run time. The corpus check now flattens and re-verifies every function of all 211
end-to-end programs.

**Status:** 15 flatten tests and 154 IR tests pass; full suite (1,122), clippy
(`--features llvm`) and rustfmt clean. Next: the Cranelift backend (0017-C).
