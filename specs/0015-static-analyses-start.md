[Human+AI]

# Static analysis foundations — CFG, dataflow framework, diagnostics

This iteration is an internal rework and does not add any language features.

pinp checks its programs with a recursive, syntax-directed walk: [sema](src/sema/analyzer.rs) carries
a scope stack, infers a type per node, and stops at the first error. That is enough to compile a
correct program and reject an incorrect one, but it cannot answer any question about *how values
flow through a program* — which is where the interesting checks live.

This iteration builds the machinery to be used by those questions and any other checks/analyses.

## Scope of this iteration (0015)

In scope:
- **Source spans** on AST nodes and assignment targets, so a finding can point at source text.
- **Diagnostics**: a structured finding (stable code, severity, span, message, notes) and a pass
  that reports *many* of them per run, rather than fail-fast.
- **Control-flow graph** built per function (and for the top-level entry), with basic blocks and
  explicit terminators.
- **Dataflow framework**: a lattice trait, forward/backward direction, per-statement transfer
  functions, and a worklist solver run to fixpoint.
- **Reachability checker** — unreachable statements (`while false`, a constant-false `if` arm, a
  `for` over an empty literal range).
- **Liveness checker** — dead stores (a value overwritten before it is read) and unused bindings
  (never read at all).
- **SARIF 2.1.0 output** for the collected diagnostics.

Deferred (see the closing section for the full list):
- Ownership/lifetime as a dataflow analysis — the successor iteration, and the one that retires
  step 6 of 0014's copy-on-borrow.
- Constant propagation, and the always-true/always-false condition checks it enables.
- A pinp IR in SSA form (dominator tree, dominance frontiers, φ-placement).
- Anything interprocedural: call graph, cross-function propagation.

## Constraints

- **No behaviour change.** The analyses are read-only over a typed AST and emit diagnostics; they
  perform no transformation and codegen does not consult them. The existing 767 tests must stay
  green untouched.
- **Errors stay fail-fast.** The [fail-fast rule](0004-initial-sema.md) for the lexer, parser, and
  sema is unchanged: the first *error* still stops the pass. Batch reporting applies to the new
  analysis layer, whose findings are *warnings* produced after sema has succeeded.
- The analyses run on demand (a `check` entry point), not as part of `PinpJit::new`.
- **One day.** Compromises are expected and are listed explicitly below rather than discovered later.

## Naming: "lowering" is now ambiguous

`src/codegen/lower.rs` teaches *lowering* as "rewriting a program into a lower-level form", and until
now that meant exactly one thing: AST → LLVM IR. This iteration adds AST → CFG, which is equally a
lowering, so the bare word stops identifying anything. The convention — recorded in
[CLAUDE.md](CLAUDE.md) so it outlives this spec — is that **a pass is named by what it produces**:

| pass | verb | prefix |
|---|---|---|
| AST → LLVM IR | *generate* / "lower **to LLVM IR**" | `gen_*` (`src/codegen/`) |
| AST → control-flow graph | *build* / "CFG **construction**" | `build_*` (`src/analysis/cfg.rs`) |
| CFG × analysis → facts | *solve* | `solve` (`src/analysis/dataflow.rs`) |
| facts → diagnostics | *check* | `check_*` (`src/analysis/checks/`) |

Nothing in `src/codegen/` is renamed: `codegen::lower` is already qualified by its module path, and
its `gen_*` methods were never ambiguous. Only the prose obligation is new — "lowering" in a comment
now names its target.

## Compromises for a one-day iteration

Taken deliberately, each recorded where it applies:

1. **Spans only where a finding points.** Populating `spans` for *every* node means touching every
   `ast.push` call site. Instead the arena is added in full and filled for the node kinds the
   checkers actually report on — `Node::Var`, `Node::Global`, and assignment targets — with
   `Span::UNKNOWN` elsewhere. Later work fills the rest incrementally with no redesign.
2. **No constant folding in reachability.** Only a literal `true`/`false` condition and an empty
   *literal* range prune an edge. `while 1 > 2` is not folded; that arrives with constant
   propagation, which is already deferred.
3. **`FxHashSet<SymId>` instead of a `BitSet`** for liveness facts. The sets are tiny, the `join`
   is a set union, and "did it change" is a length comparison after the union. `BitSet` is a
   performance refinement with no behavioural difference.
