# Interprocedural optimisation and backend memory — options

Status: **analysis kept for the future** (2026-09-29). Decisions taken from it are in
[ir-rework-plan.md](ir-rework-plan.md); this file keeps the reasoning and the options not taken.

**Priority, set by the human:** minimal latency from parse to execution comes first. pinp pursues
only what is cheap at compile time *and* yields tangible results at run time.

Figures marked *measured* come from the plan's evidence table; the rest are estimates to be checked
against the pipeline timings once Cranelift runs.

## Where IPO happens in other compilers

- **Within a translation unit**, on the IR, before any machine code: LLVM visits groups of mutually
  recursive functions callees-first, inlining and inferring per-function facts ("reads no memory",
  "does not throw") — the same facts as pinp's summaries; GCC's IPA passes inline, clone for
  constant arguments, and propagate constants.
- **Across translation units** a normal `.o` holds machine code, so the linker cannot optimise
  across files beyond dropping unused sections and merging identical functions. **LTO** puts IR in
  the `.o` instead and optimises at link time: *full LTO* merges everything (memory- and
  time-hungry); *ThinLTO* (LLVM) and *WHOPR* (GCC) keep a per-module **summary**, decide imports
  and inlining from summaries alone, then optimise modules in parallel. Open64 did the same: a
  local summary phase, then whole-program IPA.
- **pinp** has no `.o` barrier: the JIT sees the whole program, so it gets LTO's scope for free.
  Its design — per-function summaries, callees first, inlining decided by pinp and carried out by
  Cranelift — is ThinLTO's architecture in one process. The barrier returns only with separately
  compiled modules in an AOT tier.

## Who does what: pinp vs Cranelift

Cranelift compiles one function at a time and does no interprocedural analysis. Recent versions
have an inliner the embedder drives: it asks, per call site, whether to inline and with which body,
then splices it in; its per-function optimiser (e-graph: value numbering, constant folding, loop
hoisting, redundant loads) then sees through the old call boundary.

| Job | pinp | Cranelift |
|---|---|---|
| Call graph, recursive groups | yes | — |
| Summaries (globals read/written, traps, effects, ownership, return facts) | yes | — |
| Using summaries (folding across calls, dead stores, check removal, ownership) | yes | — |
| Not compiling unreachable functions | yes | — |
| Deciding what to inline | yes | — |
| Performing the inlining | — | yes, on request |
| Clean-up after inlining; register allocation | — | yes |

For a call *not* inlined, Cranelift assumes it may write any memory (its alias regions are coarse),
so summaries mainly feed pinp's own passes, before emission.

## The menu of IPO techniques

| Technique | Analysis cost | JIT-time effect | Run-time benefit | Status |
|---|---|---|---|---|
| Summaries: effects, globals, traps | linear | none | medium | **adopted** |
| Not compiling unreachable functions | linear | **saves** (~40–60 µs per function, *measured*) | medium | **adopted** |
| Parameter escape flags (does a function keep or return its array argument) | linear | none | high: array freeing, 0017-F ownership | **adopted** |
| Inlining decisions, total growth capped (e.g. ≤ 1.5×) | linear | grows with inlined code | high | **adopted, capped** |
| Constant arguments across calls | near-linear; needs whole-program IR | none | medium | candidate |
| Value ranges across calls (argument/return intervals) | fixpoint per recursive group, widening | none | medium: cross-call bounds checks | later, if profiles show hot checks |
| Cloning for constant or closure arguments | cheap to decide | grows with clones | high for numeric kernels | later, heuristics only |
| Alias analysis (which arrays may be the same) | Steensgaard ~linear, Andersen ~cubic | none | depends on the array-parameter decision | deferred |
| Closure-flow analysis (0-CFA) | ~cubic | none | modest over signature-type resolution | rejected |
| Context-sensitive analysis | can blow up | none | marginal | rejected |

Scale: a 1,800-line program has at most a few hundred functions; every linear analysis above is well
under a millisecond, against a 100 ms budget of which the Cranelift probe used ~5 ms end to end
(*measured*). The real costs are code growth (paid in Cranelift time) and engineering risk — each
adopted technique is defended by differential tests: every e2e program with and without it,
identical results.

Recursion and closures (planned) keep this viable: summaries are computed per strongly connected
component with a short fixpoint; calls through a closure value take the union of all closures of
the matching signature; closures capture immutably, so a call cannot change the caller's locals.

## Memory

Per node, estimated: pinp IR ~32 B (16 operation + 4 type + 8 span + list share); Cranelift IR
~50–60 B per instruction, with ~1–1.5 instructions per flat node (local reads/writes vanish into
SSA; element addresses and checks expand).

| | 1,800 lines | 100k lines |
|---|---|---|
| AST, alive for the whole compile today | 1.09 MB (*measured*) | ~60 MB |
| pinp IR of the whole program (phase 1 of IPO) | ~0.5 MB | ~28 MB |
| Cranelift, one function at a time (IR + e-graph + regalloc) | ~0.2–1 MB, briefly | same |

- The Cranelift copy is per function and does not multiply with IPO; ours is ~10–20 % of that
  function's transient peak.
- **Drop the AST after desugaring** (keep only the name table and string literals): at ~140 B vs
  ~32 B per node, phase 2 then runs at about half of today's memory even with every function's IR
  alive.
- **Free each function's arena after it is emitted**, and **reuse one Cranelift compile context and
  one builder context** across functions (no reallocation after the first few).
- Fallback if memory ever binds: **desugar twice** — phase 1 keeps only summaries, phase 2
  re-desugars each function before emitting (one extra linear walk; parse + sema is 2.7 ms for
  1,800 lines, *measured*).

## Options considered and rejected

- **Forking Cranelift to run on pinp's IR.** Its optimiser and instruction selectors require SSA
  with Cranelift's data-flow shape — the fork would turn pinp's IR into Cranelift's, not remove it.
  Saving: ~10–20 % of a per-function transient peak. Cost: hundreds of thousands of lines
  (including generated instruction selectors) to maintain, loss of Wasmtime's continuous fuzzing
  and of upstream fixes and architectures. Control needs are met without it: pinp's own passes
  before emission, source locations on instructions, upstream hooks if ever needed, a pinned
  Cranelift version.
- **If control or memory ever does bind: a second tier, not a fork.** A small single-pass baseline
  compiler straight from the flat IR (as Wasmtime's Winch sits beside Cranelift) for instant start,
  with Cranelift recompiling hot functions — where run-time profiles would also drive inlining and
  cloning, as in Julia and V8. Not needed at ~5 ms to JIT.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
