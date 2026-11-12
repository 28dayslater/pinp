[Human]

# `break` and `continue`

Inside a loop body, `break` leaves the loop and `continue` skips the rest of the current pass:

```
break [@label]
continue [@label]
```

Without a label, both apply to the innermost enclosing loop — `while`, `loop … while`/`loop … until`,
or `for`.

```
sum_of_odd(n: int): int is
    total = 0
    for i in 1..n
        if i mod 2 == 0
            continue
        total += i
    total

count_until_zero(): int is
    count = 0
    for v in [3, 1, 0, 5]
        if v == 0
            break
        count += 1
    count
```

A loop is labelled by a trailing `@label` at the end of its header line, so the label does not
change the indentation of the code. `break @label` and `continue @label` then apply to that loop:

```
find(target: int): int is
    found = -1
    for i in 1..10 @rows
        for j in 1..20
            if i * j == target
                found = i * 100 + j
                break @rows
    found
```

`loop … while`/`loop … until` is labelled on its `loop` line:

```
loop @retry
    ...
until done
```

In `loop … while`/`loop … until`, `continue` goes to the condition test, as in C's `do … while`.
In a `for`, it moves on to the next element. A `for` over a matrix is one loop: `break` leaves it,
`continue` moves on to the next element.

`break` and `continue` outside a loop are errors, and so is a label that does not name a loop around
them. Neither crosses a function: a loop around a call does not make `break` valid inside the called
function.

Like `return`, both are statements: `x = break` is an error. Code that follows them in the same body
will be reported as unreachable (PINP1001).

[AI]

## Goal

The two remaining statements that **leave** a body. 0018 built the machinery — the `Never` type, the
structural reachability flag in flatten, the unreachable-code report — for `return`; `break` and
`continue` reuse it with a loop instead of the function as the target. What is new is that the
target is a loop, chosen by a label or by nesting, and that pinp's loops are not one IR loop each:
a matrix `for` is two nested counted loops, and a user `break` there must leave both.

## Grammar

```
while_stmt    := 'while' expr [ label ] block
loop_stmt     := 'loop' [ label ] block ( 'while' | 'until' ) expr
for_stmt      := 'for' binders 'in' expr [ label ] block
break_stmt    := 'break' [ label ]
continue_stmt := 'continue' [ label ]
label         := '@' identifier          (one token, no space after '@')
```

## Decisions (human, 2026-11-11)

1. **Trailing labels, spelled `@name`** — at the end of the header line, so labelling a loop changes
   no indentation. `@` because a plain identifier there would turn typos (`while done retry`) into
   labels and close the slot to future syntax; `<name>` was rejected (it collides with `<` and
   with placeholder notation in docs).
2. **`continue` in `loop … while`/`until` goes to the condition test** (C's `do … while`).
3. **Outside a loop: a sema error**, as `return` outside a function is.
4. **Statements typed `Never`**, as `return`: in expression position they are parse errors.
5. **A matrix `for` is one loop** to the user, whatever it becomes in the IR.

6. **Labels have their own namespace**: `for i in xs @i` is legal; `break @i` cannot mean the
   variable.
7. **No shadowing**: a loop may not reuse the label of a loop it is inside — `break @outer` would be
   ambiguous to a reader. Sibling loops may reuse a label.
8. **An unused label is not reported.**
9. **`@` stays free of operators** — matrix multiplication will be `*`, the dot product `dot`.

## Lexer

- `break` → `KwBreak`, `continue` → `KwContinue`, both reserved.
- `@` immediately followed by an identifier lexes as one `Label` token (its text keeps the `@`);
  `@` alone, or followed by anything but a name, is the lexer's usual "Unexpected character `@`."
  `@` has no other use.

## Parser

- `parse_while`, `parse_for` take an optional `Label` token after the header expression; `parse_loop`
  after `loop`. No expression can contain a `Label` token, so the Pratt loop stops before it: one
  token test per header, no lookahead.
- Loops gain `label: Option<Label>`, `Label { name: SymId, span: Span }` — interned like any
  name, the span covering `@name` for diagnostics.
- `Stmt::Break { label, span }`, `Stmt::Continue { label, span }`: the span covers the keyword and
  the label.
- In expression position `parse_primary` reports "Break cannot be used as a value." / "Continue
  cannot be used as a value.", as it does for `return`. A `Label` anywhere but after a loop header
  or `break`/`continue` is a parse error.

## Sema

- **Loop stack.** The analyzer keeps a stack of the loops it is inside, each with its optional
  label; cleared at a function body.
- **Resolution.** An unlabelled `break`/`continue` needs a non-empty stack: "Break outside a loop."
  / "Continue outside a loop.". A labelled one searches the stack from the top: not found → "No
  loop labelled `@name` around this statement." (also when the label exists elsewhere — a sibling
  loop, or a loop in the caller). A loop labelled like one on the stack → "Loop label `@name`
  already names an enclosing loop.". All at the offending span.
