[Human+AI]

# IR rework — milestone

**This is a major refactor, and its specs deliberately deviate from the earlier ones.** It is a
[milestone](README.md#milestones): this file is the overview, and each part is a running notebook,
not an up-front [Human]/[AI] design document — what was done, decided, and found, in the order it
happened. Entries are appended, not rewritten, so an early entry may be contradicted by a later
one — the later one wins.

The direction and the evidence behind it are in [dev-docs/ir-rework-plan.md](../dev-docs/ir-rework-plan.md). Its
sequence is a sketch, not a commitment; parts will be merged, split, and reordered as the work
reveals things, and the notebooks say when and why.

In short: pinp gets an intermediate representation between the typed AST and the backends,
modelled on Open64's WHIRL — one node type, several levels, a level being the set of operations
still allowed to appear. The *structured* level keeps loops, `if`s, and whole-array operations
(where pinp's own optimisations will run); the *flat* level is the control-flow graph (what
analyses solve over and backends read). Cranelift then replaces LLVM as the JIT backend.

Project rules are unchanged: tests first and presented before code, adversarial coverage,
in-module unit tests plus `tests/*.rs` end to end, human owns commits.

---

## Parts

| part | content | status |
|---|---|---|
| [A — structured level](0017-A-structured-level.md) | IR data structures, printer, desugaring, corpus check | done |
| [B — flat level](0017-B-flat-level.md) | `flatten` | done |
| [C — Cranelift backend](0017-C-cranelift-backend.md) | emitter from `Function<Flat>`, differential run against LLVM | done |
| [D — flat analyses](0017-D-flat-analyses.md) | dataflow and checkers on the flat level, `src/cfg/` retired | done |
| ~~E — optimisations~~ | moved out of the milestone (2026-10-27): future work, its own iteration | — |
| [F — ownership](0017-F-ownership.md) | string copies and frees as IR, placed by desugaring | done |
| [G — retire LLVM](0017-G-retire-llvm.md) | delete the LLVM backend and dependency | done |

**The milestone is complete (2026-10-27).** Order as it went: A–C first (AST → IR → Cranelift
JIT → output), then D; E left as future work; F before G, since strings leaked on the Cranelift
path until F placed their frees. Pipeline timing and KI-0007 follow, each on its own branch.

---

## Ground rules

Settled answers to the plan's open questions that shape the milestone (details in the parts'
entries and in the plan's resolved list):

- **Own containers** in `src/ir` (`IdVec`, `SideTable`, `List`/`ListPool`, `Option<Id>`), no
  `cranelift-entity` — A, Entry 3.
- **Pass verbs** *desugar* (AST → structured), *flatten* (structured → flat), *emit* (flat →
  Cranelift IR); in CLAUDE.md's table since C.
- **No behaviour change** through B: `PinpJit` compiled from the AST and the IR was exercised by
  tests only. From C, `PinpJit` compiles from the IR; each behaviour change is listed in C's
  notebook.

---

## Review (2026-11-09)

An outside review of the committed milestone found the architecture sound and raised seven
points, addressed as follows.

1. **The solver's visit budget could stop a correct analysis** whose facts grow many times. Now
   each analysis states its `height` — how often a fact can grow — and `solve` stops only when one
   block's fact grows more often than that; a block is never queued twice at once. Test: a
   thousand-step counter converges.
2. **The solver's contract was incomplete.** The docs now state that `merge` is associative,
   commutative, and idempotent with `nothing_known()` as identity, and that transfer functions
   must be monotone; `solve` merges each result into the block's fact and re-queues on `merge`'s
   flag, as documented, instead of comparing whole facts.
3. **The IR verifier ran only in tests.** Desugaring and flattening now verify their output in
   debug builds.
4. **Names:** `live_successors` → `feasible_successors` ("live" belongs to liveness); desugaring's
   string `aliases` → *ownership temporaries* (nothing is shared); `owned_globals` →
   `entry_private_globals` ("owned" belongs to string ownership). Stale `VariableSet` and lost
   `ReachabilityFact` docs rewritten.
5. **`check` versus CLAUDE.md's `check_*`:** the checkers are `check_reachability` and
   `check_liveness` (human, 2026-11-09).
6. **Compiling and analysing each desugar and flatten**, and 7. **source reads and generated
   reads look alike to liveness**: both recorded as preconditions in the plan's part E sketch.
