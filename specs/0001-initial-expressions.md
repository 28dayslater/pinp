# Iteration 0001 — initial expressions

[Human]

First iteration. Scope: **arithmetic expressions over typed symbols.** No functions (iteration 2),
no control flow, no LLVM yet. Development is unit-test driven.

pinp is **typed from the beginning**: every literal, variable, and sub-expression has a static
type (`int` or `float` in this iteration). Mixed `int`/`float` promotes to `float`; there is no
implicit `float → int`.

Integer and float literals may use `_` to separate **3-digit groups** — in the integer part, the
fractional part, and the exponent (e.g. `12_000_321`, `1_000.000_000e-1_000`).

```
a = 2 + 3 * 4          // a : int   = 14
b = 10 / 4             // b : float = 2.5   (/ is float division)
c = 10 div 4           // c : int   = 2     (div is integer division)
r = 7 mod 3            // r : int   = 1
x = 2.0 * a            // x : float          (int a promoted to float)
p = 2 ^ 10             // p : int            (^ is power, not xor)
g = 12_000_321         // g : int            (3-digit groups)
n = -p + (a - 1)       // grouping with parens; unary minus
```

`^` is exponentiation. `div` / `mod` are integer division / modulo. `=` introduces or rebinds a
symbol at the (single, global) scope that exists in this iteration; referencing a symbol before
it is assigned is an error.

[AI]

## In-scope grammar

Pratt expression parsing — precedence lives in the binding-power table below, not in nested
rules. Atoms and statement shapes:

```
program    = { statement }
statement  = assignment | expr-stmt
assignment = Ident "=" expression          // '=' is statement-level, NOT an expression operator
expr-stmt  = expression                     // bare expression; its value/type is what tests assert
primary    = Int | Float | Ident | "(" expression ")"
expression = primary combined per the precedence table (prefix/infix operators)
```

Statements are newline-separated. `Indent`/`Dedent` are emitted by the lexer but unused this
iteration (flat expressions only).

## Precedence table (iteration 1)

Binding power; higher binds tighter. Right operand of a right-assoc op is parsed at `bp − 1`.

| bp | operator            | fixity  | assoc | notes                                  |
|----|---------------------|---------|-------|----------------------------------------|
| 60 | `+` `-`             | infix   | left  | additive                               |
| 70 | `*` `/` `div` `mod` | infix   | left  | multiplicative, left-assoc (C/Python convention) |
| 75 | `-`                 | prefix  | —     | unary minus                            |
| 80 | `^`                 | infix   | right | power                                  |
| —  | `(` … `)`, literals, `Ident` | primary | — | tightest                          |

Unary minus sits below `^` (75 < 80), giving the math reading `-2^2 = -(2^2)`.

`mod` shares the multiplicative level (same BP as `* / div`, left-associative), matching C and
Python. Note: since `/` yields `float` and `mod` requires `int`, a left-to-right mix like
`12 / 5 mod 2` parses as `(12 / 5) mod 2` and is a type error (float `mod`) — parenthesise as
`12 / (5 mod 2)` for that meaning.

## Type system (iteration 1)

```
enum PinpType { Int, Float }     // grows to Bool, Str, … in later iterations
```

Literals: integer and float literals support `_`-separated 3-digit groups in the integer,
fractional, and exponent parts (handled and tested in the lexer). Hex/binary literals
(`0x…`, `0b…`) allow arbitrary `_` grouping and are `Int`.

Inference rules:
- `Int` literal (`42`, `0xFF`, `0b1010`, `16E10`, `12_000_321`) → `Int`; literal with a `.`
  (`3.14`, `.5`, `1_000.000_000e-1_000`) → `Float`.
- `+ - *`: both `Int` → `Int`; any `Float` → `Float`.
- `/`: always `Float` (operands promoted) — float division.
- `div`, `mod`: both operands must be `Int` → `Int`; a `Float` operand is a type error.
- `^`: both `Int` → `Int` (integer powers are non-negative; a negative integer exponent is an
  error, checked when evaluation lands); any `Float` → `Float`.
- unary `-`: preserves operand type.
- `Ident`: the type recorded when the symbol was assigned; unassigned use is an error.
- assignment `a = e`: `a` takes the type of `e`.

## Data model

- **Interner:** identifiers → `SymId(u32)`, backed by `&'src str` slices
  (`HashMap<&'src str, SymId>` + `Vec<&'src str>`) — zero source-text allocation.
- **AST arena:** `Vec<Node>` indexed by `ExprId(u32)` (no `Box`/`Rc`); children reference parents
  by `ExprId`.
- **Nodes (this iteration):** `Int(i64)`, `Float(f64)`, `Var(SymId)`, `Unary{op, ExprId}`,
  `BinOp{op, ExprId, ExprId}`.
- **Statements:** `Assign{target: SymId, rhs: ExprId}`, `ExprStmt(ExprId)`.
- Inferred `PinpType` is attached per node (parallel `Vec<PinpType>` indexed by `ExprId`, or a
  field on `Node` — decide during the refactor pass).

## Lexer

The `logos`-based lexer keeps its numeric and identifier regexes — including `_`-separated
3-digit grouping for decimal literals and arbitrary grouping for hex/binary. It adds a pass over
the token stream that:
- classifies `Identifier` against a keyword table → `KwDiv`, `KwMod` (and the future
  `and/or/not/xor/true/false/if/else/is/…`);
- emits `Newline`, `Indent`, `Dedent`, `Eof` (Indent/Dedent computed from the leading-space count
  but unused until blocks land);
- keeps `Token::text` a `&'src str` slice for the interner.

Operator token names: `Mul → Star`, `Div → Slash`; `Equal` stays `Equal`.

## Test plan (TDD: red → green → refactor)

Unit tests assert parsed AST shape **and inferred types**:
- precedence/associativity: `2 + 3 * 4`, `2 ^ 2 ^ 3` (right-assoc), `-2 ^ 2`, paren grouping;
- type inference: each rule above, including `int`/`float` promotion and grouped literals;
- type errors: `2.0 div 1`, use of an unassigned symbol;
- assignment then reference: `a = 2 + 3` then `a * a`.

## Resolved

- `mod` sits at the multiplicative level (same BP as `* / div`, left-associative — matches C and
  Python).

## Forward-looking operator framework (NOT in iteration 1)

For when comparisons/logicals/ternary land — tightest last, so these slot *below* additive:

| bp | operators            | fixity | assoc | result |
|----|----------------------|--------|-------|--------|
| 10 | `if … else` (ternary)| —      | right | branch type |
| 20 | `or`                 | infix  | left  | bool   |
| 25 | `xor`                | infix  | left  | bool   |
| 30 | `and`                | infix  | left  | bool   |
| 35 | `not`                | prefix | —     | bool   |
| 40 | `== !=`              | infix  | left  | bool   |
| 50 | `< > <= >=`          | infix  | left  | bool   |

`and`/`or`/`not` are short-circuit **logical** ops (bool operands, Python-like low precedence).
**Open:** you mentioned a logical-vs-"binary bool" distinction — pinp has no bitwise tokens yet
(`^` is power), so I need to know what the "binary bool ops" are (e.g. eager non-short-circuit
`&`/`|`, or bitwise on ints) and what tokens they get, before I can place them. Out of scope for
iteration 1.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
