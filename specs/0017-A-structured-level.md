[Human+AI]

# The structured level — notebook

Part A of milestone [0017](0017-ir-rework.md): the IR data structures, the printer, desugaring
(AST → `Function<Structured>`), and the corpus check. A running notebook — entries appended, a
later entry overriding an earlier one; the milestone's ground rules are in its overview.

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
tests-only in part A; division errors follow Rust; LLVM is deleted in 0017-G. Language-level decisions
that shape the IR are in the plan's "Language features the IR must accommodate" table.

**Status:** Entry 1's tests need the changes above before implementation.

---

## Entry 3 — own containers instead of `cranelift-entity` (2026-09-28)

Overrides the original working assumption and Entry 1's new dependency. `cranelift-entity` 0.136 pins
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

---

## Entry 5 — step 1 implemented (2026-09-28)

`src/ir/` exists and is wired in as `pinp::ir`; nothing outside the tests uses it yet.

| File | Holds |
|---|---|
| `mod.rs` | module overview, the typed ids, `Constant`, `TypeTable`/`ConstantPool`/`ProgramTables`, `Local` |
| `containers.rs` | `typed_id!`, `TypedId`, `IdVec`, `SideTable`, `List`/`ListPool` |
| `operation.rs` | `Operation` (16 bytes, statically asserted), `Level`, `Position`, `ScalarKind`, `RuntimeFunction`, per-node properties |
| `arena.rs` | `Arena`; the child walk (with positions), tree walk, read walk, `is_removable` — all iterative |
| `function.rs` | `Function`, `Shape`, `Structured`, `Flat`, `BasicBlock`, `Terminator` (16 bytes), successors and predecessors |
| `verify.rs` | `VerifyError`, `verify` |

Found while implementing:

- A `Switch`'s targets live in the arena's list pool, so successors and predecessors need the
  arena: they moved from `Flat` to `Function<Flat>` (tests adjusted).
- `ListPool` also **shrinks** a list whose length drops into a smaller size class, as Cranelift's
  does. Otherwise a block's size would no longer follow from its list's length, and a block could
  be freed under the wrong size class.
- `ListPool::items` returns a slice of typed ids from the pool's raw `u32`s. That is one `unsafe`
  cast; `TypedId` is an `unsafe trait` whose contract (`repr(transparent)` over `NonZeroU32`)
  `typed_id!` guarantees.
- `RuntimeCall` counts as `may_trap`: a runtime function may raise a runtime error.
- `ProgramTables` holds only `types` and `constants`; globals and signatures arrive with
  `desugar` (step 3), which is the first thing to need them.
- `Hash` derived on `PinpType`, `ArrayElementType`, `StrId`, as planned; no new dependency.

**Status:** 135 IR tests (99 in `tests.rs`, 36 in `containers.rs`) pass; full suite, clippy
(`--features llvm`) and rustfmt clean.

---

## Entry 6 — step 2, the printer: tests (2026-09-28)

A readable text form, **one line per node**, children indented under their parent (WHIRL's dump
style), for golden tests and for investigating verifier failures. Not parseable — a parser was
deferred (Entry 2).

`Printer::new(&tables, names, strings)` takes the program tables plus the AST's symbol names and
string literals; `.function(&function)` prints a whole function, `.tree(&arena, root)` one tree.

Format: a header (`function f0 (1 parameter), structured`); a `locals` section (`l1 total: float`,
a temporary as `l4: int`); then `body`/`result`, or one section per block (`block0 (entry)`) ending
in its terminator. Operations print as snake_case words with their flags and targets
(`element_get checked`, `load word +8`, `switch block2 block1 default block3`), values end in
`: type` (never `: void`). Nested bodies are labelled (`then`, `else`, `header`, `body`); an empty
`else` or `header` is omitted. Broken IR prints rather than panics: `<dangling n999>`,
`<shared n3>` for a node's second appearance (so a cycle terminates), `?` for a missing name,
local, or type.

Tests added to `src/ir/tests.rs` (17); fixtures now also return their `ProgramTables`. Awaiting
review before implementation.

---

## Entry 7 — step 2 implemented (2026-09-28)

`src/ir/print.rs`: `Printer` plus `PrintableShape`, which each shape implements to lay out its
sections (`body`/`result`, or one section per block). Printing is iterative, like every other
walk. The format is Entry 6's, unchanged. One test-only fix: the table of cases in
`flags_kinds_and_targets_print_after_the_operation_name` had the wrong type annotation.

**Status:** 152 IR tests pass; full suite, clippy (`--features llvm`) and rustfmt clean.

---

## Entry 8 — step 3 split; 3a (core desugaring): tests (2026-09-28)

