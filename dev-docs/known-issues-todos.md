# Known Issues / TODOs

Deferred / known problems we're consciously living with — not yet worth their own
spec step. Each row is a **`KI-NNNN`** (zero-padded to 4 so ids sort correctly).

Conventions:
- **Add** a row when you *decide to defer* something real — not for every papercut.
- Trivial, code-local stuff stays as an inline `// TODO` / `// FIXME` instead.
- When an issue lives at a spot in the code, drop `// TODO(KI-NNNN): …` there so
  code ↔ ledger is one grep (`rg -n 'KI-[0-9]{4}'`).
- **Delete** a row when fixed (git log is the history); **promote** to a spec
  when it grows teeth.
- **This ledger is live state; `specs/` is the frozen decision record.** A spec's
  Deferred section says what we chose not to build *then*; a row here says what is
  wrong *now*. If never doing it would leave something **wrong**, it belongs here;
  if it would leave something **missing** (a feature not yet built), it belongs in
  a spec's Deferred list, not here.

| ID | Area | Severity | Status | Issue / notes |
|----|------|----------|--------|---------------|
| KI-0001 | lexer | low | open | "Inconsistent indentation." is reported at the newline *ending the previous line*, not at the offending indentation — a bad indent on line 3 reports line 2, one column past its last character. `emit_indent` receives the pending newline's offset instead of the offending line's indent start. Preserved deliberately by [0016](../specs/0016-diagnostics.md) under its no-behaviour-change constraint and locked in by `inconsistent_indentation_keeps_its_position` in `tests/diagnostics.rs`; fixing it means changing that test too. |
| KI-0002 | sema | low | open | `` `Sub` is not defined for str. `` renders the Rust `BinOp` variant name straight into user-facing text via `{op:?}` — `Sub`/`Mul`/`IntDiv` are internal identifiers, not pinp syntax, and the reader has no way to map them back to the operator they typed. `string_bin_type` in `src/sema/analyzer.rs` is the only site. Wants a `BinOp` → source-spelling mapping (`Sub` → `-`), which several other messages would use too. Left alone by [0016](../specs/0016-diagnostics.md) under its no-message-change constraint. |
| KI-0003 | sema | low | open | A matrix row or column index out of range reports "Array index out of bounds." — `indexed_assign2d_check` reuses `check_literal_index`, which only knows the length it was handed, so the message says *array* for a matrix and does not say which of the two indices was wrong. Position is correct (it points at the offending index); only the wording is. Left alone by [0016](../specs/0016-diagnostics.md) under its no-message-change constraint. |
| KI-0007 | flatten | medium | open | A counted loop whose range ends within one step of the integer limits never terminates: `for i in (9223372036854775807 - 1)..9223372036854775807` runs forever, as does a descending one ending near `i64::MIN` or a stepped one whose next value would pass either limit. The loop tests the index against the stop, then adds the step, which wraps past the stop and keeps the test true. Fix: test before stepping whether the index has reached the last value (or step with an overflow check and leave on overflow). Site: `counted_loop` in `src/flatten/mod.rs` (every loop the IR runs, comprehensions too). Found during 0017-C. |
