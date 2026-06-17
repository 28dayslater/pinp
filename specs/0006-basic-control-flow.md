[Human]

# Basic control flow

`if` is an expression and it's value can be used.
One-line syntax: `fu = expr1 if cond1 else expr2`
The expression can go into the second line if too long. There is a requirement that the next line indents to the column at which `expr1` starts:
```
fu = 42 + 142 if a > 42
     else 42
```

Multiline syntax is:
```
if cond1
    body1
[elif cond2
    body2]*
[else
    body3]
```
[]* means 0 or more repetitions.

There is a requirement that every body block has the same indent.
Just like in Rust, the last expression in a body would be the `if` value.
If `if` value is to be used, sema must verify that `else` is present.

`while` loop is a stament and has two forms:

```
while <condition>
    <body>
```

and 

```
loop
    <body>
while/until <condition>
```
`until` is used to loop as long as the condition is false.

## Scoping

`if` and loop bodies are their own scopes, but assignment reaches outward: assigning a name that
already exists in an enclosing scope updates *that* variable. So a conditional update works,

```
m = a
if b > a
    m = b
m            # the larger of a, b
```

and so does a plain loop counter,

```
i = 0
while i < n
    i += 1
i            # n
```

A name that is new everywhere is local to the body and is not visible after it — so a variable you
want to read after an `if` must be introduced before the `if`. There is no shadowing in this
iteration: a name inside a body that matches an outer one always means the outer one. Reaching a
deliberately shadowed outer binding (a future `../var`) and `do` blocks are left for a later
scope-focused iteration.

Deferred: 
A basic match expression. It depends on ranges.

```
a = match fu
    1,7,13,42: val1
    6: val2
    200..220: val3
    _: default_val
```

[AI]

## Goal

Add `if` as an **expression** — the one-line ternary (`e1 if c else e2`) and the multi-line
block form — plus `while`/`loop` as **statements**. This is the codebase's first
*statement-level* branching and its first back-edge: until now the only basic blocks were the
intra-expression ones `and`/`or` short-circuit grew in 0005. It builds straight on the `Bool` from
0005 — every condition is type-checked as `Bool`, with **no truthiness** (`if x` on an `Int` is an
error; write `if x > 0`). `match` is deferred: it needs ranges, which arrive in 0008.

## `if` is one expression, two spellings

Both spellings lower to a single `Node::If`. An `if` **yields a value** — the last expression of
the taken branch, Rust-style — but that value is only *usable* when an `else` makes the choice
exhaustive. Without `else`, or when a branch does not end in an expression, the `if` has type
`Void` and may appear only in statement position (its value discarded), exactly as a call to a
`void` function already may. The one-line ternary is just a `Node::If` with single-expression
branches and a mandatory `else`.

