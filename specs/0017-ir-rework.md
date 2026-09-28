[Human+AI]

# IR rework — notebook

**This is a major refactor, and this file deliberately deviates from the earlier specs.** It is
not an up-front [Human]/[AI] design document. It is a running notebook: what was done, what was
decided, what was found, in the order it happened. Entries are appended, not rewritten, so an
early entry may be contradicted by a later one — the later one wins.

The direction and the evidence behind it are in [dev-docs/ir-rework-plan.md](../dev-docs/ir-rework-plan.md). That
plan's iteration sequence is a sketch, not a commitment; steps here will be merged, split, and
reordered as the work reveals things, and this notebook says when and why.

In short: pinp gets an intermediate representation between the typed AST and the backends,
modelled on Open64's WHIRL — one node type, several levels, a level being the set of operations
still allowed to appear. The *structured* level keeps loops, `if`s, and whole-array operations
(where pinp's own optimisations will run); the *flat* level is the control-flow graph (what
analyses solve over and backends read). The Cranelift port follows once there is an IR to
translate from.

Project rules are unchanged: tests first and presented before code, adversarial coverage,
in-module unit tests plus `tests/*.rs` end to end, human owns commits.

---

## Working assumptions

Provisional answers to the plan's open questions, so work can start. Each is revisited when it
starts to matter.

- **`cranelift-entity`** provides the containers (`PrimaryMap`, `SecondaryMap`,
  `EntityList`/`ListPool`, `PackedOption`).
- **Pass verbs** *desugar* (AST → structured) and *flatten* (structured → flat) are used as
  working names; CLAUDE.md's table is not updated until they are confirmed.
- **No behaviour change.** `PinpJit` keeps compiling from the AST; the IR is exercised by tests
  only until something consumes it.

---

## Entry 1 — the IR data structures (`src/ir/`)

Starting point: the draft structures sketched and compiled during the WHIRL study (condensed in
the plan's appendix). This step lands them for real.

**Scope:** ids; program-wide tables (`TypeTable`, `ConstantPool`, globals, signatures);
`Operation`; the per-function `Arena`; the `Structured` and `Flat` shapes; `verify`. No
lowering yet — nothing produces IR except tests.

**Changes to existing code:** `Hash` derived on `PinpType`, `ArrayElementType`, `StrId` (interning
needs it). New dependency: `cranelift-entity`.

**Additions over the draft**, found while writing the tests:

- `verify` reports ids that point outside the arena (`DanglingNode`, `DanglingBlock`) instead of
  panicking on an out-of-range index. A pass that mixes up two functions' ids is a compiler bug,
  and a verifier that crashes on it hides where it happened.
- `verify` reports a node with no type (`Untyped`). The `types` side table defaults to the
  reserved id precisely so that this is detectable.
- A self-referencing node is reported as `SharedNode`, which also guarantees `verify` terminates
  on a cycle.
- A `Branch` whose two targets are the same block produces that successor twice and that
  predecessor twice — one entry per edge, matching the existing CFG's "one edge for every way
  execution can pass".

**Tests** (`src/ir/tests.rs`), written first and presented for review before the implementation:

| Area | What is pinned down |
|---|---|
| Layout | `Operation` and `Terminator` are 16 bytes; `EntityList`, `PackedOption`, ids are 4 |
| Type interning | idempotent; every shape/size/element distinction yields a distinct id; round-trip |
| Constant interning | idempotent; NaN equals itself; NaN payloads, `-0.0`/`0.0`, `Int(0)`/`Bool(false)`/`Float(0.0)`, and distinct strings all stay distinct |
| Arena | type and span recorded; untyped and unspanned nodes read as the reserved type and `UNKNOWN`; list round-trip and order; empty lists; `clear` restarts ids |
| Child walk | exact child order for every operation family, optional `Range` step included; leaves have none |
| Read walk | reads inside nested bodies and index lists; globals and write targets are not reads |
| Properties | level membership of every operation; `and`/`or`/`div`/`mod` never a `Binary`; statement vs value; `may_trap` / `has_effects` / `is_removable` including the `checked` flags |
| Verifier | valid structured, flat, and empty functions; shared node (within a statement, across statements, between a statement and a terminator); self-reference; value in statement position (top level and inside nested bodies); statement in value position; structured operation in a flat function; dangling node and block ids; untyped node |
| Flat graph | successors and derived predecessors for goto, branch, back edge, error exit, unreachable block, and a branch with identical targets |

**Status:** tests written; awaiting review before implementation.

---

## Entry 2 — decisions after an MLIR comparison (2026-09-27)

MLIR shares WHIRL's roots; most of what it offers pinp already has in a closed form (levels ≈
dialect legality, bodies ≈ regions, `may_trap`/`has_effects` ≈ effect traits). Taken from it:

