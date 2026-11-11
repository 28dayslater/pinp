[Human]

# `return`

A function may exit early with the `return` statement:

```
return <expr>
return
```

The first form returns the value of `<expr>`, which must be assignable to the declared return type of
the function, under the same rules as the function's final expression (`int` is promoted to `float`).
The second form is permitted only in a function without a return type (`void`), and is the only form
permitted there.

```
sign(n: int): int is
    if n < 0
        return -1
    if n == 0
        return 0
    1
```

A body that ends in `return` needs no final expression. Neither does a function whose body ends in an
`if` every branch of which returns; such an `if` has no value and is accepted wherever a value of the
function's return type is expected:

```
abs(n: int): int is
    if n < 0
        return -n
    else
        return n
```

A function that has return type other than void, must return value from all paths.
A failure to do that results in an error.
```
sign(n: int): int is
    if n < 0
        return -1
    if n == 0
        return 0
# Error: Not all paths return a value in function abs().
```

`return` outside a function is an error. There will be a built in exit() function to do that.

Code that follows a `return` in the same body will be reported as unreachable (PINP1001).

Deferred: `break` and `continue` (the next iteration).

[AI]

## Goal

The first statement that **leaves** a body: `return` exits the function from any depth of nesting.
0002 deferred it; the IR was built for it (0017's flat level already ends functions with a `Return`
terminator, and the ownership model frees every string slot on the way out). The design work is in
sema — typing a body that never finishes — and in keeping the code a `return` leaves behind
consistent through desugar, flatten, and the analyses. `break`/`continue` reuse the same machinery
in the next iteration.

## Grammar

```
return_stmt := 'return' [ expr ]
```

The `expr` is absent when the next token ends the statement (`Newline`, `Dedent`, `Eof`).

## Decisions (human, 2026-11-11)

1. **A statement, typed *never*.** `return` is parsed only where a statement may stand. Sema gives
   it a **never** type (the bottom type: it produces no value because control does not continue),
   so a body or an `if` branch ending in `return` diverges and needs no trailing value. `x = return
   1` and `1 + return 2` are parse errors. The `x or return y` sugar is dropped (human,
   2026-11-11), so nothing is planned to put `return` in expression position.
2. **Top level: an error** — "Return outside a function." Ending a program early is left to a
   future built-in `exit()`.
3. **Code after `return`: a warning**, through the existing unreachable-code checker.
4. **`return` only**; `break`/`continue` next.

## Lexer

`return` joins the keyword table as `KwReturn` and becomes reserved. A program using `return` as a
variable name stops parsing; none in the test corpus does.

## Parser

- `Stmt::Return { value: Option<ExprId>, span: Span }`. The span covers `return` and its value: a
  `Stmt` has no node of its own to hang one on (like `Stmt::For::var_span`), and diagnostics about
  the statement — top level, unreachable — point at it.
- `parse_stmt` dispatches `KwReturn` like `while`/`loop`/`for`.
- **Function body rule moves to sema.** The parser's "Function body must end with an expression."
  goes: a block body may end in any statement (`result: None`), and whether it yields a value on
  every path is sema's question — it needs types, which the parser does not have.
- **`return` in expression position** — `parse_primary` meeting `KwReturn`, which covers the
  single-line body, an operand, an assignment's right side, and a ternary arm — reports
  "Return cannot be used as a value." instead of a generic unexpected-token error.

## Sema

- **`PinpType::Never`.** A new variant in `PinpType`, never written by the user (there is no type
  name `never`). It types a diverging construct:
  - a `return` statement;
  - a **block** containing a diverging statement — a `return`, or an expression statement of type
    `Never` — whatever its trailing expression;
  - an **`if` with an `else`** whose every branch diverges.
- **`if` join.** `Never` is assignable to every type and joins as the identity: a diverging branch
  does not affect the `if`'s type. With `else`, if every branch diverges the `if` is `Never`; if the
  rest end in values they join as before; a non-diverging branch without a value makes it `Void`.
  Without `else` an `if` stays `Void`, whatever its arm does (the missing `else` falls through).
- **Where `Never` may stand.** As a statement, as a block's trailing expression, and as an `if`
  branch. As an assignment's value, an operand, or an argument it is rejected like `Void` — which
  `return` being a statement already makes rare: only an `if` all of whose branches return can carry
  it there. Message: "Expression never produces a value."