4. **No secondary notes on findings.** `Diagnostic::notes` exists and is serialised, but the two
   checkers emit none — "first assigned here" needs a second span to be tracked through the
   solution, which is polish rather than substance.
5. **Proportionate tests, not exhaustive ones.** The project policy calls for extensive and
   adversarial suites; that bar is written for the *sema* layer, whose job is rejecting bad
   programs. This layer emits advisory warnings and cannot affect compilation, so its first
   iteration gets solid per-step coverage of the contracts and the edge cases named in each step —
   not the adversarial sweep 0014's sema work received. This is the compromise most worth revisiting
   if the checkers ever gain teeth.

---

[AI]

## Goal

A small, correct, well-tested dataflow substrate — the part that is reusable — plus two client
analyses that prove it works end to end and produce findings a user would actually want.

The sequencing matters: the framework is built first and the checkers are written against it, so
that adding a third analysis later means writing a lattice and a transfer function, nothing else.

## Why the language makes this tractable

Three properties of pinp as it stands simplify CFG construction considerably, and are worth stating
because they are what keeps this iteration small:

- **No early exits *yet*.** There is no `return`, `break`, or `continue`, so every construct is
  single-entry/single-exit today and lowering a block is a straight recursive walk with no patch
  lists and no unresolved jump targets. An explicit `return` is landing soon, and it is the one
  place where leaning on this simplification would force a rework — so the CFG is shaped for it up
  front. See "Designed for `return`" in step 3: it costs a few lines now and makes the later change
  purely additive.
- **No exceptions or unwinding.** A runtime error (`pinp_runtime_error`) longjmps out of the whole
  program, so it needs no edges.
- **Locals do not escape their block.** The 0006 scoping rules mean a name introduced in a body is
  invisible after it, which bounds every local's live range to its own subtree.

The one complication is that **`if` is an expression**, so control flow can appear inside an
expression (also in `and`/`or` short-circuit). See "CFG shape" below for how v1 handles that.

---

## Step 1 — Source spans ([src/parser/ast.rs](src/parser/ast.rs))

Today the AST carries no positions at all. The lexer records 1-based `line`/`col` per token and the
parser drops them; `SemaError` is two variants wrapping a bare `String`. A diagnostic cannot point
anywhere.

### `Span`

```rust
/// A half-open byte range into the source. Byte offsets rather than line/col: the parser can copy
/// them from token spans with no arithmetic, and a `LineIndex` resolves them for display only when
/// a diagnostic is actually rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}
```

Two arenas gain spans:

- `ProgramAst.spans: Vec<Span>`, parallel to `nodes` — pushed by `ProgramAst::push` exactly as
  `types` and `builtin_members` already are. `push` records `Span::UNKNOWN`; a
  `push_spanned(node, span)` records a real one. **Only the node kinds a finding points at use it
  this iteration** — `Node::Var` and `Node::Global` — so the parser changes in two places rather
  than fifty. A `Span::UNKNOWN` renders as the file with no position, and filling the remaining
  kinds later is mechanical.
- `Stmt::Assign` gains `target_spans: Vec<Vec<Span>>`, parallel to `target_lists`. This is what lets
  an unused-binding or dead-store finding point at the *binder name* rather than at the value
  expression. Assignment targets are `Place`s, not nodes, so they have no span otherwise.

`Token` must therefore expose its byte offset. It currently carries `line`/`col` only; add
`start: u32` (the lexer already has `lexer.span()` in hand at the point it builds each token).

### `LineIndex`

Resolving an offset to `(line, col)` for display is the same computation the lexer's `locate`
already performs. Extract it:

```rust
/// Line-start offsets for a source, so a byte offset resolves to 1-based line/column in O(log n).
pub struct LineIndex { line_starts: Vec<u32> }
```

The lexer switches to using it, so there is one implementation rather than two.

### Statement spans

`Stmt` is an enum stored inline in `Block`/`TopLevel`, not an arena entry, so it has no id to hang a
span off. Rather than restructure it, a statement's span is **derived** from a representative
expression:

```rust
fn stmt_span(ast: &ProgramAst, stmt: &Stmt) -> Span
```

— an `Expr`'s own node, an `Assign`'s first target span joined with its last value, a `While`'s
condition, and so on. This points slightly inside the statement (it does not cover the leading
keyword), which is acceptable for v1; a precise statement span requires the parser to record one and
is deferred.