- **Partial lowering.** The `Function<Structured|Flat>` typestate governs control flow only;
  `ElementAddress`/`Load`/`Store`/`RuntimeCall` become legal at both levels. Operations are lowered
  by small in-place passes, and `flatten` only builds the CFG. Changes Entry 1's level-membership
  table and the "flat operation in a structured function" rejection.
- **Every node has a span.** `verify` rejects an unspanned node (`Unspanned`), as it rejects an
  untyped one.
- **ADT-ready from step 1:** a `Switch` terminator with a `ListPool<BlockId>`, and an offset on
  `Load`/`Store`.

Deferred: a parser for the printer's text form (MLIR's `mlir-opt` style testing).

Also settled: pass verbs *desugar*/*flatten*/*emit* (working names confirmed); the IR stays
tests-only in 0017; division errors follow Rust; LLVM is deleted in 0022. Language-level decisions
that shape the IR are in the plan's "Language features the IR must accommodate" table.

**Status:** Entry 1's tests need the changes above before implementation.

---

## Entry 3 — own containers instead of `cranelift-entity` (2026-09-28)

Overrides the working assumption and Entry 1's new dependency. `cranelift-entity` 0.136 pins
`wasmtime-internal-core` — self-described as internal — at exactly `=49.0.0`; an IR built on it
could break at any Cranelift release. `src/ir` gets its own containers instead (plan, design
decision 10), re-implementing Cranelift's documented list-pool design rather than copying its code
(Apache-2.0 WITH LLVM-exception vs pinp's MIT).

Decided along the way:

- One untyped `u32` list pool per arena with typed handles — replaces Entry 2's separate
  `ListPool<BlockId>`.
- Lists stay contiguous. Considered and rejected: one combined then/else list with a split index
  (saves ~4 bytes per `If`, complicates every reader, doesn't extend to `match`), block-chained lists
  (no slices), and bodies stored as runs of nodes (would move nodes and so change their ids).
- `match` arms are `Arm { variant, body }` nodes.
- No edit batching: edits apply directly; the pool's spare capacity and free lists absorb them.
- `desugar` presizes the arena from parser-recorded counts and builds lists through one scratch
  stack.

Names: plainer than Cranelift's, for the teaching audience — `IdVec` (was `PrimaryMap`),
`SideTable` (`SecondaryMap`), `List<T>` (`EntityList`), `ListPool`, and plain `Option<Id>`
(`PackedOption`).

---

## Entry 4 — step-1 tests revised (2026-09-28)

Entry 1's tests (`src/ir/tests.rs`) are brought in line with Entries 2 and 3, and the containers
get their own suite (`src/ir/containers.rs`, tests only so far). Awaiting review before
implementation.

| Area | Change |
|---|---|
| Names and ids | `IdVec`/`SideTable`/`List`/`ListPool`; ids built with `from_index`, read with `index()`; optional ids are plain `Option` |
| Types side table | `SideTable<NodeId, Option<TypeId>>` — an untyped node reads `None`; the reserved type id is gone |
| Layout | `List<NodeId>`, `List<BlockId>`, `Option<NodeId>`, `Option<TypeId>` are 4 bytes |
| Arena | `clear` empties the list pool; node and block lists share one pool |
| Levels | `RuntimeCall`/`ElementAddress`/`Load`/`Store` allowed at both levels; the "flat operation in a structured function" rejection becomes a test that lowered operations inside structured control flow verify |
| `Load`/`Store` | carry an `offset` |
| Spans | tests build every node with a real span; `Unspanned` is rejected for an operand, a statement, a node in a nested body, the result, and a terminator operand; a node missing both type and span reports `Untyped` |
| `Switch` | successors (targets in order, then default; no targets; repeated blocks counted per edge), dangling target/default/tag, statement or structured value as tag, tag shared with a statement |
| Containers | ids (size, round trip, index 0 vs `None`, `u32::MAX` given up for the niche, order); `IdVec` (consecutive ids, replace in place, `get`, out-of-range panic, iteration, `clear`, presizing); `SideTable` (default, gap filling, far reads, overwrite, `clear`); `ListPool` (empty list takes no space, every block boundary, id 0 as an item, no overlap, growth beside neighbours, insert/remove incl. panics, block reuse after growth and after emptying, mixed id types in one pool, `clear`, presizing, 2,000 random edits checked against `Vec`s) |