- **Return checks**, against the enclosing function's declared type:
  - `return expr` with `expr` not assignable → "Function `f` returns {T:?} but is declared {R:?}.";
  - `return expr` in a `void` function → "Function `f` is void and cannot return a value.";
  - bare `return` in a non-`void` function → "Function `f` must return a value of type {R:?}.";
  - `return` at the top level → "Return outside a function." (sema, which knows it is outside a
    function; the parser shares `parse_stmt` between both).
- **Missing return (human, 2026-11-11).** A non-`void` function whose body can reach its end
  without a value is a sema error: "Not all paths return a value in function `f()`." The body
  is fine when it diverges (`Never`) or ends in a trailing expression of a value type; it fails
  when it ends in a statement, or in an expression that is `Void` — an `if` without `else` whose
  arm returns, or an `if`/`else` with a branch that neither returns nor yields a value. The error
  points at the function's name, as the header rules do: the fall-through has no span of its own.
  The existing "Function `f` body yields … but is declared …" stays for a value of the wrong type.
- **`void` functions** may now end in a statement, since the parser no longer insists on a trailing
  expression — `set(x: int) is` / `::g = x` is accepted. Their trailing expression, if any, is
  still evaluated and discarded.
- **Loops never diverge.** `while`, `loop`, and `for` stay `Void` even when their body returns on
  every path: the loop may not run (`while`, `for`), and telling `while true` apart needs constant
  analysis. A non-`void` function whose last statement is such a loop is a missing return.

## IR

- **Structured: `Operation::Return(Option<NodeId>)`**, a statement allowed at the structured level
  only. The verifier rejects it at the flat level, and anywhere but statement position.
- **Flat: `Terminator::Unreachable`.** Ends a block control cannot reach. Emitted by flatten for the
  end of a function whose body diverges, where a `Return` would have no value to return in a
  non-`void` function. No successors and no operand. Printed `unreachable`; emitted as a Cranelift
  trap (it can never execute).

## Desugar

- **`Stmt::Return`** computes its value, then does what the end of a function does: the value is
  moved out (an owned string as is, a borrowed one copied), every string slot is freed, and the
  `Return` statement follows the frees. That is `free_on_exit` reused — a slot never written holds
  the empty string, so freeing every slot is right wherever the `return` stands. The frees are
  placed when the function is finished, once every local is known (see Implementation notes).
  Int-to-float promotion uses the existing `convert`.
- **A diverging body ends without exit code.** When the function body diverges (a top-level
  `return` statement in it, or a `Never` trailing expression), the end-of-function frees and result
  are not generated: the shape's `result` is `None`. Generating them would place code with real
  spans where nothing can reach it — the unreachable-code checker would then report the function's
  last line. A trailing expression the user did write (`return n` / `n + 1`) is kept, as a
  statement evaluated for its effect, so that it is still placed and reported.
- A `Never`-typed `if` is desugared as a statement (no result temporary), like a `Void` one.

## Flatten

- **`Return(value)`** terminates the current block with `Terminator::Return(value)` and continues
  in a fresh block with no predecessors. Statements that follow land there and are what the
  reachability checker reports.
- **Structural tracking of the current block.** Flatten keeps a flag: the current block can be
  reached, as far as structure says (no constants are evaluated). It clears after a `Return`; an
  `If`'s branches start with the flag the `If` met, and its join is reachable when a branch end is
  (or, without an `else`, when the `If` was); a `While` or counted loop's body and exit are
  reachable when the loop was, through its own test; a `DoWhile`'s exit only when its body's end
  is. Every construct sema types `Never` therefore ends with the flag clear.
- **Generated code is not placed where it cannot run.** A counted loop's step (`index += step`), and
  the function's final terminator, depend on the flag: an unreachable loop-body end skips the step
  and just jumps back to the header; a function end with no result and the flag clear gets
  `Unreachable` instead of `Return`. A function with a result keeps `Return(result)` — its type
  matches the signature whether or not anything reaches it. User statements are still placed, so
  that they can be reported.

## Backend

- `Terminator::Return` already emits frees-then-return code per block; several `Return` blocks per
  function are new but need nothing new.
- `Terminator::Unreachable` → `trap`.
- Blocks with no predecessors are filled and left for Cranelift to drop.

## Diagnostic codes

