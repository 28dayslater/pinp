[Human]

# Multiple variable definition and assignment in a single statement

Several variables ca be defined/assigned at once, similar to Python, but there is a difference
when the statement flows into the next line - see below.

```
a, b = 1, 2      # parallel: a=1, b=2
a, b = b, a      # swap (right-hand side evaluated before any assignment)
a = b = c = 0    # chained: a, b and c all become 0
a, b = c, d = 1, 2   # both: (a,b) and (c,d) all become (1,2)
```

Targets are plain names (or `::globals`); pinp has no tuples, so this is positional, not unpacking.

If the multiple assignment flows to the next line (or lines in a degenerate case),
the following lines RHS values must start at the column of the first value/expresion.
If the LHS list breaks into the next line, the next line must have the same indent as the line above.

```
some_variable_1, some_variable2, some_variable3 = 123455686453, "Mary had a little lamb...",
                                                  <long-expression>

fu_bar_baz() is
    var1, var2, var3, var4,
    var5, var6 = <expr1>, <expr2>, <expr3>,
                 <expr4>, <expr5>
```

If the LHS symbol list breaks into the next line, comma must be in the current line,
not start the next one

```
fu_bar_baz_bar() is
    var1, var2, var3, var4
    , var5, var6 = <expr1>, <expr2>, <expr3>,  // <- error "Multi-line assignment: a line must not start with a comma."
                   <expr4>, <expr5>
```

TODO: indentation style must be explained in language docs (future).

[AI]

## Goal

Generalise the assignment statement so one `=`-driven line can bind several targets at once, matching
Python's `target_list = … = expr_list` grammar — covering both **parallel** (`a, b = 1, 2`) and
**chained** (`a = b = 0`) forms, and their combination. The right-hand side is evaluated **in full
before any store**, so `a, b = b, a` swaps. Single assignment (`a = 1`) becomes the degenerate case
of the same construct. Compound assignment (`+=` …) stays single-target, single-value.

## Grammar

```
assignment ::= target_list "=" ( target_list "=" )* expr_list
target_list ::= place ( "," place )*
expr_list   ::= expr ( "," expr )*
place       ::= name | "::" name
```

- Every `target_list` must have the **same arity** as the final `expr_list` — `a, b = 1` and
  `a, b = c = 1, 2` are arity errors. (A *chained* group is a `target_list` of arity 1, so
  `a = b = 1, 2` is also an error: arity 1 ≠ 2.)
- The trailing group is the values; every earlier group is a set of targets.
- A line that is a single group with **no** `=` is an ordinary expression statement (so a bare
  `a, b` — a comma list with no `=` — is a syntax error, as it is not an expression).

## Line continuation (layout)

A comma-list — the LHS targets or the RHS values — may wrap onto further lines. This is purely a
parsing concern: the resulting `Assign` is identical, so the **data model, sema, and codegen are
unchanged**.

- **Trigger — a trailing comma.** A `,` as the *last token on a line* continues its list onto the
  next line; the intervening `Newline` is ignored. With no trailing comma, a line ends the statement
  as usual.
- **Alignment — one rule.** A continuation's first item must start at the **column where its list
  began**: the first target's column for the LHS (which is the statement's indent), the first
  value's column for the RHS. Otherwise `ParseError::Layout`. This is the same column rule already
  used for parameter lists and the wrapped-conditional `else` (0006).
- **A comma may not start a line.** A list that breaks without a trailing comma, whose next line
  *begins* with a comma, is rejected with `"Multi-line assignment: comma must not start a line."`
  (the comma must trail the current line, not lead the next).
- **A trailing comma with no continuation value is an error** — an aligned next item is required.

**Layout reuse.** When an RHS continuation aligns to a column *past* the enclosing block's indent
(values stacked under the first value), the lexer opens an `Indent` it later closes with a `Dedent`;
that stray `Dedent` is swallowed by the existing `pending_dedents` mechanism (from the 0006 wrapped
`else`), so nothing new is needed in `parse_block`. An LHS continuation sits at the *same* indent, so
it is merely a suppressed `Newline`.

## Semantics

- **Evaluate the `expr_list` once, left to right, into N values, before any store.** This gives the
  swap; chained `a = b = e` evaluates `e` a single time.
- **Assign those N values to each `target_list`, left to right** (across groups, and within a group).
  Aliased targets follow last-write-wins (`a, a = 1, 2` leaves `a == 2`), as in Python.
- Each `(place, value)` pair obeys the existing single-assignment rules: mutate the nearest enclosing
  binding if the name is already in scope, otherwise introduce a non-escaping local (0006 scoping);
  the value must be assignable to an existing slot's type; a `Void` value is rejected.
- A trailing assignment statement still yields a value for the program result (as today): the value
  bound to the **last** position (`values.last()`), so `a = 5` is unchanged.

## Data model

`Stmt::Assign` generalises ([parser.rs](src/parser.rs)):

```rust
Assign {
    target_lists: Vec<Vec<Place>>, // one inner Vec per `=`-separated target group
    values: Vec<ExprId>,           // the trailing expr_list, evaluated once
}
```

