[Human+AI]

# The analyses on the flat level — notebook

Part D of milestone [0017](0017-ir-rework.md): the static analyses of
[0015](0015-static-analyses-start.md) — the dataflow solver and the reachability and liveness
checkers — move from the AST-borrowing CFG of `src/cfg/` onto `Function<Flat>`, and `src/cfg/` is
retired. Same form as the other parts: entries appended, a later one overriding an earlier one.
Direction: the plan's [0017-D section](../dev-docs/ir-rework-plan.md).

---

## Entry 1 — requirements analysis and design draft (2026-10-25)

**Order.** The plan put pipeline timing and the known-issue fixes right after C. Both go on their
own branches, and the milestone is to be finished before it is published (human, 2026-10-25), so
they wait until 0017 is done; D follows C directly.

### What the part must deliver

From the plan: `solve` over `Function<Flat>`; facts over dense ids as bitsets (0015 compromise 3);
reachability and liveness ported, reported through the arena's spans; findings may only become
*more* precise, each newly reported case tested and listed here; `src/cfg/` retired. The public
entry point stays `analysis::check(ast) -> Vec<Diagnostic>`, so `tests/analysis.rs` and
`tests/diagnostics.rs` keep their shape.

### Findings that shape the design

- **Liveness tracks names, the IR tracks bindings.** Today's fact is a set of `SymId`s, so two
  bindings of one name in different scopes are one variable to the checker. On the IR each is its
  own `LocalId`: findings for shadowed names can only become more precise.
- **Top-level variables are globals in the IR.** Today the top-level graph analyses them like
  locals unless a function reaches them with `::` (`escaping_symbols`). The plan's "escaping
  symbols go, globals are a different id type" would silently drop every top-level finding, so
  the entry function's facts cover its **globals** as well as its locals, and a global any other
  function reads or writes stays exempt and live at the end — today's rule, now computed from the
  IR (`ReadGlobal`/`WriteGlobal` outside the entry).
- **Loop bindings become ordinary writes.** Today a `for` binder is a bare `Bind` action, never
  reported. On the IR, a `for` value binder (`for v in a`), index binders, and a comprehension's
  `float` variable are named locals written by ordinary `WriteLocal`s — so an unused one would now
  be reported (question 1). A range loop's index is read by the loop's own test and step, so it
  is never reported.
- **A literal empty range is no longer visible in the branch.** Today the checker sees a `for`
  over `3..<3` and prunes the body. After `flatten`, the loop test reads the range through a saved
  temporary, so it is no longer a constant. The checker needs to know which header belongs to
  which range: the plan's *structure map*, of which only this much is needed (question 3).
- **Defect in today's emptiness rule.** `literal_range_is_empty` takes an inclusive range without
  a step as empty when `start > stop`, but such a range counts *down* (`5..1` is 5, 4, 3, 2, 1):
  `for i in 5..1` is reported as code that can never run, and the loop runs. The ported rule
  follows the range's real semantics (question 2).
- **No single exit block.** The flat level ends in `Return` (and, later, `RuntimeError`)
  terminators instead of one `Exit` sink. A backward analysis starts at every returning block; a
  runtime-error exit needs nothing to be live after it.

### Design

- **`dataflow.rs` over `Function<Flat>`.** `Analysis` keeps its shape: a fact, a direction, a
  boundary, `transfer_statement(node)` and `transfer_terminator`, and `live_successors`; `solve`
  keeps its worklist and visit budget, with the `'ast` lifetime gone. Facts: `ReachabilityFact`
  (a bool) and `VariableSet`, a bitset over the function's locals followed by — in the entry — its
  globals. A `replay_backward` helper walks a solved block statement by statement, which the
  liveness checker needs for each write (the plan's `replay_forward` has no user yet).
- **Reachability.** A branch on a `bool` constant takes one edge; a counted-loop header whose range
  is two integer constants and provably empty takes only the exit. An unreachable block is
  reported at its first spanned statement, or its terminator's operand, as today.
- **Liveness.** A write kills its variable, a read makes it live. Unused variable: a named
  variable (`_` excepted) with no read anywhere, reported at its first write. Dead store: a write
  whose variable is not live after it. Escaping globals exempt.
- **`analysis::check`** desugars and flattens the program — the analyses are on request, separate
  from compiling — and runs both checkers on every function, sorted by position as today.
- **Retire `src/cfg/`**; its tests go, the solver's tests move to IR built from source.

### Open questions

1. **Unused loop bindings.** `for v in a` whose body never reads `v`, `for i, v in a` without `i`,
   a comprehension's unused `float` variable: report them as unused variables, as `rustc` does
   (`_` as the binder silences it), or exempt binders made by a `for`? Recommended: report them.
2. **The `5..1` defect.** Fix it here, since the rule is rewritten in this part anyway
   (recommended), or keep today's behaviour and record it as a known issue?
3. **Structure map.** Only what reachability needs — each counted-loop header's range node,
   recorded by `flatten` in the flat shape (recommended) — or the plan's full map (header, body,
   exit, range, index per loop, and the construct each block came from), which nothing needs yet?