**Tests:** parser tests asserting spans for each literal/expression form and that a span's text
slice round-trips (`&src[span]` equals the expected source text); `LineIndex` unit tests including
the boundaries (offset 0, offset at a line start, final offset).

---

## Step 2 — Diagnostics ([src/analysis/diagnostic.rs](src/analysis/diagnostic.rs))

```rust
pub enum Severity { Error, Warning }

/// A stable identifier for a class of finding. The discriminant is what appears in output
/// (`PINP0101`) and what a suppression mechanism would key on, so values are never reused.
pub enum DiagnosticCode {
    UnreachableCode,   // PINP0101
    DeadStore,         // PINP0102
    UnusedBinding,     // PINP0103
}

pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: Severity,
    pub span: Span,
    pub message: String,
    /// Secondary locations: "first assigned here", "shadowed here".
    pub notes: Vec<(String, Span)>,
}
```

`DiagnosticCode` carries `code_str()` (`"PINP0101"`) and `title()` (a short rule name), so rendering
and SARIF share one source of truth. This is the structured-error-code refactor that has been on the
backlog; `SemaError` is **not** migrated in this iteration (it stays fail-fast and message-only) to
keep the diff to the new layer, but the enum is designed so it can absorb sema's errors later.

A plain text renderer (`code`, severity, `line:col`, message, then notes) is enough for tests and
for the future runner; SARIF is step 7.

**Tests:** rendering, and a test asserting every `DiagnosticCode` has a distinct numeric code — the
kind of thing that silently breaks when a variant is inserted in the middle.

---

## Step 3 — Control-flow graph ([src/analysis/cfg.rs](src/analysis/cfg.rs))

### Shape

```rust
pub struct BlockId(u32);

pub enum Terminator<'ast> {
    /// Falls through / jumps unconditionally.
    Goto(BlockId),
    /// Two-way branch on a condition expression.
    Branch { cond: ExprId, then_block: BlockId, else_block: BlockId },
    /// Leaves the function, carrying the result expression if there is one. Its sole successor is
    /// `Cfg::exit`. **Any number of blocks may carry this** — exactly one today (the body's
    /// trailing result), several once `return` lands.
    Return(Option<ExprId>),
    /// The unique sink, carried only by `Cfg::exit`. No successors.
    Exit,
}

pub struct BasicBlock<'ast> {
    /// Statements in execution order. Borrowed from the AST, which outlives the analysis.
    pub stmts: Vec<&'ast Stmt>,
    pub terminator: Terminator<'ast>,
}

pub struct Cfg<'ast> {
    pub blocks: Vec<BasicBlock<'ast>>,
    pub entry: BlockId,
    pub exit: BlockId,
}
```

One `Cfg` per `FuncDef`, plus one for the top-level entry. Predecessors are computed once after
construction (`preds: Vec<Vec<BlockId>>`) since a backward analysis needs them.

`exit` is a real block — empty, terminated by `Exit`, and the successor of every `Return`. A unique
sink is not needed for anything in this iteration (one `Return` block seeds a backward analysis just
as easily as one `exit` does), but it is what a post-dominator tree requires, and post-dominance is
the first thing the SSA iteration will want. Establishing it now costs one empty block.

### Construction

A recursive lowering that threads a "current block" and returns the block control falls through to:

```rust
/// Lowers `stmts` starting in `current`; returns the block control continues in afterwards, or
/// `None` when control cannot fall through because every path has left the function.
fn lower_stmts(&mut self, stmts: &'ast [Stmt], current: BlockId) -> Option<BlockId>;
```

Nothing yields `None` in this iteration. The shape is there for `return` — see below.

- `Assign`, `IndexedAssign`, `IndexedAssign2D`, and a non-branching `Expr` append to the current
  block.
- `While { cond, body }` — header block (terminator `Branch`), body block, exit block; the body's
  end gets `Goto(header)`. The back edge is what makes the solver iterate, so it is the first thing
  the framework tests exercise.
- `Loop { body, cond, until }` — post-test: body block, then a `Branch` at the end; `until` swaps
  the successors.
- `For` / `ForArray` — header/body/exit like `While`. The loop variable's binding is modelled as an
  assignment at the top of the body so liveness sees the definition.
- `Stmt::Expr(Node::If { .. })` — an `if` in statement position expands to real branches: one
  diamond per arm, joined at a merge block.