0016 reserved the bands `PINP2xxx` (parser) and `PINP3xxx` (sema) but minted no codes in them:
those errors render without one. 0018's errors are the first to get codes (human, 2026-11-11), each
with severity `error`. Errors from earlier iterations stay uncoded until a chore backfills them.

| code | name | message |
|---|---|---|
| `PINP2001` | `return-as-value` | Return cannot be used as a value. |
| `PINP3001` | `missing-return` | Not all paths return a value in function `f()`. |
| `PINP3002` | `return-outside-function` | Return outside a function. |
| `PINP3003` | `return-type-mismatch` | Function `f` returns {T:?} but is declared {R:?}. |
| `PINP3004` | `void-function-returns-value` | Function `f` is void and cannot return a value. |
| `PINP3005` | `missing-return-value` | Function `f` must return a value of type {R:?}. |
| `PINP3006` | `never-value` | Expression never produces a value. |

Code after a `return` reuses `PINP1001` `unreachable-code` (warning).

Mechanics: `ParseErrorKind` and `SemaErrorKind` gain an optional code that `to_diagnostic` copies,
and `DiagnosticCode` gains the seven variants with `Severity::Error` — the first error-severity
rules, so its "everything is advisory" doc line goes. Tests assert codes, not message strings.

## Analyses

- **Reachability:** unchanged algorithm. The block after a `return` has no predecessor, so it stays
  bottom and its first statement is reported. The module doc's "in a language with no `return` or
  `break`" premise is rewritten.
- **Liveness:** the algorithm is unchanged, but `return` changes what it finds — a function now
  has several exits, and each cuts the paths a value can be read along.
  - **Every `Return` block gets the boundary state** (nothing live), as the single one does today. A
    `return expr` reads its value through the terminator, so the variables in `expr` are live
    up to it.
  - **New dead stores.** A write followed on some path only by a `return` that does not read it is
    now dead along that path; if no other path reads it either, it is reported:
    `t = n * 2` / `if n < 0` / `return 0` / `t` is clean, but `t = 1` / `return 0` reports `t`.
  - **Reads in unreachable code.** The code after a `return` is not a successor of anything, so a
    read there keeps nothing live before the `return`: `a = 1` / `return 0` / `a` reports a dead
    store at `a = 1` (and the unreachable `a`). The unused-variable rule is different — it scans
    every block for reads, unreachable ones included — so such a variable counts as *used*, and
    the report is the dead store, not "never used". Kept rather than special-cased. (Code under a
    constant-false `if` differs: liveness does not prune that edge, so a read there still counts.)
  - **Frees are not reads.** Each `return` brings its own frees of every string slot; liveness
    already skips `StrFree` operands, which now matters at every exit instead of one.
  - **Loops.** A `return` inside a loop body ends that path; the body's remaining paths still
    reach the back edge. The counted loop's step is not generated after a `return` (Flatten), so
    the index is not made live by code that cannot run.
  - **Globals.** `return` is a sema error at the top level, so the entry keeps its single exit and
    its private-globals treatment is untouched. Functions do not track globals.
  - `Unreachable` blocks start at bottom: nothing is live where control never arrives.

## Test plan (TDD: red → green → refactor)

Adversarial throughout. Placement as usual: lexer/parser/sema/desugar/flatten/IR unit tests in
their modules, end to end in `tests/` through `PinpJit`.

- **lexer:** `return` is `KwReturn`; `returns`, `return_value`, `_return` stay identifiers.
- **parser:** `return` with and without value; bare `return` before `Dedent` and before `Eof`;
  `return` inside every body kind; `Stmt::Return` span covers the value; a function body ending in
  `return`, in a loop, or in an assignment parses (`result: None`); "Return cannot be used as a value."
  for the single-line body, an assignment's value, an operand, a call argument, a ternary arm;
  `return` followed by more statements parses (the rest is unreachable, not invalid).
