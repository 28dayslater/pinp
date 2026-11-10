# IR rework — implementation plan

Status: **accepted as a direction, not as a schedule.** The design and the evidence below stand;
the iteration sequence is a sketch and will change as the work reveals things. What was actually
done, decided, and found is recorded in milestone [specs/0017-ir-rework.md](../specs/0017-ir-rework.md)
and its parts, running notebooks.

**Status (2026-10-27): milestone 0017 is complete** — source → IR → Cranelift JIT → output, with
the LLVM backend gone. Done: A (IR, desugaring), B (`flatten`), C (the Cranelift backend), D (the
analyses on the flat level, `src/cfg/` retired), F (string copies and frees as IR operations), G
(LLVM removed). E — the optimisation framework — left the milestone as future work. Measured
against the targets: a 5.5 MB stripped release binary, ~2.5 ms from process start to result for a
small loop program. Behaviour changed where the parts say: division by zero and `i64::MIN div -1`
are runtime errors, an out-of-range `int` power saturates, a written step decides a range's
direction, `_` may be a loop's value binder, strings no longer leak. Next, each on its own branch:
pipeline timing, KI-0007. The sketch below is kept as written, annotated where things went
otherwise; the parts' notebooks say what was done.

## Why

Today the typed AST is consumed directly by two unrelated passes:

- **codegen** walks the AST into LLVM IR, and in doing so owns most of pinp's semantics: index
  normalisation and bounds checks, range defaults and the zero-step guard, slice copies,
  comprehension loops, promotions, string ownership (`owns_str_result`, `free_scope_strings`), and a
  second copy of sema's scope resolution (`CodeGen::locals`);
- **the analysis layer** builds a separate CFG that *borrows* AST statements (`Cfg<'ast>`), and so
  inherits the AST's shape: if-expressions and `and`/`or` stay atomic (0015's "if-expression
  compromise"), and `for` loops need a fake `Action::Bind` and a branch on the source expression.

Porting codegen to Cranelift as it stands would duplicate all of those semantics in a second
backend. And since the analyses are to start driving optimisation, they must see the same program
the backend compiles.

The fix is an IR between the AST and the backends, modelled on Open64's WHIRL: **one node type,
several levels, where a level is the set of operations still allowed to appear**, lowered in stages.

## Constraints

- **Latency first.** Minimal time from parse to execution is the #1 priority (decided
  2026-09-29): only optimisations that are cheap at compile time *and* pay off at run time.
- **Targets, not gates** — source → JIT → result around **100 ms** for typical code, watched
  through the pipeline timings (it depends on the size of the code); release executable
  **under 10 MB**, watched by checking its size. Neither fails the build.
- **No behaviour change** until the optimisation part (0017-E), except where a step says
  otherwise and its spec lists the change.
- **Cheap optimisations only.** Every pinp-owned pass is linear, or close to it, in one function.
  Anything Cranelift already does (value numbering, loop-invariant hoisting, algebraic
  simplification, redundant loads) is not re-implemented.
- **No own SSA.** Cranelift's frontend builds SSA from plain variables (Braun et al.); pinp's IR
  stays non-SSA.
- Project rules apply unchanged: tests first and adversarial, in-module unit tests plus
  `tests/*.rs` end-to-end, SPDX headers, naming conventions, human owns commits.

## Evidence (measured 2026-09-22/23)

| Measurement | Result |
|---|---|
| Current AST, 1,800-line generated program | 7,800 expression nodes, 1.09 MB live (**140 B/node**), **2.04 allocations/node**, parse + sema 2.7 ms |
| `Node` / `Stmt` / `PinpType` | 56 / 72 / 24 bytes |
| LLVM 22 heap per `add` / store+load / 2-arg call | 144 / 288 / 192 bytes |
| Draft `Operation` (compiled) | **16 bytes**, statically asserted |
| Loop example (`for i in 0..n: total += xs[i mod 4]`) | today's LLVM IR: 65 instructions, 8 blocks; draft structured IR: **21 nodes** |
| LLVM `-O2` on that IR | recovers an 11-instruction loop: bounds check, zero-step guard and range struct removed |
| Cranelift 0.136 on the same question | no value-range analysis; `srem` folds only on constants — the bounds check would stay |
| Cranelift JIT probe (x86 only, release, stripped) | 5.2 MB; ~40 µs/function (`none`), ~60 µs/function (`speed`); process start + JIT ≈ 5 ms |