### Designed for `return`

An explicit `return` lands soon. It is accommodated by three decisions taken here rather than later,
because each is nearly free now and expensive to retrofit:

1. **Lowering yields `Option<BlockId>`**, where `None` means control does not fall through. This is
   the only decision that genuinely matters. A builder written as "every lowering returns the block
   control reaches" bakes in the assumption that control always continues; unpicking it later means
   revisiting every lowering arm *and* every join point, since a merge block must then be built from
   only those predecessors that actually reach it. Threading `Option` from the start costs a handful
   of `?`s and one rule at each join: drop the arms that yielded `None`, and if none remain, yield
   `None` yourself.
2. **`Return` is not unique.** Nothing asserts "exactly one returning block", and the CFG exposes
   `returning_blocks()` rather than a single field. Today it yields one entry.
3. **`exit` is a unique sink** that every `Return` edges into, so "the fact on leaving the function"
   has one home regardless of how many return sites there are.

What `return` then costs in this layer: emit `Terminator::Return` at the return site and yield
`None`. Nothing else — not the solver, not the lattices, not either checker. `break` and `continue`
ride the same mechanism, adding only a stack of `(break_target, continue_target)` for the enclosing
loop.

Two things arrive free the moment `return` exists:

- **Unreachable code after a `return`** — the classic finding, and step 5 reports it without a line
  of new code: the block simply never reaches fixpoint.
- **Definite-return checking** ("not all paths return a value") becomes a client of this framework
  instead of an ad-hoc walk in sema — it is reachability of `exit` along fall-through edges.

The one thing `return` *will* force is lifting the if-expression compromise below, since a `return`
inside an `if` arm is control flow inside what this iteration treats as an atomic expression. Until
then the arms of an if-expression contain no early exits by construction, so the approximation
stays sound.

### The if-expression compromise

An `if` nested *inside* an expression (`x = a if c else b`) is **not** split into blocks in v1: the
statement stays atomic and both arms are treated as "may evaluate". The same applies to `and`/`or`
short-circuit, where the right operand may or may not be evaluated.

This is deliberately conservative in the correct direction for both checkers here: liveness is a
*may* analysis, so a union over both arms over-approximates the live set, which can only *suppress*
a dead-store finding, never invent one. It is recorded because it is the first thing to revisit when
a *must* analysis (definite assignment, ownership) is added — those need the real branches, and that
is a prerequisite of the ownership iteration.

**Tests:** in-module, asserting block counts, terminator kinds, and edges for each construct; that a
`while` produces a back edge; that a nested `if` inside a loop body joins correctly; that `exit` is
reached from every `Return`; and a `dot`-format dump helper used by the tests to make a failure
readable.

---

## Step 4 — Dataflow framework ([src/analysis/dataflow.rs](src/analysis/dataflow.rs))

```rust
pub trait Lattice: Clone + PartialEq {
    /// The identity element for `join` — the fact at an unreached program point.
    fn bottom(&self) -> Self;
    /// Least upper bound, in place. Returns whether `self` changed, which is what lets the
    /// worklist stop.
    fn join(&mut self, other: &Self) -> bool;
}

pub enum Direction { Forward, Backward }

pub trait Analysis<'ast> {
    type Fact: Lattice;
    const DIRECTION: Direction;
    /// The fact entering the entry block (forward) or the exit block (backward).
    fn boundary(&self) -> Self::Fact;
    /// How one statement transforms a fact.
    fn transfer_stmt(&self, stmt: &'ast Stmt, fact: &mut Self::Fact);
    /// How a terminator transforms a fact (a `Branch` reads its condition).
    fn transfer_terminator(&self, terminator: &Terminator<'ast>, fact: &mut Self::Fact);
}

/// Runs `analysis` over `cfg` to fixpoint, returning the fact at each block's entry and exit.
pub fn solve<'ast, A: Analysis<'ast>>(cfg: &Cfg<'ast>, analysis: &A) -> Solution<A::Fact>;
```

The solver is a worklist: seed every block with `bottom`, set the boundary block, then pop blocks
and re-evaluate, pushing successors (or predecessors, backward) whose input changed. Termination
rests on the lattice having finite height and `join` being monotone — both hold for the bitset facts
used here, and the module doc states the obligation for anyone adding a lattice.

