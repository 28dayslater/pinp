[Human]

# Integer ranges

A range denotes a sequence of integers defined by a start, a stop, and an optional step. It is
written `start..stop`, optionally followed by `:step`.

The range operator fixes which bounds are included and the direction of traversal:

- `..` — both bounds are included; the direction follows from `start` and `stop`.
- `..<` — the range ascends and the stop bound is excluded.
- `..>` — the range descends and the stop bound is excluded.

For instance, a descending range that retains both bounds is expressed with `7..2` - that yields `7,6,5,4,3,2`.

`start`, `stop`, and `step` may each be given as an integer literal or an integer variable.

Where the step does not divide the span exactly, the range terminates at the last value reached before `stop` would be passed.

Examples:
```
a = 1..13
b = 1..17:2     // 1,3,5,7,9,11,13,15,17
c = 1..<17:2    // 1,3,5,7,9,11,13,15
d = 17..>1:-2   // 17,15,13,11,9,7,5,3
e = 1..20:3     // 1,4,7,10,13,16,19
f = 1..<19:3    // 1,4,7,10,13,16
g = 10..1:-1    // 10,9,8,7,6,5,4,3,2,1
```

A range is a first-class value and may appear wherever a value is expected, including among the
operands of a multi-assignment:
```
a, r, b = 42, 7..<100:10, 666
```

As the bounds and step may be variables, a range may be constructed from values established earlier:
```
a, b, step = 7, 100, 10
r = a..<b:step
```

Self-reference within a single multi-assignment is not supported due to technical reasons (right hand side is evaluated in full before assignment and the symbols `a`, `b`, `step` are not known at that instant):
```
a, b, step, r = 13, 133, 7, a..b:step // error
```

Where the bounds and step are literals, ill-formed ranges are rejected during semantic analysis:
```
r = 1..149:-13   // Error: The range ascends, but the step is negative.
r = 1..>149:13   // Error: An ascending range must use ..<.
r = 42..<1:5     // Error: A descending range must use ..>.
```

Where the bounds or step are variables, these checks cannot be performed ahead of run time. A range whose parameters are inconsistent at run time — for instance `start..<stop` with `start` at or beyond `stop` — yields an empty range.
No dedicated notation for an empty range is provided yet. This will be added later.1

A range is built from the values its bounds and step hold at that moment; the values are captured then, so a later change to a variable a range was built from does not reshape the range.

A step that is zero only at run time cannot be detected ahead of time. As such a range could never advance, building it is a runtime error, reported as `Range step cannot be zero.`, and execution stops rather than looping without end.

# Looping over a range

A range may drive a loop:
```
for idx in 1..20:3
    <body>
```

# Testing for membership

The expression `value in <range>` tests whether a value lies within the range and yields a boolean:
```
a, b, step = 1, 10, 2
fu = bar in a..<b:step   // true when bar is one of 1, 3, 5, 7, 9
```

Future: the use of ranges in `match`, and in the initialisation of one-dimensional arrays.

[AI]

## Goal

Add integer **ranges** as a first-class value written `start..stop[:step]` (`..` inclusive,
`..<`/`..>` half-open), the `for idx in <range>` loop — the language's first `for` — and the
`value in <range>` membership test. A range is the codebase's first **aggregate** value:
a `PinpType::Range` lowering to an LLVM struct `{ start, stop, step, inclusive }`. Iteration and
membership are emitted inline (no runtime library); the direction is taken from the sign of the
step, so a range whose bounds and step disagree at run time is simply empty.

## Surface and scope

- A range is built from `Int` parts (`Bool` promotes to `Int`, as elsewhere); a `Float` bound or
  step is a `Type` error — these are *integer* ranges.
- Ranges are stored in variables and consumed by `for`/`in`. There is **no `range` type name**, so
  a range cannot appear in a parameter or return annotation, and a program (or function body) does
  not *yield* a range to the host: `PinpValue` gains no `Range`, and a trailing range result is
  rejected in codegen. This mirrors arrays — storable and iterable now, surfaced later.

## Data model ([parser.rs](src/parser.rs))