Consequence: after the port nothing downstream rediscovers pinp's facts (static sizes, loop
structure, immutable loop variables), so the IR must keep them.

## Target architecture

```text
source → AST ──desugar──▶ Function<Structured> ──(cheap pinp optimisations)──▶ flatten
                                                                                  │
          (checks, when asked) ◀── Function<Flat> = the CFG ◀──────────────────────┘
                                         │ ──(flat optimisations)──▶ Cranelift (SSA, scalar opts) ──▶ machine code
                                         └────────────────────────▶ textual .ll → clang (optional AOT tool, later)
```

Processing has **two phases** after whole-program sema: every function is desugared and summarised
(see *Interprocedural summaries* under Later), then each function in turn is optimised → flattened
→ optimised → emitted. Arenas stay per function: all are alive after phase 1 (once recursion and
closures land, a summary may depend on a function defined later), and each is freed once its
function is emitted. The AST is dropped after phase 1 except its name table and string literals
(~140 B per AST node against ~32 B per IR node), and one Cranelift compile context and builder
context are reused across functions. Memory estimates and the fallback (desugar twice) are in
[ipo-and-backend-memory.md](ipo-and-backend-memory.md).

### Module layout

```text
src/ir/                  Operation, Arena, Function<Structured|Flat>, tables, verifier, printer
src/desugar/             AST → Function<Structured>
src/flatten/             Structured → Flat
src/analysis/            dataflow.rs (solve), liveness, reachability, constants, ownership — facts only
src/analysis/checks/     facts → Diagnostic (warnings, on request)
src/optimize/            facts → edits; the pass pipeline
src/codegen/cranelift/   Function<Flat> → Cranelift IR (the JIT)
```

`src/cfg/` is retired in 0017-D: the flat level *is* the control-flow graph.

### Design decisions (from the WHIRL study)

1. **Two levels, one node enum.** *Structured* keeps `If`/`While`/`DoWhile`/`CountedLoop` and whole
   array operations (`ElementGet`, `Slice`, `Range`) — where pinp's own optimisations run.
   *Flat* has basic blocks and terminators — what analyses solve over and what backends read.
   (Revised by 0017-B: every value operation, whole-array ones included, is legal at both levels;
   only nested control flow is barred from the flat level.)
2. **The level is a type.** `Function<Structured>` / `Function<Flat>` (typestate) fixes the
   container, so a flat function cannot even hold a nested `If`; `verify` checks the operation set.
   WHIRL enforced levels only by convention.
   **Partial lowering** (decided 2026-09-27, from MLIR): the typestate governs *control flow* only.
   `ElementAddress`/`Load`/`Store`/`RuntimeCall` are legal at both levels, so operations are
   lowered by small separate passes while the function is still structured, and `flatten` only
   builds the control-flow graph. Lowering passes edit in place, never rebuild.
3. **16-byte nodes.** Every payload is `u32` ids (≤ 12 bytes plus up to three one-byte fields);
   anything larger goes out of line behind an id. `const _: () = assert!(size_of::<Operation>() == 16)`.
4. **Struct-of-arrays arena.** `operations`, `types` (`TypeId`), `spans`, `locals`, and a
   `ListPool` for variable-length children, bodies and arguments (`List`, 4 bytes). No `Vec`,
   `String` or `Box` inside a node — ever.
5. **Interning.** `PinpType` (24 B) → `TypeId` (4 B); constants → `ConstantId`, floats stored as
   bits (NaN equals itself, `-0.0` ≠ `0.0`).
6. **Resolved names.** `LocalId` / `GlobalId` from desugaring onward; codegen's scope stack goes.
   Compiler temporaries are locals with no name, and checkers never report them.
7. **Trees, not DAGs.** Every node has exactly one parent (verified), so a node can be replaced in
   place and only that one use changes; ids — and the side tables indexed by them — stay valid.
8. **Derived graphs.** Predecessors, def-use, parent links: computed on demand into side tables,
   never stored in nodes.
9. **Checks as flags.** `ElementGet`/`ElementSet`/`IntegerDivide` carry `checked`; an analysis
   that proves safety clears it, and the backend emits the check only while it is set (checks stay
   inside expressions — 0017-B).