Liveness facts are `FxHashSet<SymId>` (compromise 3): `join` is a set union and "did it change" is a
length comparison across it. A dedicated `BitSet` makes `join` a word-wise OR and gets the changed
flag from the same loop — a performance refinement with no behavioural difference, worth doing when
there is a lattice large enough to care.

**Tests:** a deliberately trivial analysis (e.g. "has statement N executed") to test the driver
independently of any real checker; a loop CFG that requires more than one pass to converge; a test
asserting the solver visits a bounded number of times (guards against a non-monotone join looping
forever); empty-CFG and single-block edge cases.

---

## Step 5 — Reachability ([src/analysis/checks/reachability.rs](src/analysis/checks/reachability.rs))

The simplest client, and the one that validates the framework on a real question. Forward analysis,
fact = reached/not; a block is unreachable if its fact is still `bottom` at fixpoint.

Edges are pruned where a condition is a **literal** `true`/`false`, which is what makes anything
unreachable at all in a language with no `return`:

- `while false` / `loop … until true` — body unreachable.
- A literal-false `if` arm; a literal-true arm makes the *subsequent* arms and the `else`
  unreachable.
- A `for` over a literal range that is empty (`1..<1`, `5..1` ascending) — body unreachable. Sema
  already computes literal range lengths (`literal_range_length`), so the constant is in hand.

No folding beyond a literal: `while 1 > 2` is not recognised (compromise 2). Constant propagation
subsumes this cleanly and is deferred; doing half of it by hand here would only have to be undone.

Finding: **PINP0101** at the first unreachable statement, with a note pointing at the condition that
made it so.

**Tests:** one per source of unreachability above, plus the negative cases (a variable condition is
never constant-folded, `while true` is reachable and has no unreachable exit block complaint — the
exit *is* reachable in pinp since there is no `break`, so a `while true` program simply never
terminates and that is not this checker's business).

---

## Step 6 — Liveness ([src/analysis/checks/liveness.rs](src/analysis/checks/liveness.rs))

Backward analysis, fact = the set of `SymId`s live at a program point. Standard transfer: a read
makes a symbol live, a write kills it, and a statement's reads are processed after its writes.

Two findings fall out:

- **Dead store (PINP0102).** A write whose symbol is not live immediately after it: the value is
  overwritten or the scope ends before anything reads it. Reported at the target's span. (The
  "overwritten here" note is compromise 4 — the second span is not tracked through the solution.)
- **Unused binding (PINP0103).** A symbol with no read anywhere in its function. Reported at the
  first binding. Distinct from a dead store because the message and the fix differ, and because
  reporting *every* store to a never-read binding would be noise.

Cases that must **not** be reported, each with a test:

- A global assigned in the entry and read from a function — globals are live at the entry's exit, so
  the boundary fact seeds them.
- The `_` don't-care binder, which exists precisely to say "unused".
- A loop variable that the body ignores (`for idx in 1..3` with no use of `idx`) — arguably a real
  finding, but it is idiomatic and firing on it would make the checker unusable. Recorded as a
  deliberate suppression, revisit if it proves wrong.
- A `str` binding whose only "use" is its scope-exit free. Freeing is not a read; a `str` local
  assigned and never read *is* a dead store, and correctly so.

**Tests:** in-module liveness facts for straight-line code, an `if`, and a loop (a value assigned
before a loop and read inside it is live across the back edge — the case a single pass gets wrong);
then diagnostic-level tests for each finding and each suppression.

---

## Step 7 — SARIF and the `check` entry point ([src/analysis/sarif.rs](src/analysis/sarif.rs))

```rust
/// Runs every checker over an analysed program, returning all findings in source order.
pub fn check(ast: &ProgramAst, source: &str) -> Vec<Diagnostic>;

/// Serialises findings as SARIF 2.1.0.
pub fn to_sarif(diagnostics: &[Diagnostic], source_path: &str, source: &str) -> String;
```

SARIF is the interchange format static-analysis tooling consumes, which is the reason for choosing
it over a bespoke JSON shape. The emitted document carries `version`, one `run`, a `tool.driver`
with `rules` derived from `DiagnosticCode` (id, name, short description), and one `result` per
finding with `ruleId`, `level`, `message.text`, and a `physicalLocation` carrying both the byte
`region` (`charOffset`/`charLength`) and the resolved `startLine`/`startColumn`.

