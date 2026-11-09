# Testing strategies

Ways of checking pinp beyond hand-written expected values: what is in place, and what is worth
adding. Ideas, not commitments; each gets a spec entry when it is taken up.

## In place

- **IR corpus check:** every end-to-end program is desugared, flattened, and verified at both
  levels.
- **Leak balance:** `tests/string_leaks.rs` checks the allocator's own counters after every
  program — each allocation freed exactly once.

While the LLVM backend existed (0017-C to G), every end-to-end program also ran on both backends,
which had to agree exactly; it ended with the backend.

## Worth adding

- **Metamorphic testing.** Programs that must give identical results without anyone knowing the
  result: `a + b` against `b + a`, `a * b` against `b * a`, a loop against its unrolled form, an
  expression against the same one through a temporary or a function. Only where pinp's semantics
  permit it — integer `+`/`*` wrap and commute; float `+` and `*` commute but do not associate,
  and NaN never compares equal, so float relations need care. With one backend left, the natural
  successor to the differential run.
- **Optimisation equivalence**, once pinp has optimisations: every program run with them on and
  off, results and runtime errors compared.
- **Reference calculations.** Expected values computed in Rust rather than written by hand, for
  well-defined operations: integer arithmetic (`wrapping_*`, `checked_div` for the error cases),
  float arithmetic and `powf` compared bit for bit, comparisons, ranges as Rust iterators. Partly
  done: the Cranelift emitter's tests compute some (`powf`, `i64::MIN`); most end-to-end tests
  still compare against literals. Pairs well with generated inputs — the expectation is computed,
  so the inputs need not be hand-picked.