10. **Own containers in `src/ir`, no `cranelift-entity`** (decided 2026-09-28). The crate pins an
    "internal" Wasmtime crate at an exact version, so depending on it could break us at any
    Cranelift release; Cranelift's own dependency on it stays confined to the backend. Re-implemented
    (not copied — its licence is Apache-2.0 WITH LLVM-exception, pinp is MIT): typed ids and
    `IdVec` (an id-indexed `Vec` that creates the ids); `SideTable` (per-id data with a default); `Option<Id>` via a `NonZeroU32` niche instead of
    `PackedOption`; a list pool following Cranelift's documented design — length prefix,
    power-of-two blocks, one free list per size class linked through the freed blocks. One tweak:
    **a single untyped `u32` pool per arena, with typed handles** (`List<NodeId>`, `List<BlockId>`),
    so the `Switch` targets need no second pool. Lists stay contiguous (slices); if long-body
    insertion ever shows in the pipeline timings, the fallback is Cranelift-style `prev`/`next` side
    tables for statements.

A compiled, tested draft of the structures exists (condensed in the appendix); 0017-A step 1 starts
from it.

### Language features the IR must accommodate

The syntax is a work in progress and never reaches the IR (it sits behind sema); the *semantics*
below do. Most features are additive — new `Operation` variants inside the 16-byte budget. The
ones marked **skeleton** change the IR's structure, so they are decided before they are built.