**Written by hand, without `serde_json`.** The document shape is fixed and small, the only subtlety
is string escaping, and the project's dependency list is deliberately short. The escaping helper is
tested directly against quotes, backslashes, newlines, and a non-ASCII byte.

**Tests:** a golden-ish test asserting the SARIF for a two-finding program contains the expected
rule ids, levels, offsets, and resolved line/columns; a test that the output parses as JSON (a
minimal recursive-descent validator in the test module, or a byte-level check that quotes and braces
balance — cheap insurance against a malformed escape).

---

## Step 8 — README

A short **Analyses** section: what the layer does, the current finding list with codes, and the
SARIF output. The analyses are invisible from the language surface, so nothing else advertises them.

---

## Module layout

```
src/analysis/
    mod.rs                  — the `check` entry point, Diagnostic re-exports, module docs
    diagnostic.rs           — Severity, DiagnosticCode, Diagnostic, text rendering
    cfg.rs                  — BasicBlock, Terminator, Cfg, construction, dot dump
    dataflow.rs             — Lattice, Direction, Analysis, BitSet, the worklist solver
    sarif.rs                — SARIF 2.1.0 emitter
    checks/
        reachability.rs
        liveness.rs
```

`analysis` sits beside `sema` in [src/lib.rs](src/lib.rs) and depends on `parser` only — it consumes
a typed `ProgramAst` and knows nothing about codegen or LLVM, so it builds without the `llvm`
feature. That also means its tests run in the fast, backend-free part of the suite.

## Tests

Per the project test policy, each step's tests are written and reviewed before its code. By layer:

- **parser** (in-module): spans per node form, span text round-trip, assignment target spans,
  `LineIndex` boundaries.
- **analysis/cfg** (in-module): block/edge structure per construct, back edges, nested control flow,
  the if-expression atomicity compromise made explicit in a test so the limitation is documented
  where it will be found.
- **analysis/dataflow** (in-module): the driver against a trivial analysis, multi-pass convergence,
  a bounded-iteration guard, degenerate CFGs.
- **analysis/checks** (in-module): fact-level assertions, then finding-level assertions including
  every suppression above.
- **e2e** ([tests/analysis.rs](tests/analysis.rs)): source in, diagnostics out — code, severity,
  and resolved line/column for each finding; a clean program yields none; a program with several
  findings reports them all, in source order (the batch-reporting property, which is the one thing
  fail-fast sema cannot demonstrate).
- **SARIF**: as described in step 7.

## Deferred

- **Ownership and lifetime as a dataflow analysis** — the successor iteration. Lattice
  `{Owned, Borrowed, Moved, Freed}` per binding, which retires 0014's syntax-directed
  `owns_str_result`, removes the copy-on-borrow pessimisation (`a = b = s` no longer copies), and
  turns the string model into leak / double-free / use-after-free detection. It needs the real
  branches that the if-expression compromise above skips, so that compromise is lifted there.
- **Constant propagation** — a lattice of `{Bottom, Const(value), Top}` and the always-true/
  always-false condition findings it enables. It would also subsume the ad-hoc constant folding
  reachability does in step 5.
- **A pinp IR in SSA form** — dominator tree, dominance frontiers, φ-placement, replacing the
  alloca-per-local pattern that currently leaves SSA construction to LLVM's `mem2reg`.
- **The five one-day compromises** listed near the top: spans on every node kind, constant folding
  in reachability, a `BitSet` lattice, secondary notes on findings, and an adversarial test sweep.
- **`return`, `break`, `continue`** — the language change belongs to its own iteration. This layer
  is built to absorb it (step 3, "Designed for `return`"): a terminator and a `None` fallthrough,
  plus a loop-context stack for the loop keywords. Definite-return checking is then a client of this
  framework rather than a separate walk.
- **Interprocedural analysis** — a call graph and cross-function fact propagation. pinp has no
  recursion (signatures are recorded after each definition), so the call graph is a DAG and a
  bottom-up summary-based approach is straightforward.
- **Suppression** — a comment pragma (`# pinp: allow(dead-store)`) once there are enough findings
  for it to matter. Comments are currently discarded by the lexer, so this needs them retained.
- **Taint analysis** — deferred until the language has I/O; there are no sources or sinks to
  connect today, and a contrived pair would be worse than none.
- **Alias / pointer analysis** — not planned. pinp's surface has no pointers or references, and
  arrays and strings do not alias by construction, so there is nothing to disambiguate.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