Single `a = 1` is `target_lists: [[Local(a)]], values: [rhs]`; compound `a += 1` desugars as before to
the single-target, single-value shape with a `Bin` value. The nesting is what distinguishes parallel
(`a, b = …` → one inner Vec of two) from chained (`a = b = …` → two inner Vecs of one) — a flat list
could not tell `a, b = 1` (arity error) from `a = b = 1` (chained).

## Parser

`parse_stmt` parses assignment as the Python grammar does — a chain of comma-lists separated by `=`:

- Parse a comma-separated `expr_list` (`parse_expr_list`, reusing `parse_expr`). Then:
  - **`=` follows** → keep consuming `"=" expr_list` groups. The last group is `values`; each earlier
    group is converted to a `target_list` — every element must be a `Node::Var`/`Node::Global` (else
    `ParseError::Unexpected("Invalid assignment target …")`), mapped to `Place::Local`/`Place::Global`.
    Validate the arity rule, then build `Assign`.
  - **a compound assign op follows** a single-element first group → the existing single-target compound
    desugar (`finish_assign`), unchanged.
  - **end of statement** → the first group must be a single expression → `Stmt::Expr`; a multi-element
    group with no `=` is a syntax error.
- This replaces the current 1–2 token `Identifier =`/`:: Identifier =` look-ahead. Parsing targets as
  expressions and converting to `Place` keeps `::global` and multi-target handling uniform; the read
  `Var`/`Global` nodes produced for targets are left unused in the arena (harmless).
- **Line continuation lives in `parse_expr_list`.** It records the column of its first item; after
  consuming a `,`, if the next token is a `Newline` the list continues — it skips the continuation's
  layout tokens (recording any `Indent` in `pending_dedents`), requires the next item at the recorded
  column (else `ParseError::Layout`), and parses on. The LHS and RHS lists both wrap through this one
  path; a trailing comma whose continuation yields no aligned item is the trailing-comma error.
- **The comma-starts-a-line diagnostic.** In the "no `=`" branch, when a comma-list ended at a
  `Newline`, `parse_stmt` peeks past it; a leading comma there reports `"Multi-line assignment: comma
  must not start a line."` rather than the generic bare-list error.

## Sema

`analyze_stmt`'s `Assign` arm loops the generalised shape:

- Infer each value's type (rejecting `Void`, as today).
- For each `target_list`, zip it with the values and apply the existing per-place check
  (`Place::Global` must already exist; `Place::Local` mutates an enclosing binding via
  `lookup_assign_target` or introduces a new local), reusing `check_assignable`.
- No new error kinds; arity is already guaranteed by the parser.

## Codegen

`gen_stmt`'s `Assign` arm:

- Evaluate every value into an SSA value **first** (`expect_value` left to right) — this is what makes
  the swap correct, no temporaries needed beyond the SSA values themselves.
- Then, for each `target_list`, `resolve_place` each target and `build_store` the promoted value into
  its slot (entry-block alloca model from 0006).
- Return `Some(last value)` for the program-result path.

## Test plan (TDD: red → green → refactor)

- **parser.rs**: `a, b = 1, 2` → `Assign` with `target_lists [[a,b]]`, `values [1,2]`; `a = b = 0` →
  `target_lists [[a],[b]]`, `values [0]`; combined `a, b = c, d = 1, 2`; arity mismatch (`a, b = 1`)
  and non-place target (`1 = 2`, `a + b = 1`) are errors; a bare `a, b` (no `=`) is an error; single
  `a = 1` still parses as the degenerate `Assign`.
- **sema.rs**: parallel targets get the value types; new names are introduced, existing ones mutate
  (type-checked); a `Void` value is rejected; chained `a = b = e` types both from `e`.
- **codegen.rs / tests/**: `a, b = 1, 2` then `a + b` → 3; **swap** `a, b = b, a` observed; chained
  `a = b = 5` then `a + b` → 10; a parallel define inside a function body used afterwards.
- **parser.rs (continuation)**: an RHS wrapped onto an aligned next line parses; an LHS wrapped via a
  trailing comma parses; a continued assignment inside a function body parses (exercising the
  `pending_dedents` path for the RHS `Indent`); a misaligned continuation is a `Layout` error; a
  comma starting the next line gives the specific "comma must not start a line" error; a trailing
  comma with no continuation value is an error.
- **tests/ (continuation)**: a wrapped multi-assign evaluates to the same result as its one-line form.

## Resolved (sign-off)

1. **Both forms, unified under Python's grammar** (`(target_list =)+ expr_list`), and they compose.
2. **RHS fully evaluated before any store** (swap works); chained RHS evaluated once.
3. **Targets are names/`::globals` only** (no tuples/indexing yet); positional, not unpacking.
4. **Compound assignment stays single-target/single-value**; only plain `=` chains/parallelises.
5. **`Stmt::Assign` generalises** to `target_lists: Vec<Vec<Place>>` + `values: Vec<ExprId>`; single
   assignment is the degenerate `[[place]] = [value]`.
6. **Line continuation** is triggered by a **trailing comma**; a continuation aligns to the column
   where its list began (one rule for LHS and RHS); a comma must not start a line; a trailing comma
   with no value is an error. It is purely syntactic — it reuses the 0006 column-alignment and
   `pending_dedents` machinery and leaves the AST, sema, and codegen unchanged.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