| Feature | Decision | IR impact |
|---|---|---|
| `return` / `break` / `continue` | planned | structured statements that leave an enclosing body; `flatten` makes them edges |
| recursion | planned | none in the IR; interprocedural summaries over strongly connected components |
| closures | planned; **captures are immutable** | `MakeClosure { function, captures }`, an indirect call, function types |
| algebraic data types + `match` | planned | see below — additive, but the largest item |
| error handling | **no exceptions**; `Result` and `Option` (spelled `T?`, implicit wrap, `x or default`, `x or return …` / `x or fail "…"`, narrowing after `x != none` — done in sema); runtime errors stay aborts (as Rust's panics) | none beyond ADTs and early `return`; all the sugar desugars to an `If` on the tag |
| generics | **open** — `Option`/`Result` need at least built-in generic ADTs | **skeleton**: monomorphisation (one `FunctionId` per instantiation) vs shared code |
| generators / coroutines | **planned** | **skeleton**: a function becomes a state machine |

**ADTs in the IR.** Sum types are tagged unions: operations to construct a variant, read its tag,
and read a field. `match` is compiled into a decision tree by `desugar` (exhaustiveness is sema's
job), producing a structured multi-way branch on the tag; the flat level gains a `Switch`
terminator (`{ tag, targets: List<BlockId>, default }` — 12 bytes, in the arena's single list
pool). At the structured level each `match` arm is an `Arm { variant, body }` node, so arms carry
their own spans. Cranelift has no aggregates: a value wider than a
register lives in a stack slot or on the heap, so the flat level reads and writes it as `Load`/`Store`
at an offset. Recursive ADTs (lists, trees) are heap-allocated, and 0017-F's ownership analysis must
extend from `str` to any ADT that owns heap data.

## Parts of milestone 0017 (a sketch)

Each part is red → green → refactor with a human review at the end of the part. Letters are
placeholders for order; parts will be merged, split, and reordered.

**Priority (2026-09-29): AST → IR → Cranelift JIT → output first.** After 0017-A, `flatten`
(0017-B; no lowering passes before Cranelift, as it turned out) comes next, then the Cranelift backend (0017-C); porting the
analyses onto the flat level (0017-D) follows once programs run through Cranelift. Known
issues found along the way (KI-0004 to KI-0007) are fixed on their own branches after that.
Strings leak on the Cranelift path until 0017-F places their frees (decided 2026-09-30): the
first goal is correct output; `tests/string_leaks.rs` runs against the LLVM backend only
until then.

**Next stage after 0017-C, the Cranelift backend (decided 2026-09-30): pipeline timing**, on its
own branch. Every compile records the time of each stage — parse, sema, static analyses (when
requested), desugar, each lowering and optimisation pass, `flatten`, emitting Cranelift IR,
Cranelift compilation (total and slowest function), JIT link — with the program's own run time
kept separate. Exposed through `PinpJit` and a `--timings` flag of the planned runner. The
~100 ms latency for typical code is a target these timings let us watch, not a test that fails
the build. One `Instant` per stage, no per-node instrumentation.

### 0017-A — IR foundation and desugaring (structured level)

No behaviour change: `PinpJit` still compiles from the AST.

1. **`src/ir/`** — ids, `ProgramTables`, `Operation`, `Arena`, shapes, verifier. Adds `Hash` to
   `PinpType`, `ArrayElementType`, `StrId`; the containers of design decision 10. Ready for ADTs
   from the start: a `Switch` terminator (`{ tag, targets: List<BlockId>, default }`) and an offset
   on `Load`/`Store`.
   *Tests:* size assertions; interning (NaN, `-0.0`); verifier rejects a shared node, a value in
   statement position, a structured operation in a flat function, `and`/`or` as `Binary`, a node
   with no span (`Unspanned`).
2. **IR printer** — a readable text form, one line per node, used by every later golden test.
3. **`desugar`** — AST → `Function<Structured>`:
   - names → `LocalId`/`GlobalId`; implicit promotions → `Convert`;
   - `div`/`mod` → `IntegerDivide { checked: true }`; `and`/`or` → `If`;
   - if-expression → temporary + `If`; multi-assignment → temporaries (swap semantics);
   - `x[i] op= e` → element get, op, set with the array and index evaluated once;
   - `for` over range/array/matrix → `CountedLoop`, desugaring-generated accesses `checked: false`;
   - comprehension → `ArrayAllocate` + `CountedLoop` + `ElementSet`;
   - f-strings → `Convert` to `str` + concatenation; `identity`/`str`/`meminfo`; `.len` & co. →
     constants (sizes are static).
   - efficiency (deferred 2026-09-30 to the pipeline-timing stage, to be driven by measurement):
     presizing the arena from parser-recorded counts, and building lists through one shared scratch
     stack.
   *Tests:* golden printer output per construct, adversarial nesting (if-expression inside a
   `while` condition → `header` list; comprehension inside a comprehension; swap).
4. **Corpus check** — `tests/common` desugars and verifies every program it evaluates, so the whole
   e2e suite becomes an IR test at no extra authoring cost.

### 0017-B — Flat level

1. **`flatten`** (revised 2026-09-30, [specs/0017-B](../specs/0017-B-flat-level.md) entry 1): the only
   pass before Cranelift. It turns `If`/`While`/`DoWhile`/`CountedLoop` into blocks and branches
   over the same arena. Checks stay in expressions (`checked` flags, the zero-step rule); the
   Cranelift emitter splits blocks at them, so nothing is reordered. Every value operation is legal
   at the flat level, the backend translating it; a counted loop reads its range through
   `RangeField`. Lowering passes (explicit addresses, loads, stores) and the editing API come
   later, with the first optimisation that needs them.
   *Tests:* golden output per construct; the corpus flattens and re-verifies every function.

### 0017-C — Cranelift backend

1. Move today's codegen to `src/codegen/llvm/` unchanged; it becomes the oracle.
2. **Fix integer division first** (KI-0004): today `7 div a` with `a = 0` returns 0
   (LLVM exploits undefined behaviour) and `i64::MIN div -1` kills the process with SIGFPE. Cranelift
   traps on both. Rust's approach (decided): both are runtime errors, for `div` and `mod` alike —
   "Division by zero." and "Division overflow." — emitted by the Cranelift emitter for `checked`
   divisions (the LLVM side stays KI-0004).
3. **`src/codegen/cranelift/`** from `Function<Flat>`: locals → frontend `Variable`s, runtime
   functions resolved by symbol, the uniform `__pinp_main(out)` entry, the `pinp_run` trampoline.
   `opt_level = speed`.
4. `PinpJit` switches to Cranelift. **Differential mode**: the e2e corpus runs on both backends and
   must agree on every value and every runtime error message.
5. **Check the release binary size** with the `llvm` feature off against the 10 MB target —
   a measurement to watch, like latency, not a test that fails the build.
6. `llvm` stops being a default feature.

### 0017-D — Analyses move onto the flat level

1. **Dataflow over `Function<Flat>`** — `solve` unchanged in shape; blocks hold statement
   `NodeId`s, so the `'ast` lifetime disappears. Facts over dense `LocalId`s become bitsets
   (retires 0015 compromise 3). Adds the statement-level replay (`replay_forward`/`replay_backward`).
   Flow analyses (liveness, reachability, dead stores, constants, ownership) run here; analyses
   that need loop structure (bounds, trip counts, loop optimisations) run on the structured level.
   With this item, `flatten` starts recording a **structure map** — per loop its header, body, and
   exit blocks and its original range and index; per block the construct it came from — so flat
   checkers report at the right construct without rediscovering loops.
2. **Checkers ported** — reachability and liveness on the flat level, reported through
   `arena.spans`. The if-expression compromise is lifted, so findings can only become *more*
   precise; each newly reported case gets a test and is listed in the spec (open question 2).
   `escaping_symbols` goes (globals are a different id type).
3. **Retire `src/cfg/`.**

### 0017-E — Optimisation framework and first passes (moved out: future work)

*Moved out of milestone 0017 on 2026-10-27: optimisations are future work, an iteration of their
own. The letter stays vacant so references to F and G keep their meaning. The sketch stands:*

*Two preconditions, from the 0017 review (2026-11-09):*

- *One pipeline.* Compiling (`codegen::cranelift::compile`) and analysing (`analysis::check`)
  each desugar and flatten on their own. Before passes edit the program, a shared driver builds
  `Program<Flat>` once and hands the same program to analysis, optimisation, and emission.
- *Source use and generated use.* Liveness skips `str_free` arguments, and a counted loop's own
  test and step read its index, so a binder the source never reads looks read. For diagnostics
  that is deliberate; facts that drive optimisation — removing stores, eliding copies — must keep
  the two kinds of use apart explicitly.

1. **Editing API** on `Function` (its first part arrives with the lowering passes):
   `replace_operation`, `remove_statement`, `set_terminator`, `insert_before`, applied directly
   (no edit batching — decided 2026-09-27).
2. **Effect properties** — `may_trap`, `has_effects`, `Arena::is_removable`: nothing that can raise a
   runtime error or has an effect is deleted or reordered.
3. **Pipeline** — structured passes, `flatten`, checks (on request), then flat passes in rounds
   (capped) until nothing changes, verifier after each pass in debug builds. No analysis caching:
   each pass re-solves, so there is no invalidation to get wrong.
4. **Passes:**
   - `eliminate_bounds_checks` (structured) — interval evaluation of the index under enclosing
     `CountedLoop`s with constant bounds; loop variables are read-only (sema), array sizes static.
     Handles `i`, `i ± c`, `i mod c`, and the negative-index `select` desugaring emits (a proven
     sign picks one side).
   - `fold_constants` (flat) — per-local lattice `{Nothing, Exactly(literal), Varies}`, branch
     folding, removal of division and zero-step checks proven unnecessary. One `fold_binary`
     defines pinp's arithmetic at compile time and returns `None` for anything that would trap.
   - `remove_unreachable`, `eliminate_dead_stores` (flat, locals only, gated by `is_removable`).
   - The always-true/always-false condition warnings deferred by 0015, as a checker over the same
     constant facts.
   - An **always-out-of-bounds index** checker over the same facts: an index whose value is known
     (`i = 0 - 20` then `a[i]`) is wrapped and checked at compile time, exactly as sema does for a
     literal, and reported as an error.
5. **Rule:** structured passes never add or remove a user-visible read or write, so findings are
   identical with optimisation on and off (tested).

### 0017-F — Ownership as a transform

*Done differently (2026-10-27, [specs/0017-F](../specs/0017-F-ownership.md)): string copies and
frees are IR operations placed by desugaring, freed at function exit; the scope markers and the
copy-eliding pass below wait for the optimisation work.*


Desugaring first gains **scope markers**: at the end of every scope, a statement naming the locals
that die there (Rust MIR's `StorageDead`), which `flatten` carries along as ordinary statements.
Then `place_string_frees`: a forward analysis over string locals (`Owned`/`Borrowed`/`Moved`/`Freed`)
that inserts frees and removes copies whose source is dead afterwards. Retires codegen's
`owns_str_result`/`free_scope_strings` and 0014's copy-on-borrow. `tests/string_leaks.rs` is the
regression suite.

### 0017-G — Retire LLVM from the JIT

*Done 2026-10-27 ([specs/0017-G](../specs/0017-G-retire-llvm.md)).*


Delete `src/codegen/llvm/` and the LLVM dependency (decided: the AOT tier, if built, starts over
from the flat level as textual `.ll`), and update CLAUDE.md's pass-naming table and the memory
of the backend plan. (README.md already describes Cranelift, since 0017-C.)

### Later

- Element-wise array/matrix arithmetic and its fusion pass (`fuse_element_wise`, structured).
- Static-size specialisation of small matrices.
- **Interprocedural summaries** — a `FunctionSummary` per function in `ProgramTables`: globals
  read/written (bitsets over `GlobalId`), `may_trap`, `has_effects`; later returned-`str` ownership
  (0017-F) and return intervals. Computed callees first over the call graph's strongly connected
  components, iterated to a fixpoint inside a recursive group, from desugared (unoptimised) IR. A
  call through a closure value takes the union of all closures with a matching signature. Relies
  on closures capturing immutably. Adopted alongside, all linear: **not compiling functions
  unreachable from the entry** (saves ~40–60 µs of JIT per function), **parameter escape flags**
  (array freeing, 0017-F), and **inlining decided by pinp, performed by Cranelift's inliner**, with
  total code growth capped. Constant arguments across calls is a candidate; value ranges across
  calls, cloning and alias analysis wait for evidence; closure-flow and context-sensitive analysis
  are rejected. The full menu, costs and reasoning: [ipo-and-backend-memory.md](ipo-and-backend-memory.md).
- The AOT tier: textual `.ll` emitted from the flat level, compiled by `clang`, as a separate tool.

## Testing strategy

| Layer | What | Where |
|---|---|---|
| IR invariants | sizes, interning, verifier rejections | `src/ir/tests.rs` |
| Each lowering | golden printer output per construct, adversarial nesting | `src/desugar/tests.rs`, `src/flatten/tests.rs` |
| Corpus | every e2e program desugars, flattens, and verifies | `tests/common/mod.rs` hook |
| Analyses | fact-level tests on hand-built IR; findings e2e | in-module; `tests/analysis.rs`, `tests/diagnostics.rs` |
| Backends | every e2e program on Cranelift and LLVM, identical results (`--features llvm`) | `tests/common` differential mode |
| Each optimisation | IR before/after (golden) plus adversarial cases: a trapping dead store survives, a loop-carried store is live, NaN/`-0.0`/overflow in folding, no folding of `x div 0` or `i64::MIN div -1`, inclusive/exclusive/descending/empty ranges and negative indices in bounds-check elimination | `src/optimize/*` |
| Optimiser as a whole | every e2e program optimised vs unoptimised, identical results and errors; findings identical | `tests/common` |
| Folder = runtime | random operands through `fold_binary` vs JIT results | `src/optimize/constants.rs` |

## Risks

| Risk | Mitigation |
|---|---|
| Behaviour drift between backends (float formatting, `^`, NaN comparisons) | differential corpus in 0017-C before the switch |
| Diagnostics pointing at compiler-made code | origin spans on every node; unnamed locals never reported |
| A list used with the wrong pool, or after its arena is cleared | lists never leave `Arena`; access only through its methods |
| A pass turns a tree into a DAG | verifier after every pass in debug builds |
| Latency or size creeping up as features accumulate | pipeline timings show latency per stage; the binary size is checked against its target |
| Cranelift upgrades breaking pinp (exact-pinned internal crates) | the IR has its own containers; upgrades touch only `src/codegen/cranelift/` |
| Iterations too large to review | each step is a review point; split further when a step grows |

## Open questions for the human

Resolved 2026-09-27:

1. **Pass verbs** — *desugar* (AST → structured), *flatten* (structured → flat), *emit* (`emit_*`,
   flat → Cranelift IR); optimisations named by effect (`fold_`, `eliminate_`, `remove_`, `place_`,
   `fuse_`). In CLAUDE.md's table since 0017-C; desugar and flatten use no method prefix.
2. **0017-D findings** — strictly more precise warnings are accepted; each newly reported case gets a
   test and is listed in the spec.
3. **LLVM after 0017-G** — deleted.
4. **Containers** (2026-09-28) — our own (design decision 10), named `IdVec` (owns items, creates
   their ids), `SideTable` (per-id data with a default), `List<T>` + `ListPool`, and plain
   `Option<Id>`.
5. **Division** — Rust's approach: "Division by zero." and "Division overflow." runtime errors for
   `div` and `mod`.
6. **0017-A exposure** — tests only (the corpus hook covers every e2e program).
8. **Generators / coroutines** — planned.

Deferred to the future language iterations that introduce them — not decided here, but the IR
must stay able to accommodate any of the listed answers:

7. **Generics** — monomorphised (one `FunctionId` per instantiation) or shared code?
   `Option`/`Result` need at least built-in generic ADTs.
9. **Result error propagation** — bare `x or return` propagating the same error, plus an `err`
   keyword naming it on the right of `or`?
10. **Shared mutable state for closures** — nothing (one-element array), a `cell` type, or a
    declaration-site marker on the variable?
11. **Array parameters** — mutable by reference, copied, or read-only unless declared mutable?

## Appendix — the draft structures (condensed)

```rust
pub struct NodeId(u32);   pub struct LocalId(u32);    pub struct GlobalId(u32);
pub struct FunctionId(u32); pub struct TypeId(u32);   pub struct ConstantId(u32);
pub struct BlockId(u32);  // (draft names; containers now pinp's own — design decision 10)

pub enum Constant { Bool(bool), Int(i64), Float(u64 /* bits */), Str(StrId) }

pub struct ProgramTables {
    pub types: TypeTable,            // PinpType ⇄ TypeId
    pub constants: ConstantPool,     // Constant ⇄ ConstantId
    pub globals: IdVec<GlobalId, Global>,
    pub signatures: IdVec<FunctionId, Signature>,
}

pub enum Operation {
    // both levels
    Constant(ConstantId), ReadLocal(LocalId), ReadGlobal(GlobalId),
    WriteLocal { local_id: LocalId, value: NodeId },
    WriteGlobal { global_id: GlobalId, value: NodeId },
    Evaluate(NodeId),
    Unary { op: UnOp, operand: NodeId },
    Binary { op: BinOp, lhs: NodeId, rhs: NodeId },          // never And/Or/IntDiv/Mod
    IntegerDivide { remainder: bool, checked: bool, lhs: NodeId, rhs: NodeId },
    Convert { operand: NodeId },
    Call { callee: FunctionId, args: List<NodeId> },
    // structured only
    Range { kind: RangeKind, start: NodeId, stop: NodeId, step: Option<NodeId> },
    Membership { value: NodeId, range: NodeId },
    ArrayNew { elements: List<NodeId> },
    ArrayAllocate { zeroed: bool },
    ElementGet { array: NodeId, indices: List<NodeId>, checked: bool },
    ElementSet { array: NodeId, indices: List<NodeId>, value: NodeId, checked: bool },
    Slice { source: NodeId, selectors: List<NodeId> },
    If { cond: NodeId, then_body: List<NodeId>, else_body: List<NodeId> },
    While { header: List<NodeId>, cond: NodeId, body: List<NodeId> },
    DoWhile { body: List<NodeId>, cond: NodeId, until: bool },
    CountedLoop { index: LocalId, range: NodeId, body: List<NodeId> },
    // flat only
    RuntimeCall { function: RuntimeFunction, args: List<NodeId> },
    ElementAddress { array: NodeId, index: NodeId },
    Load { address: NodeId, stored: ScalarKind },
    Store { address: NodeId, value: NodeId, stored: ScalarKind },
}
const _: () = assert!(size_of::<Operation>() == 16);

pub struct Arena {
    pub operations: IdVec<NodeId, Operation>,
    pub types: SideTable<NodeId, TypeId>,
    pub spans: SideTable<NodeId, Span>,
    pub locals: IdVec<LocalId, Local>,     // Local { name: Option<SymId>, type_id }
    pub lists: ListPool,                    // one untyped u32 pool; typed `List<T>` handles
}

pub enum Terminator {
    Goto(BlockId),
    Branch { cond: NodeId, then_block: BlockId, else_block: BlockId },
    Return(Option<NodeId>),
    RuntimeError(ConstantId),
}
pub struct BasicBlock { pub statements: List<NodeId>, pub terminator: Terminator }

pub struct Structured { pub body: List<NodeId>, pub result: Option<NodeId> }
pub struct Flat { pub blocks: IdVec<BlockId, BasicBlock>, pub entry: BlockId }
pub struct Function<S: Shape> { pub id: FunctionId, pub parameter_count: u32, pub arena: Arena, pub shape: S }

pub fn verify<S: Shape>(function: &Function<S>) -> Result<(), VerifyError>;  // level, position, tree
```

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