- **sema (extensive):**
  - types: `return` value assignable to the declared type; `int` returned from a `float` function;
    `float` from an `int` function rejected; `str` and `bool` returns; returning an array or a
    range from a scalar function rejected;
  - void: bare `return` in a void function; `return 1` in a void function rejected; bare `return` in
    an `int` function rejected; `return f()` where `f` is void, in a void function, rejected;
  - top level: `return` at the top level rejected, inside a top-level `if`/loop body too;
  - never: an `if`/`else` with every branch returning is accepted as an `int` body; mixed — one
    branch returns, the other yields — joins to the other's type (`int`, `float` promotion,
    `str`); `elif` chains
    with all arms returning plus `else`; nested `if`s returning at depth; a block with `return`
    mid-block is `Never`; a `Never` if assigned (`x = if … return … else return …`) rejected;
  - missing return: each of these, in a non-`void` function, is "Not all paths return a value in
    function `f()`." — trailing `if` without `else` whose arm returns; `if`/`elif` all returning, no `else`;
    `if`/`else` where one branch returns and the other ends in an assignment; nested: outer `else`
    holding an inner `if` without `else`; body ending in an assignment; body ending in a `while`,
    `loop`, or `for` whose body returns on every path; a `return` nested only inside a loop. Each
    accepted counterpart: the same with a trailing value, or with an `else` returning. The error
    points at the function name. A wrong-typed trailing value still gets "body yields … but is
    declared …"; a `void` function ending in an assignment or a loop is accepted;
  - scope: `return` reads locals of nested bodies, parameters, and `::` globals; the type check
    uses the enclosing function's return type from within nested bodies.
- **desugar / IR:** structured `Return` appears in statement position with frees before it for
  every string slot; a diverging body has `result: None` and no trailing frees; verifier rejects
  `Return` at the flat level and in value position.
- **flatten:** a `return` ends its block with `Return` and starts a predecessor-less block; a
  counted loop whose body ends in `return` has no step write; a diverging function ends in
  `Unreachable`; a non-diverging one in `Return` as before; `if`/`else` both returning makes the
  join unreachable; a loop after a `return`-ending `if` without `else` stays reachable.
- **analyses:** code after `return` is reported once, at its first statement; code after an
  `if`/`else` that returns on both arms is reported; code after `if`s *without* `else` that return
  (the `sign` shape) is not reported; a `return` as last statement of a loop body
  reports nothing (no false positive from the step); no report for a diverging function's end;
  liveness: a value returned is not a dead store, at any nesting depth; a write read on one path
  and returned past on another is clean; a write followed only by a `return` that ignores it is a
  dead store; a variable read only after `return` gets a dead store at its write, not "never
  used"; `return` inside a loop does not hide reads on the loop's other paths; string frees before
  each `return` do not count as reads (an unread string local is still reported); a function with
  several returns reading different variables reports none of them.
- **end to end:** early return from straight-line code, from `if`/`elif`/`else`, from `while`,
  `loop … until`, counted `for`, array `for`, and nested loops three deep; a `return` in a loop that
  stops at the first match (search); a float function returning an `int` value; void function with
  early bare `return` (observed through a global it would have changed); strings: returning a
  parameter, a local, a literal, a concatenation, a string from inside a loop with other string
  locals live — leak balance checked in `tests/string_leaks.rs` for each; a function returning in
  one call and falling through in another.

## Implementation notes

- **Frees at a `return` are placed when the function is finished**, not when the `return` is
  desugared. Locals are created as their first assignment is desugared, so a `return` early in a
  loop body would otherwise miss a string slot assigned later in that body — which on an earlier
  pass already holds a string. `finish` walks the structured body once every local is known.
- **String parameters are not supported** (0014); tests that return strings build them in locals.
- **Two defects on `main` blocked tests here** and were fixed separately, on `fix/block-bodies`
  (merged into `main` 2026-11-11):
  the parser read a block `if` directly followed by another `if` as a one-line conditional
  (`x if c else y`), and desugaring dropped the trailing expression of a `while` or `loop` body —
  a body ending in an `if` silently skipped it. `return` made both common: the `sign` shape, and
  a loop whose body ends in `if … return`.

## Deferred

- **Array and matrix return types** (human, 2026-11-11): a function cannot yet declare one —
  pinp has no type names for them — so returning an array is a type mismatch. A later iteration.
- Error codes for the errors of earlier iterations: a chore (human, 2026-11-11).

## Resolved (sign-off)

1. `return` is a statement typed `Never`; `return` in expression position is a parse error. The
   `x or return y` sugar is dropped.
2. Top-level `return` is a sema error.
3. Code after `return` is a PINP1001 warning.
4. `break`/`continue` are the next iteration.
5. A non-`void` function that can reach its end without a value is a sema error ("Not all paths
   return a value in function `f()`."); the parser's trailing-expression rule moves there, so
   a `void` function may end in a statement.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