Step 3 covers the whole language, too much for one review. It is split:

| Sub-step | Scope |
|---|---|
| **3a** | program structure (functions, entry, globals, signatures, scopes); literals; operators and promotions; `and`/`or`; calls; `if`; assignment (single, multiple, chained); `while`; `loop` |
| 3b | ranges, `in`, `for` over a range |
| 3c | arrays: literals, indexing, indexed assignment (plain and compound), members, comprehensions, `for` over arrays |
| 3d | matrices |
| 3e | strings, f-strings, the built-ins (`str`, `identity`, `meminfo`) |

Presizing the arena moves to step 4, where the corpus measures the expansion factor it needs.

**3a design**, as pinned by the tests (`src/desugar/tests.rs`, 29; plus 2 printer tests):

- **`Program`** (in `src/ir`): `ProgramTables` (now also `globals` and `signatures`), the
  functions in definition order with the **entry** — the top-level statements — last. A signature
  records its name (none for the entry), parameter and result types; a global its name and type,
  taken from its first top-level assignment, as codegen does. The printer names globals and
  functions when the tables know them (`read_global g0 total`, `call f0 double`).
- **Scopes** mirror sema: parameters and locals resolve outward to the function's base; at the top
  level the base is the global frame, so a name first assigned inside a top-level block is a local
  of the entry, not a global. (Codegen differs here — it binds such a name to a same-named global
  declared later — but the difference cannot be observed.)
- **Promotions** become explicit `Convert`s: arithmetic operands to the result type; `/` both to
  `float`; `div`/`mod` both to `int`; comparisons to `float` if either side is, otherwise to `int`
  (so booleans compare as 0 and 1); arguments to parameter types; a result to the return type; an
  assigned value to the target's type; `if` branches to their joined type. An `int ^ int` stays an
  `int` `power` (the backend computes it as today).
- **Statements hoisted out of expressions.** `and`/`or` become a temporary plus an `If`; an
  if-expression a temporary written in every branch; `elif` nests in the `else` body. Such
  statements run before the statement containing the expression — so an operand to their **left**1
  is first saved in a temporary (constants excepted), keeping left-to-right evaluation. A `while`
  condition's statements go in its `header`; a `loop` condition's at the end of its body.
- **Assignment.** Several values or several target groups go through temporaries (swap and chain
  semantics). A program ending in an assignment evaluates to the value assigned, *before*
  conversion to the target's type — through a temporary, as codegen does.
- A void function's final expression, and an if-expression used as a statement, are evaluated for
  effect (`evaluate`, or an `If` of `evaluate`s).
- **Spans:** a node takes its expression's span; a write its target's; a `Convert` its operand's;
  everything made for `and`/`or`/`if` the whole construct's.

Awaiting review before implementation.
1
---

## Entry 9 — 3a implemented (2026-09-28)

`src/desugar/`: `mod.rs` (the driver — signatures, globals, one function at a time, the entry
last — and the building blocks: nodes, temporaries, conversions, scopes), `expr.rs`
(expressions, operand ordering, `and`/`or`), `stmt.rs` (statements, assignment, `if` chains,
bodies). `Program`, `Global`, `Signature` join `src/ir`; the printer names globals and functions.
Constructs of later sub-steps stop with `todo!` naming the sub-step that brings them.