- **`Never`.** Both statements diverge: `analyze_stmt` reports them as diverging, so a block or an
  `if` branch ending in one is `Never`, exactly as with `return`. Loops stay `Void` (0018), so the
  divergence never leaks out to make a function body `Never`; missing-return is unchanged.

## IR

- **Structured: `Operation::Break(u32)` and `Operation::Continue(u32)`**, statements allowed at the
  structured level only, like `Return`. The operand is the **depth**: how many enclosing structured
  loops to step out of before acting, 0 meaning the innermost (as WebAssembly's `br`). Printed
  `break 1` / `continue 0`. The verifier rejects a depth that exceeds the loops around the
  statement (`VerifyError::NoEnclosingLoop`); an `If` is not a loop, and a `While`'s header is
  outside its loop.
- **Why a depth, not a node id:** the loop's node is created after its body, so a body cannot name
  it; a depth is known while the body is desugared. Labels are a front-end matter — the IR never
  sees them.
- **No new flat operations.** At the flat level both are plain `Jump` terminators.

## Desugar

- Desugar keeps, per function, a stack of **user loops**: for each, its label and how many
  structured loops it opened (1 for `while`, `loop`, a range `for`, and an array `for`; 2 for a
  matrix `for`). It resolves labels against this stack itself — sema has already guaranteed they
  resolve, and the search is a few entries deep.
- A `break` targeting user loop *k* becomes `Break(d)`, where *d* is the number of structured loops
  opened by the user loops above *k*, plus *k*'s own count minus one — out of every IR loop *k*
  opened. A `continue` targeting *k* becomes `Continue(d)` with *d* counting only the loops above
  *k*: *k*'s innermost IR loop. In a matrix `for` that is the column loop's step — the next element;
  at the end of a row the column loop ends and the row loop steps.
- **No frees.** String slots live for the whole function, so leaving a loop leaves nothing to free.

## Flatten

Flatten keeps a stack of loop targets — `(exit block, continue block)` — pushed when a loop is
entered:

| loop | `continue` target | `break` target |
|---|---|---|
| `While` | the header (the test) | the exit |
| `DoWhile` | the condition block | the exit |
| `CountedLoop` | the step block | the exit |

`Break(depth)` / `Continue(depth)` end the current block with a `Jump` to the target `depth` entries
from the top of the stack, start a fresh block with no predecessors, and clear the reachable flag —
as `Return` does.

**Reachability flag, extended.** Each loop target also records whether a `break` or `continue`
reached it while the flag was set:

- `While` / `CountedLoop` exit: reachable when the loop was (its own test) — unchanged.
- `DoWhile` condition: reachable when the body's end is, **or** a `continue` reached it; its exit
  when the condition is, **or** a `break` reached it.
- `CountedLoop` step: generated when the body's end is reachable **or** a `continue` reached it
  (0018 generated it only for a reachable body end).

The step and the `DoWhile` condition get a block of their own only when a `continue` targets them:
it is created at the first such `continue`, and the body's end then jumps into it. A loop without
`continue` keeps today's layout — the step or condition at the end of the body block.

## Backend

Nothing new: `Jump` terminators only.

## Diagnostic codes

| code | name | severity | message |
|---|---|---|---|
| `PINP2002` | `break-as-value` | error | Break cannot be used as a value. |
| `PINP2003` | `continue-as-value` | error | Continue cannot be used as a value. |
| `PINP3007` | `break-outside-loop` | error | Break outside a loop. |
| `PINP3008` | `continue-outside-loop` | error | Continue outside a loop. |
| `PINP3009` | `unknown-label` | error | No loop labelled `@name` around this statement. |
| `PINP3010` | `shadowed-label` | error | Loop label `@name` already names an enclosing loop. |

One code per keyword, as `return` has its own: the message names the keyword anyway, and a
reader looking a code up finds one statement. Code after `break`/`continue` reuses `PINP1001`
`unreachable-code`.

## Analyses

Algorithms unchanged; the new edges change what they find.

- **Reachability:** the block after a `break`/`continue` has no predecessor and is reported. A
  `loop … until` whose body always `break`s has an unreachable condition, which is reported —
  correctly: the condition never runs.
- **Liveness:**
  - `break` makes the code after the target loop a successor: a value written before a `break` and
    read after that loop is live (not a dead store) — through several loops for a labelled one.
  - `continue` makes the target's next test a successor: a write before `continue` read by the
    condition or the next pass is live; one read only by the rest of the skipped pass is a dead
    store along that path, reported when no other path reads it.
  - A `continue` in a counted loop reaches the step, so the index is live up to it.

## Test plan (TDD: red → green → refactor)

Adversarial throughout; placement as usual.

- **lexer:** `break`/`continue` are keywords; `breaks`, `continue_at`, `_break` stay identifiers;
  `@rows` is one `Label` token with text `rows`; `@ rows`, `@` at end of line, `@1` are lex errors;
  `@_x`, `@a1` are labels.
- **parser:** labels on every loop kind (`loop` on its own line); label after a wrapped header
  expression; `break`/`continue` with and without a label in every loop kind, nested in `if`; spans;
  followed by more statements parses; as a value — single-line body, assignment value, operand,
  call argument, ternary arm, `return break` — is the coded parse error; `break 1`, `break rows`
  (no `@`), a label on an `if`, a label after `until cond`, a label at statement start, two labels
  on one header — parse errors.
- **sema (extensive):**
  - outside a loop: top level, top-level `if`, function body, function `if`; inside a function
    called from a loop; after a loop has ended — rejected, with the code;
  - labels: `break @outer` from the inner loop; from an `if` in the inner loop; `continue @outer`;
    label of a sibling loop → unknown; label of a loop in the caller → unknown; label after its loop
    ended → unknown; shadowing at depth 2 and 3 → error; sibling loops reusing a label → accepted;
    label equal to a variable or binder name → accepted; unlabelled `break` inside a labelled
    inner loop targets the inner one;
  - `Never`: `if`/`else` with `break` in one branch and a value in the other joins to the value
    type; an `if` with `break`/`continue` in every branch assigned → "Expression never produces a
    value."; mixed `break`/`continue`/`return` branches are `Never`;
  - missing return: a function ending in a loop that `return`s, with a `break` too, is still a
    missing return; with a trailing value after the loop, accepted.
- **desugar / IR:** `while`/`loop`/range `for`/array `for` produce `Break(0)`/`Continue(0)`; matrix
  `for` produces `Break(1)`/`Continue(0)`; a labelled `break` across two plain loops → `Break(1)`,
  across a matrix `for` and a plain loop → `Break(2)`, `continue` to an outer matrix `for` →
  `Continue(1)`; verifier rejects `Break`/`Continue` at the flat level, in value position, and with
  a depth beyond the enclosing loops.
- **flatten:** each loop kind's `break` jumps to its exit and `continue` to its
  test/condition/step; a labelled `break` jumps past every inner loop; a counted loop whose body
  ends in `continue` keeps its step; one whose body ends in `break` has no step; a `DoWhile` whose
  body always breaks has an unreachable condition; an outer `DoWhile`'s condition reached only by an
  inner `continue @outer` is reachable.
- **analyses:** code after `break`/`continue` is reported once; `loop … until` with an always-
  breaking body reports the condition; liveness: write → `break` → read after the loop is clean;
  write → `break @outer` → read after the outer loop is clean; write → `continue` → read in the
  condition is clean; write read only after a `continue` in the same pass is a dead store; no false
  report from the counted-loop step after `continue`.
- **end to end:** every loop kind with `break` and with `continue`, including `continue` in
  `loop … until` evaluating the condition; nested loops — unlabelled `break` leaves only the inner,
  `break @outer` both, `continue @outer` resumes the outer's next pass (and its counted step runs);
  matrix `for` with `break` (leaves both dimensions) and `continue` (next element, also across a row
  end); `break`/`continue` at the top level inside a loop; `break` in a loop inside a function with
  string locals — leak balance in `tests/string_leaks.rs`.

## Implementation notes

- **Arrays are never freed yet** — a bare `a = [1, 2, 3]` allocates and nothing frees it (the
  array ownership work is still ahead). The leak tests therefore use ranges only; `break` out of a
  matrix `for` is covered end to end instead. Not a 0019 regression: `main` shows the same count.
  Tracked as KI-0008.
- **Test corrections while going green**: a slice in a parser test is written `[0..1]`, not `[0:1]`;
  the parser test helper now also walks a block's trailing `if`; one flatten test now checks the
  exit block directly — a dead block, printed after it, holds the never-reached `until` condition.
  The predicted block layouts held.

## Deferred

- Warnings about pointless loop control — a `break` as a loop body's last statement (the loop runs
  at most once), a `continue` as the last one (does nothing) (human, 2026-11-11).

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