- **Inline (ternary).** `e1 if c else e2`. Python's order/associativity: `c` chooses, `e1` is the
  then-value, `e2` the else-value. It is the loosest operator — binding power **10**, below `or`
  (20) — so `a or b if c else d` is `(a or b) if c else d`. The else-tail is right-associative:
  `a if p else b if q else c` is `a if p else (b if q else c)`. Like the comparison chain (0005),
  it is handled apart from the [`infix`](src/parser.rs#L220) table, in the `parse_expr` loop.
- **Block.**
  ```
  if cond1
      body1
  [elif cond2
      body2]*
  [else
      body3]
  ```
  `if`/`elif`/`else` headers share one column; each body is indented one level past them (the
  lexer's indent stack already enforces this). A block `if` may be a function's trailing result —
  `max` needs no ternary:
  ```
  f(a: int, b: int): int is
      if a > b
          a
      else
          b
  ```

## `while` / `loop` are statements

Both yield nothing (`Void`); they exist for the side effects of assignments in their bodies, so
they never serve as a function's result expression. Conditions are `Bool`.

```
while <cond>          loop
    <body>                <body>
                      while/until <cond>
```

The `while` form is pre-test; the `loop … while/until` form is post-test (do–while). `until <c>`
loops while `c` is **false** — it is `while not c` at the bottom. The trailing `while/until <cond>`
takes a single condition expression and no body. Per the human, the one-line loop spellings are
dropped.

## Layout

- **Inline continuation.** When a ternary wraps, the line carrying `else` must start at the column
  where `e1` starts:
  ```
  fu = 42 + 142 if a > 42
       else 42
  ```
  `parse_expr` records the start column of the expression it is building; after the condition, the
  ternary parser skips any `Newline`/`Indent`/`Dedent` and, if a newline was crossed, requires the
  `else` token's column to equal that start column (a `ParseError::Layout` otherwise). This reuses
  the column-tracking already in place for parameter alignment
  ([parser.rs:473](src/parser.rs#L473)). Because `e1` always begins to the right of the enclosing
  block's indent, the stray `Indent`/`Dedent` bracketing the `else` line never dedents the outer
  block.
- **Block bodies.** Indentation is the lexer's existing job; the parser only checks that `elif`/
  `else` reappear at the `if`'s column (which the matching `Dedent` guarantees).
- **Limitation.** A multi-line *block* `if` cannot be written inside parentheses: paren line-joining
  suppresses the `Newline`/`Indent` that `parse_block` needs. The inline value form is the ternary;
  a block `if`'s value reaches an expression by being bound first (`r = if … else …`, then use `r`).

## Data model

- `Node` gains `If { arms: Vec<IfArm>, els: Option<Block> }`, where
  `IfArm { cond: ExprId, body: Block }`. The leading `if` and each `elif` are uniform `IfArm`s; the
  inline ternary is one arm plus `els: Some(..)`.
- `Block.result` becomes `Option<ExprId>` ([parser.rs:118](src/parser.rs#L118)). A control-flow body
  may end in a statement (`None`); a function body still requires the trailing expression — the
  parser enforces `Some` there, keeping the existing "Function body must end with an expression."
  error ([parser.rs:537](src/parser.rs#L537)).
- `Stmt` gains `While { cond: ExprId, body: Block }` and
  `Loop { body: Block, cond: ExprId, until: bool }` ([parser.rs:104](src/parser.rs#L104)).
- No new `BinOp`/`UnOp`: `until` lowers to a negated branch in codegen, not a `not` node.

## Lexer

`if elif else while until loop` join the keyword classifier
([lexer.rs:206](src/lexer.rs#L206)) as `KwIf KwElif KwElse KwWhile KwUntil KwLoop`, becoming
reserved words (like `and`/`or`). No new operator lexemes.

## Parser

- **Prefix vs infix disambiguates the two `if`s.** `if` at the start of a statement or expression
  (`parse_primary`) is the block form; `if` after an operand (the `parse_expr` loop) is the ternary.
  `while`/`loop` are recognised in `parse_stmt` and never parse as expressions.
- **`parse_block() -> Block`** parses `Newline Indent <stmts> Dedent`, taking a trailing
  `Stmt::Expr` as the `result`. `parse_func_body`'s block arm is refactored onto it and additionally
  requires `result.is_some()`.
- A block `if` drives its own multi-line parse (consuming the internal `Newline`/`Indent`/`Dedent`),
  returning one `ExprId`, so it slots into the trailing-result and assignment paths unchanged.

## Sema

- **Conditions are `Bool`.** Each `cond` in an `if`/`while`/`loop` is checked; non-`Bool` is a
  `SemaError::Type`. No `Int`/`Float → Bool` (0005's no-truthiness rule).
- **`if` type.** Analyse every arm body and the `els`; the node's type is the **join** of the branch
  result types **iff** `els` is present and every branch ends in an expression — otherwise `Void`.
  `join` is the lattice max over `Bool → Int → Float` (`join(a,b)` = whichever of `a`,`b` the other
  is assignable to); a `Void` branch result has no join, so any missing result forces the whole `if`
  to `Void`. A `Void`-typed `if` used where a value is required then trips the existing void checks
  (`Cannot assign a void value.`, the function-return mismatch, etc.).
- **Scoping (lexical, mutate-outer; no shadowing in 0006).** Each `if`-branch and loop body pushes a
  scope frame onto the existing stack ([sema.rs:83](src/sema.rs#L83)). A bare-name **read** resolves
  *outward* through the enclosing frames to the current function's base frame (globals only via `::`;
  a top-level body, having no function frame, sees the top-level globals). A bare-name **assignment**
  *also* searches outward: if the name is already bound in any enclosing frame it **mutates that
  binding** (checked assignable), so conditional updates and loop counters/accumulators alter the
  outer variable — `if b > a / m = b` updates `m`; `while i < n / i += 1` drives a bare counter that
  terminates. Only a name bound *nowhere* in scope is **introduced in the innermost (body) frame**,
  where it is body-local and does **not escape**; reading it after the body is `UnknownSymbol`, so a
  value wanted after an `if` must be declared before it. There is **no shadowing** — a colliding name
  mutates the outer rather than masking it — so `../var` is unneeded here; intentional shadowing and
  `do` blocks are the deferred scope iteration. (Implementation note: `analyze_stmt`'s `Local` case
  changes from "check innermost only" to this outward search.)

## Codegen

- **`if`** lowers to a chain of diamonds — `arms[0]` branches into its body or into the lowering of
  `arms[1..]` + `els`, recursively. When the node's type is non-`Void`, each diamond contributes to a
  `phi` in the merge block (incoming from each branch's *end* block, tracked like
  [gen_short_circuit](src/codegen.rs#L586)); when `Void`, no `phi` — branches run for effect and
  merge. Branch values promote to the node's join type before the `phi`.
- **`while`**: a header block (evaluate `cond`, `condbr` to body or exit), a body block branching
  back to the header. **`loop … while/until`**: body first, then evaluate `cond` and `condbr` back to
  the body (for `until`, swap the branch targets) or to exit. First loops/back-edges in the codebase.
- **Locals move to entry-block allocas.** A pre-pass over each function body allocates one slot per
  local `SymId` up front, replacing the lazy first-assignment alloca in
  [resolve_place](src/codegen.rs#L325). This keeps every load dominated by its slot and stops an
  alloca from re-executing (and growing the stack) each loop iteration. One slot per `SymId` is
  enough: sema's mutate-outer means a name maps to a single binding, and sema rejects any read of a
  body-local after its scope, so codegen never loads an out-of-scope name. Assignment then just
  stores through the slot — an outer mutation and a bare loop counter both write across blocks with
  no SSA.

## Test plan (TDD: red → green → refactor)

- **lexer.rs**: `if elif else while until loop` classify as keywords (and stay reserved).
- **parser.rs**: ternary precedence (`a or b if c else d` = `(a or b) if c else d`) and right-assoc
  else-tail; the wrapped-`else` column rule (aligned parses, misaligned is `Layout`); block `if`
  shape (arms + optional `els`); a block `if` as a trailing function result; `while` and both
  `loop … while`/`loop … until` shapes; a body ending in a statement (`result: None`).
- **sema.rs**: non-`Bool` condition is an error; ternary/`if` type is the branch join; an `if`
  without `else` (or with a non-expression branch) is `Void` and unusable as a value; a bare
  assignment in a body **mutates** an existing outer binding (visible after); a name new to all
  scopes is body-local and reading it after the body is `UnknownSymbol`; a bare loop counter/
  accumulator type-checks.
- **codegen.rs / tests/**: `max` via block `if`; ternary value selection; a `while` counting loop and
  a `loop … until` do–while driven by a **bare counter** (observable result); a `max`-style
  conditional update of an outer variable observed after the `if`.

Test placement follows the convention: token/keyword tests in `lexer.rs`, AST-shape and layout in
`parser.rs`, type/scope errors in `sema.rs`, end-to-end execution in `tests/`. Messages stay
capitalised and end with a period.

## Resolved (sign-off)

1. **`if` is an expression in both spellings; `else` is required only to *use* the value.** One
   `Node::If`; missing `else` (or a non-expression branch) ⇒ `Void`, statement-position only. The
   ternary is sugar for the same node.
2. **Ternary is the loosest operator (bp 10, below `or`); else-tail right-associative.** Wrapped
   continuations must align `else` to `e1`'s start column.
3. **`while`/`loop` are `Void` statements;** `until c` ≡ loop while `not c` (a negated branch, no
   `not` node). One-line loop forms dropped per the human.
4. **Conditions are `Bool` — no truthiness** (carrying 0005's rule forward).
5. **Scoping — lexical, mutate-outer, no shadowing (signed off).** `if`-branch and loop bodies are
   nested scopes. A bare-name assignment mutates the nearest enclosing binding of that name
   (conditional updates and bare loop counters/accumulators alter the outer variable); a name new to
   every scope is body-local and does not escape (read-after-body is an error, so declare-before-`if`
   for a value you want afterward). No shadowing in 0006 — a colliding name mutates rather than masks
   — so intentional shadowing, `do` blocks, and `../var` are deferred to a dedicated scope iteration.
6. **`match` deferred to post-ranges (0008).**

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.