Awaiting answers; the tests follow (Entry 2).

---

## Entry 2 — answers; tests (2026-10-25)

**Answers** (human, 2026-10-25), each the recommended option: (1) unused loop binders are reported,
`_` silences them; (2) the `5..1` defect is fixed here; (3) `flatten` records only each counted-loop
header's range. Following from (1): the AST records no binder spans, so a finding could not point
at the binder — **the parser now records them** (`Stmt::For::var_span`, `Stmt::ForArray::binder_spans`,
`Node::Comprehension::var_span`), as 0016 did for assignment targets, and desugaring puts them on
the binder's write (human, 2026-10-25).

**Shared step.** Compiling and analysing both need "desugar, then flatten every function", so
`Program` becomes generic over its shape (`Program<Structured>`, `Program<Flat>`) and
`flatten::flatten_program` does it once; `Flat` gains `counted_loop_ranges` (header block → range
node).

**Tests** (written first, for review):

- `src/flatten/tests.rs` (+5): the header's range recorded for a written range and for a range
  variable (its read); none for `while`, `loop`, or `if`; one per nested loop, outer first;
  `flatten_program` keeps every function, in order, and the entry.
- `src/analysis/dataflow.rs` (11, replacing 7): the old cases over IR built from source, plus a
  pruned edge carrying no facts, `replay_backward` giving the fact after each statement, and
  `VariableSet` across several 64-bit words and equal regardless of history.
- `reachability.rs` (17, replacing 11): every old case; the `5..1` correction (and `1..1:7`,
  `1..5:2` as non-empty, `5..1:2` and `1..5:-1` as empty); a dead `elif` arm; a dead if-expression
  arm (**newly reported**); `false and …` deliberately not followed; a range variable and an array
  loop never assumed empty; three findings for three dead blocks of nested code, as today.
- `liveness.rs` (30, replacing 20): every old case; top-level variables (now globals) analysed
  like locals unless a function touches them; unread `for` value binder and `float` comprehension
  variable reported (**new**), `_` silencing them, the index binder and an `int` comprehension
  variable never (the loop reads them); same-named bindings in separate bodies as separate
  variables (**more precise**: an unused variable where a dead store was reported); swaps, chained
  assignment, compiler temporaries, unread parameters; 70 variables in one function.
- `tests/analysis.rs` (+3): `5..1` not dead, an unread loop value located at its binder, a dead
  if-expression arm located.
- `src/parser/tests.rs` (+3) and `src/desugar/tests.rs` (+3): binder spans recorded, and used for
  the binder's write.
- `src/cfg/tests.rs` (11) goes with the module.

Awaiting review before implementation.

---

## Entry 3 — implemented (2026-10-27)

Review moved to the milestone's end (human, 2026-10-25): the parts continue without a stop.

Done as designed: binder spans (parser, desugar); `Program<S>` and `flatten::flatten_program`,
which the Cranelift backend now uses too; `Flat::counted_loop_ranges`; `dataflow.rs` over
`Function<Flat>` with `VariableSet` and `replay_backward`; both checkers ported; `analysis::check`
desugaring and flattening on request; `src/cfg/` deleted. CLAUDE.md's pass table lost its *build*
row (the CFG is what `flatten` makes) and *solve* now reads the flat IR.

**Found while implementing:**

- *Sema rejected `_` as a `for` value binder*, so an unread loop value — now reported — could not
  be silenced. **`_` is now a legal value binder** (human, 2026-10-25): `for _ in a` runs once per
  element. Five sema tests flipped from error to accepted; added: `_` cannot be read, it hides no
  outer name, and e2e on arrays and matrices (both backends agree).
- **A part B defect, fixed:** `flatten` took an exclusive range's direction from its spelling,
  while the language (and the LLVM backend) take it from a written step's sign. Sema accepts
  `5..>1:2`; LLVM ran it zero times, Cranelift stepped up past `5` toward overflow — the corpus
  never tried it. A written constant step now decides, and a step from a variable decides at run
  time. Tests: three flatten goldens, two e2e (loops and membership, both backends).
- Reachability's emptiness rule follows the same semantics: a written step decides the direction.
  Sema rejects a literal range whose spelling contradicts its bounds (`1..>5`, `5..1:2`), so the
  empty literal ranges are equal bounds with an exclusive stop, or a step leading away.
- Test programs corrected: a function may use `::g` only after `g` is defined.

**Status:** full suite 1,159 tests on Cranelift alone and 1,196 with `--features llvm`; clippy
(both configurations) and rustfmt clean. Next: 0017-E.