- `PinpType` gains `Range` ([parser.rs:31](src/parser.rs#L31)).
- `Node` ([parser.rs:95](src/parser.rs#L95)) gains, with `enum RangeKind { Inclusive, UpExclusive, DownExclusive }`:
  ```rust
  Range { start: ExprId, stop: ExprId, step: Option<ExprId>, kind: RangeKind },
  Membership { value: ExprId, range: ExprId },
  ```
- `Stmt` gains `For { var: SymId, range: ExprId, body: Block }` ([parser.rs:146](src/parser.rs#L146)) —
  a `Void` statement, like `While`.

## Lexer ([lexer.rs](src/lexer.rs))

- Three new lexemes/`TokenKind`s: `DotDot` (`..`), `DotDotLt` (`..<`), `DotDotGt` (`..>`). Logos
  longest-match makes `..<`/`..>` win over `..`; `..` competes with the `Float` regex only where a
  digit follows the dot, so `.5` stays a `Float` while `1..5` is `Int DotDot Int`
  ([lexer.rs:33](src/lexer.rs#L33)).
- `for`/`in` join the keyword classifier ([lexer.rs:212](src/lexer.rs#L212)) as `KwFor`/`KwIn`.
- The step separator reuses the existing `Colon`; `:-2` is `Colon Minus Int`.

## Parser ([parser.rs](src/parser.rs))

- **Range and membership are operators in the `parse_expr` loop** ([parser.rs:984](src/parser.rs#L984)),
  handled beside the comparison band rather than as `infix` rows (their results are not ordinary
  `Bin`s):
  - `RANGE_BP = 50` — above the comparison band (45), below additive (60), so `a+1..b-1` is
    `(a+1)..(b-1)`. On a range token, `parse_range_tail` parses the stop at `RANGE_BP + 1`
    (non-associative: `1..2..3` builds a `Range` whose start is a `Range`, later a sema `Type`
    error), then, if a `Colon` immediately follows, the step at `RANGE_BP + 1`, and pushes
    `Node::Range`.
  - `MEMBERSHIP_BP = 45` — on `KwIn`, parse the range operand at `MEMBERSHIP_BP + 1` and push
    `Node::Membership` (`Bool`, so it composes with `and`/`or`).1
  - No comma form survives, so a range never introduces a comma: `parse_expr_list` (0007) is
    untouched and the multi-assignment grammar stays context-free —
    `a, r, b = 42, 7..<100:10, 666` parses because `..`/`:` bind tighter than the list comma.
- **`for` is a statement.** `parse_stmt` ([parser.rs:839](src/parser.rs#L839)) dispatches `KwFor` to
  `parse_for` (beside `KwWhile`/`KwLoop`): consume `for`, an `Identifier` (interned as `var`), `in`,
  then `parse_expr(0)` for the range and `parse_block` for the body. The loop consumes its own `in`,
  so it never reaches the membership operator.
- Whitespace around `..`/`:` is not enforced; the no-space form is the conventional style, and the
  operator parse removes the comma ambiguity that the whitespace rule once guarded against.

## Sema ([sema.rs](src/sema.rs))

- `assignable`/`join` ([sema.rs:32](src/sema.rs#L32)): `Range` is assignable only to `Range` (no
  widening); `join` of `Range` with anything else is `None`.
- **`Node::Range`** ([sema.rs:267](src/sema.rs#L267)): `start`, `stop`, and `step` (if present) must
  be `int_like`; a `Float`/`Void` part is a `Type` error. The node's type is `Range`. **Literal
  validation** runs only where the parts are `Node::Int` constants (variable parts are a run-time
  concern, per the spec):
  - step `0` → `Range step cannot be zero.` (a *variable* step that is zero only at run time can't be
    caught here; it becomes a runtime error — see codegen).
  - `Inclusive`: a literal step whose sign disagrees with `stop - start` →
    `The range ascends, but the step is negative.` / `The range descends, but the step is positive.`
  - `UpExclusive`: literal `start >= stop` → `A descending range must use ..>.`
  - `DownExclusive`: literal `start <= stop` → `An ascending range must use ..<.`
- **`Node::Membership`**: `value` must be `int_like`, `range` must be `Range`; result `Bool`.
- **`Stmt::For`**: `range` must be `Range`. The body is analysed in a pushed frame
  ([sema.rs:221](src/sema.rs#L221)) seeded with `var: Int`; `var` is **read-only** — assigning it is
  `Cannot assign to loop variable.` (a `loop_vars` set checked in `assign_place`
  [sema.rs:199](src/sema.rs#L199)). It does not escape the body (0006 scoping). The `for` is `Void`.

## Codegen ([codegen.rs](src/codegen.rs))

- **`PinpType::Range` → struct** `{ i64 start, i64 stop, i64 step, i1 inclusive }`. `basic_type`
  ([codegen.rs:236](src/codegen.rs#L236)) returns it; `zero` ([codegen.rs:930](src/codegen.rs#L930))
  returns its const-zero (a range global declares cleanly); `promote` leaves a `Range` unchanged.
- **`Node::Range`** evaluates `start`/`stop` as `i64` (`as_int`) and the step: given → `as_int`;
  omitted → `1` (`UpExclusive`), `-1` (`DownExclusive`), or `select(stop sge start, 1, -1)`
  (`Inclusive`, taking direction from the bounds). The fields are **snapshotted by value** into the
  struct, so a range freezes its parameters at the moment it is built — later mutation of the source
  variables does not reshape it. A **zero step** here (only reachable with a variable step — a literal
  zero is a sema error) raises a **runtime error** (below) rather than building a never-advancing
  range; so every range value that exists has a non-zero step, which the `for`/`in` lowerings rely on.
  It then builds the struct value, `inclusive = true` only for `..`.
- **`Stmt::For`** evaluates the range, extracts the four fields, allocates the `i64` counter in the
  entry block, stores `start`, and emits a `while`-shaped header/body/exit
  ([codegen.rs:417](src/codegen.rs#L417)). The header recomputes the direction-aware continue test:
  `going_up = step > 0`; `up = select(inclusive, counter <= stop, counter < stop)`;
  `down = select(inclusive, counter >= stop, counter > stop)`; `cond = select(going_up, up, down)`.
  The body runs in a pushed scope binding `var` to the counter slot; the latch does `counter += step`,
  stores, and branches back. An empty range fails the header immediately (body runs zero times).
- **`Node::Membership`** evaluates `value` as `i64` and the range fields, then the closed form:
  `on_step = (value - start) srem step == 0` (the step is non-zero by construction, so the `srem` is
  well-defined); `in_bounds` is the direction-aware bounds test (`going_up`: `value >= start` and
  `value </<= stop`; else mirrored); `member = on_step and in_bounds` — an `i1`. No branches, no
  runtime calls.
- A program/function **result of type `Range`** is rejected in `gen_entry`
  ([codegen.rs:350](src/codegen.rs#L350)); `run`/`PinpValue` are unchanged.

### Runtime errors

pinp has no runtime library or unwinding, so a runtime error is reported out-of-band rather than as
an exception or a trap (a bare `llvm.trap` would `SIGILL` with no message). The mechanism, currently
used only for the zero-step range:

- An external-linkage module global `__pinp_runtime_error: i64` (declared up front, `0` = no error)
  carries an error **code** to the host.
- `raise_runtime_error(code)` stores the code, then **returns from the current function** with a
  throwaway zero value: with no unwinding, execution falls back out to the host instead of producing
  a real result. (A range built in the entry function — the common case — returns immediately, so a
  zero-step loop is never entered.)
- After the entry function returns, [`PinpJit::run`] reads the global (via a symbol lookup) and, if
  non-zero, returns `Err(runtime_error_message(code))` instead of the value. So `eval` of a zero-step
  range yields `Err("Range step cannot be zero.")`.
- Codes live in one place (`RUNTIME_ERROR_ZERO_STEP = 1`, `runtime_error_message`), ready for the
  future array-bounds / `match` errors to reuse.

> The inline-IR range checks and this error-global mechanism are a deliberate *path-of-least-resistance*
> baseline. A later iteration is planned to introduce a proper runtime library — routines authored in
> C/Rust, compiled to LLVM bitcode and linked into the JIT so they inline — which will host the
> validity/membership checks (alongside lower-level libraries). The code here is expected to be
> replaced by that.

## Test plan (TDD: red → green → refactor)

- **lexer.rs**: `.. ..< ..>` classify (longest-match over `..`); `for`/`in` are keywords; `.5` is
  still `Float` but `1..5` is `Int DotDot Int`; `1..10:2` adds `Colon Int`.
- **parser.rs**: `1..10` → `Range { kind: Inclusive, step: None }`; `1..<10`/`5..>1` set the kind;
  `1..10:2` attaches the step; `1+1..2*2` brackets the bounds; `a, r, b = 1, 2..8:2, 3` puts a
  `Range` in the middle value; `for idx in r` → `Stmt::For`; `x in 1..9` → `Node::Membership`.
- **sema.rs**: a range types as `Range`; `1.0..3` is a `Type` error; the four literal-validation
  messages fire; a literal zero step is rejected even with **variable** bounds (`a..b:0`); `for idx in
  1..3` binds `idx: Int` and reassigning it errors; `for` over a non-range errors; `x in 1..9` types
  `Bool`, `1.5 in 1..9` errors.
- **tests/ranges.rs**: `for` sums of `1..5` (15) and `1..<5` (10); a stepped `1..10:3`; a descending
  `5..1:-1` and `5..>1`; membership true/false for literal and **variable** step; a first-class range
  bound to a variable then iterated; a range **freezes** its bounds (mutating the source after
  construction does not reshape it); a **variable** range with `start >= stop` iterating zero times
  and its membership false; a **variable zero step** yields `Err("Range step cannot be zero.")`.

## Resolved (sign-off)

1. **`start..stop[:step]`, three kinds** (`..` inclusive, `..<`/`..>` half-open); direction from the
   step's sign (omitted-step `..` from the bounds). No comma form.
2. **First-class `PinpType::Range`** as a `{start, stop, step, inclusive}` struct — storable and
   iterable, but not annotatable or returned to the host (no range type name; `PinpValue` unchanged).
3. **`for idx in <range>`** is a `Void` statement; `idx` is `Int`, read-only, non-escaping.
4. **`value in <range>`** is closed-form inline arithmetic yielding `Bool`, for every form including
   a variable step.
5. **Int-only** parts; a range **freezes** its bounds/step by value at construction. Literal ranges
   are validated by sema. A variable range that is direction-inconsistent at run time (e.g. `..<`
   with `start >= stop`) is **empty**; a variable **zero step** — which can never advance — is a
   **runtime error** (`Err("Range step cannot be zero.")`), reported via an out-of-band error global.
6. **Range/membership are `parse_expr`-loop operators** (`RANGE_BP = 50`, `MEMBERSHIP_BP = 45`); the
   0007 comma grammar is untouched.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