Fixes to the tests while implementing — all mistakes in the tests, not changes of design:
`result_of` now unindents the section it returns; two `contains` checks whose snippets lost their
indentation to `indoc!` became full golden comparisons; and the void-function test used a program
sema rejects (a void function's last expression must itself be void), so it now chains void calls.

**Status:** 29 desugar tests and 154 IR tests pass; full suite, clippy (`--features llvm`) and
rustfmt clean.

---

## Entry 10 — 3b (ranges, `in`, `for` over a range): tests (2026-09-28)

Seven tests in `src/desugar/tests.rs`. Design:

- A range stays one `Range` node: its kind, and a step **only when written** — so only an
  explicit step can trap on zero, and the implicit ±1 (direction taken from the bounds for `..`)
  is decided where the range is built, as today. Boolean bounds and a boolean `in` operand convert
  to `int`.
- A range is an ordinary value: it can be stored, and read again by a `for` or an `in`.
- `for i in range` becomes `CountedLoop` over the range node, spanned by the range expression. The
  index is a named local created after the range is desugared (the range is evaluated outside the
  loop's scope), read-only because sema forbids assigning it.
- Range bounds are operands like any other: one left of hoisted code is saved first.

Awaiting review before implementation.

---

## Entry 11 — 3b implemented (2026-09-28)

Ranges and membership in `expr.rs` (bounds and step through `operands`, each converted to `int`);
`for` over a range in `stmt.rs`. A `for` body's trailing expression, if any, is evaluated for
effect, like any other body's. `for` over an array or matrix still stops with `todo!` (3c).

**Status:** 36 desugar tests pass; full suite, clippy (`--features llvm`) and rustfmt clean.

---

## Entry 12 — 3c (arrays): tests (2026-09-28)

16 tests in `src/desugar/tests.rs`, plus one IR test. **One IR change:** `ElementGet` and
`ElementSet` gain a `wraps` flag — a negative index counts from the end. Normalising a negative
index is part of what the index *means*, not part of the bounds check, so it cannot ride on
`checked`: an analysis that proves an access in range and clears `checked` must not thereby make
`a[-1]` wrong. The printer shows it after `checked` (`element_get checked wraps`); it never traps.

Design:

- **User-written index:** `checked` and `wraps`. **Literal index:** sema already proved it in
  range, so neither — and a negative literal is counted from the end here, once (`a[-1]` on three
  elements reads element 2). **Compiler-made index** (loops, fills): neither.
- `a[i] = v`: one `ElementSet`, the value converted to the element type. The array and index are
  operands like any other: saved first if the value needs hoisted statements.
- `a[i] op= v`: array and index each evaluated once into temporaries; the **get** is checked, the
  **set** reuses the same index unchecked; the get is read before any statement the right-hand side
  hoists (as codegen loads the element before evaluating it).
- `a[r] = v` (slice fill): `v` converted and evaluated once, then a `CountedLoop` over the range
  storing it — unchecked, since sema checks slice bounds.
- Members (`.len`, `.ndim`) are constants; the object is not evaluated, as today.
- `for v in a` / `for i, v in a`: the array evaluated once into a temporary, a `CountedLoop` over
  `0..<len` (the index a temporary, or the named index binder), the element read into `v` first
  in the body. Everything made for it is spanned by the array expression.
- `[a..b:s]` and comprehensions: `ArrayAllocate`, a position counter, a `CountedLoop` over the range
  storing each element and advancing the position. A comprehension variable *is* the loop index,
  unless declared `: float`, in which case it is a float local set to the converted index. That
  equals codegen's repeated float additions while the values stay exact in a float (magnitude up
  to 2⁵³); beyond that the IR rounds once where codegen rounds at every step, so it is the more
  accurate of the two.

Known, accepted difference: codegen checks an assignment's index *before* evaluating the value;
the IR evaluates all operands first. Only a program that both traps and has an observable effect
in that value could tell.

The IR test uses the new field, so `cargo test` does not compile until 3c is implemented.
Awaiting review before implementation.

---

## Entry 13 — negative indices desugared; `wraps` withdrawn (2026-09-28)

Supersedes the `wraps` flag of Entry 12. The human's rule, unchanged from today's pinp: a negative
index counts from the end, as in Python, and **every** access is then bounds-checked —
`foo = [1, 2, 3, 4, 5, 6]`: `foo[-2]` is 4, `foo[-20]` is out of bounds. A literal index is wrapped
and checked by sema (a compile error when out of range); a variable one at run time; and one whose
value analyses can know (`i = 0 - 20`) should be caught at compile time — added to the plan as a
0017-E checker.

The wrap is desugared at the earliest IR stage instead of being a flag: `a[i]` becomes
`a[i < 0 ? i + len : i]`, with the access checking the result. That needs a value-level choice, so
the IR gains **`Select { cond, if_true, if_false }`** — C++'s `?:`, Cranelift's `select`. It is pure,
computes both sides, and exists at both levels; interval analysis evaluates it directly (a proven
sign picks one side). An index that is not a plain read is first computed into a temporary so it
runs once. Literal indices (resolved by desugaring) and compiler-made ones (never negative) have no
`select`. An element access in the IR is therefore always a plain 0-based index.

Tests updated: `wraps` gone from `src/ir/tests.rs`, `Select` added to the level, trap, and child
tables plus one test; the four desugar tests with variable indices now show the `select`, and one
new test covers a computed index. Awaiting review before implementation.

---

## Entry 14 — 3c implemented (2026-09-29)

`src/desugar/array.rs` holds everything array-specific; `Select` joins `src/ir` (child walk,
printer, both levels, pure). Two refactors made room for it: operand ordering is now `operand` +
`save_earlier`, so an element assignment can protect a mixed list (a place read, an index, a
value); and combining two operands under an operator is `combine`, shared by binary expressions,
compound assignment, and the `select` desugaring emits. A `for` body is built with `body_into`, so
the element read comes first in the same scope. `for` over a matrix stops with `todo!` (3d).

**Status:** 53 desugar and 155 IR tests pass; full suite, clippy (`--features llvm`) and rustfmt
clean.

---

## Entry 15 — 3d (matrices): tests (2026-09-29)

12 tests in `src/desugar/tests.rs`. Design:

- A matrix literal is an `ArrayNew` of its elements row by row, each converted to the element
  type; `identity(n, t)` a zeroed `ArrayAllocate` whose diagonal a `CountedLoop` sets to 1.
- `m[r, c]` is one `ElementGet` with two indices, each wrapped against its own dimension exactly
  as a 1D index is; literal indices are resolved; the access is `checked` unless both are literal.
- Slices keep sema's classification as `Slice` selectors: a scalar index (wrapped like any other),
  or a range — `:` spelled out as `0..<dimension`.
- Element assignment and compound assignment follow the 1D rules, with two indices.
- `for v in m` is a row loop around a column loop reading `m[r, c]` — row-major order, as today;
  `for r, c, v in m` counts with the named binders.
- Members (`.rows`, `.cols`, `.len`, `.ndim`) are constants.

**Differs from the old backend:** the scalar index of a row or column slice (`m[-1, :]`,
`m[:, -1]`) counts from the end in the IR, per the rule of Entry 13; today's codegen does not wrap
it and fails at run time. That is a defect of the old backend, to be fixed on its own branch — not
here — before the 0017-C differential run compares the two.

Awaiting review before implementation.

---

## Entry 16 — 3d implemented (2026-09-29)

The array code in `array.rs` generalised from one index to one per dimension rather than
duplicated: `index` handles element access and slices of arrays and matrices alike (`:` becomes
`0..<dimension`), `element_assignment` takes a list of indices, and `for_over_array` builds one
counted loop per dimension, rows outside columns. A matrix literal is its rows flattened into the
1D literal path; `identity` is a zeroed allocation plus a diagonal loop. Out-of-scope findings were
logged instead of fixed: KI-0004 (division), KI-0005 (matrix slice index), KI-0006 (float
comprehension), each with a `TODO(KI-…)` at its site.

**Status:** 65 desugar tests pass; full suite, clippy (`--features llvm`) and rustfmt clean.

---

## Entry 17 — 3e (strings and built-ins): tests (2026-09-30)

10 tests in `src/desugar/tests.rs`. Decided alongside: **strings leak on the Cranelift path until
0017-F** places their frees; `tests/string_leaks.rs` runs against LLVM only until then (plan,
priority paragraph). Ownership is therefore absent from the IR for now — no copies, no frees.

Design:

- A string literal is a `Constant`; `str(x)` a `Convert` to `str` (a `str` argument is left as it
  is — copying is ownership's business, 0017-F).
- A chain of `+` on strings is **one** `RuntimeCall str_concat` over its parts — the left spine
  flattened, as codegen joins in one allocation today; a scalar part is converted to `str`; a
  parenthesised concatenation on the right is one part with its own join. (`RuntimeCall` is legal
  at the structured level — partial lowering, Entry 2.)
- An f-string is a join of its literal runs and its holes converted to `str`; one literal alone is
  a constant; a lone string hole still joins (it makes a new string, as today); `f''` is the join
  of nothing — the empty string, needing no constant.
- String comparisons stay `Binary` with `str` operands; the lowering pass (0017-B) turns them into
  runtime calls.
- `.len` of a string is `RuntimeCall str_length`; `meminfo()` is `RuntimeCall meminfo` (a new
  `RuntimeFunction`), evaluated for effect.

Awaiting review before implementation.

---

## Entry 18 — 3e implemented; desugaring complete (2026-09-30)

Strings and built-ins in `expr.rs` (`concatenation` flattens the left spine, `f_string`, one
`join` for both) and `array.rs` (a string's `.len`); `RuntimeFunction::MemInfo` joins the IR. No
`todo!` remains in `src/desugar/`: every construct the language has desugars.

**Status:** 75 desugar tests and the full suite (1,108) pass; clippy (`--features llvm`) and
rustfmt clean.

---

## Entry 19 — step 4: the corpus check; part A complete (2026-09-30)

`tests/common::eval` now parses, analyses, desugars, and verifies every program before running it,
printing the offending function if one fails to verify. 211 end-to-end tests go through it (the
rest are unit tests, or error tests calling `PinpJit::eval` directly); every one verifies. The hook
was proved live by making it panic temporarily.

Arena presizing and the shared scratch stack move to the pipeline-timing stage, where measurements
can justify them (latency first: no unmeasured optimisation).

**Part A is complete:** the IR data structures, the printer, desugaring of the whole language, and
the corpus check. Next, per the priority set on 2026-09-29: the lowering passes and `flatten`
(0017-B), then the Cranelift backend (0017-C).
