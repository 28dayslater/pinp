[Human+AI]

# Diagnostics unification — one rendering path for every layer

This iteration is an internal rework and does not add any language features.

[0015](0015-static-analyses-start.md) introduced a structured `Diagnostic` — stable code, severity,
span, message, notes — but scoped it to the analysis layer. The three older layers never got it:

```rust
pub struct LexError { pub message: String, pub line: u32, pub col: u32 }

pub enum ParseError {
    Lex(String), Unexpected(String), Layout(String),
    TooDeeplyNested(String), TooManyArms(String),
}

pub enum SemaError { UnknownSymbol(String), Type(String) }
```

Every `ParseError` and `SemaError` variant is a bare `String`. No span, no line, no column, and no
`Display` impl anywhere in `src/` — nothing renders either type. [parser/mod.rs:59](../src/parser/mod.rs#L59)
compounds it by discarding position the lexer already computed:

```rust
let tokens = lex(src).map_err(|error| ParseError::Lex(error.message))?;
```

The C++ sibling attaches a `SourceLocation` to *every* diagnostic and routes all of them through one
formatter. pinp currently does that for warnings only. This iteration closes the gap.

## Scope of this iteration (0016)

In scope:
- **Promote `diagnostic.rs`** from `src/analysis/` to `src/` — it is shared infrastructure, and its
  only dependency is `crate::lexer::{LineIndex, Span}`.
- **Fill the AST span arena** for every expression node, retiring 0015's compromise 1. Without
  this the rest of the iteration does nothing for sema; see below.
- **A span on every error**: add `Span` to each `ParseError` and `SemaError` variant, and carry
  `LexError`'s position instead of dropping it.
- **One formatter**: a `to_diagnostic()` on each error type, so all four layers render through the
  same `line:col: severity[CODE]: message` path.
- **Column accuracy tests** — the reason to do this at all is that an error points somewhere useful.

Out of scope:
- Any change to error *classification*. The variants stay as they are.
- Multi-error recovery. See Constraints.
- **`Token::line` / `Token::col` stay exactly as they are.** See below — this is the one place the
  swap must not spread to.

### `Token` keeps its eager line/col

`LexError` swaps to `Span`; `Token` does not, and the distinction is not an inconsistency to tidy up
later. pinp's layout rules *compare columns as a language semantic* — hung-parameter alignment
([item.rs:161-175](../src/parser/item.rs#L161-L175)), wrapped-conditional alignment
([expr.rs:259-262](../src/parser/expr.rs#L259-L262)), list-element alignment
([item.rs:596-599](../src/parser/item.rs#L596-L599)). Those sites need a resolved column *during
parsing*, at points with no `LineIndex` in scope, and they are doing arithmetic rather than
reporting a position. Converting them to `Span` would mean resolving offsets back to columns on a
hot path to answer a question the token already answers.

Diagnostics resolve late; the layout engine resolves eagerly. Both are correct for their job.

## Constraints

- **No behaviour change.** Same errors, same conditions, same messages. Only their type gains a
  span and their rendering becomes uniform. The existing suite must stay green.
- **One deliberate API break**, and only one: `LexError`'s `line`/`col` become `span`. It has 9
  reference sites across `src/` and `tests/`, two of which are assertions
  ([lexer.rs:879](../src/lexer.rs#L879), [lexer.rs:931](../src/lexer.rs#L931)) that get rewritten to
  compare a `Span` or to resolve it through `LineIndex`.
- **Errors stay fail-fast.** The [fail-fast rule](0004-initial-sema.md) is unchanged: the first
  error still stops the pass. This iteration adds *positions*, not batching.
- **Message text is unchanged.** Messages stay capitalised and end with a period per
  [CLAUDE.md](../CLAUDE.md); no message is reworded, so no test asserting on text needs touching.

## Key decision: unify the rendering, not the error types

The obvious move — replace `ParseError`/`SemaError` with `Diagnostic` outright — is rejected. Three
reasons, recorded here because the choice should be deliberate:

1. **`DiagnosticCode` is a permanent namespace.** Its contract is that a code is never reused, which
   is right for advisory rules a suppression pragma keys on. Parse errors are dozens of distinct
   messages that nobody will ever suppress; minting and retiring stable numbers for them is
   obligation without payoff.
2. **The variants are load-bearing.** `Layout`, `TooDeeplyNested`, and `TooManyArms` let tests assert
   *which kind* of failure occurred without matching message strings. Flattening pushes ~344
   reference sites across `src/` and `tests/` onto string matching — strictly worse.
3. **The type advertises the policy.** A distinct fail-fast error type next to a batched
   `Vec<Diagnostic>` is the distinction [analysis/mod.rs](../src/analysis/mod.rs#L15-L16) exists to make.

So `Diagnostic` becomes the **presentation** type, which is what the C++ `TextDiagConsumer` actually
is — a formatter, while `DiagnosticsEngine::error()` still throws a plain `runtime_error`. Error
enums keep their identity and gain `fn to_diagnostic(&self) -> Diagnostic`.

Consequence: `Diagnostic::code` becomes optional. Analysis findings carry a `DiagnosticCode`;
lexer/parser/sema errors carry `None` and render as plain `line:col: error: message`. If parse and
sema bands (`PINP2xxx`/`PINP3xxx`) are wanted later, they slot in without reshaping anything.

## Positions are resolved late, so there is no `Display`

An error must render with its position — that is what every production compiler does and the point
of the iteration. But pinp stores a `Span` (byte offsets), and turning offsets into `line:col`
requires a `LineIndex`, which requires the source. `std::fmt::Display` takes only `&self`, so a
`Display` impl *cannot* print a position.

This is a real divergence from the C++ sibling, which sidesteps the problem by resolving eagerly:
`SourceLocation` holds `line` and `col` as integers, computed by the lexer at construction. pinp
resolves lazily instead, which is the better choice — `Span` composes (a parser can widen one to
cover a construct), survives being stored in an arena, and costs nothing until something renders.

So:

- **`render(&line_index)` is the only rendering path**, matching `Diagnostic::render`.
- **No `Display` impl on any error type.** A message-only `Display` would be a trap: it compiles,
  reads fine at the call site, and silently drops the position — reintroducing exactly the defect
  this iteration removes. Better that `{}` fails to compile and forces the caller to reach for
  `render`.
- `Debug` stays derived, so `unwrap()` in tests still prints something useful.

Every entry point that surfaces an error to a user — `PinpJit`, the future runner — already has the
source in hand, so none of them are inconvenienced by needing a `LineIndex`.

## Retiring 0015's compromise 1: spans on every expression node

[0015](0015-static-analyses-start.md) added the `spans` arena but filled it only for `Node::Var` and
`Node::Global` — the two kinds its checkers report on. Everything else holds `Span::UNKNOWN`, and
`ProgramAst::push` still defaults to it ([ast.rs:415](../src/parser/ast.rs#L415)).

That compromise is load-bearing against this iteration. `SemaError::Type` is the overwhelmingly
common sema error — 127 of the 165 wildcard test patterns — and it fires on binary operators, calls,
indexes, members, and literals. Not one of those has a span. Giving `SemaError` a span field while
the arena stays empty would render `error: Cannot assign Float to array element of type Int.` with
no position: structurally improved, practically unchanged.

So 0016 converts the parser's `push` sites to `push_spanned`. Roughly 45 sites; most take a token's
span directly, and composite nodes take `left.start .. right.end` so a binary expression covers its
whole extent rather than just its operator. `push` stays for genuinely synthetic nodes that
correspond to no source text.

This is scope growth over the first draft, taken deliberately: it is the difference between the
iteration delivering positions for one layer and for all three.

Related: `ast.rs` points at "KI in dev-docs" for this, but the ledger has no such row — it holds
only its header. Nothing to delete, then; the comment goes when the arena is filled.

## Error shape: a `kind` + `span` struct, not a span per variant

Adding a span cannot be done without touching test call sites; the only question is how many. The
current usage, counted:

| | sites |
|---|---|
| Construction in `src/` | 144 |
| Wildcard patterns in tests — `SemaError::Type(_)` and friends | 165 |
| Literal constructions in tests — all `SemaError::Type("…".into())` | 38 |

A trailing field on each tuple variant (`Type(String, Span)`) is the obvious move and the worse one:
it breaks all 38 literal constructions into needing an explicit span argument, repeats the field
across every variant, and invites tests to hardcode byte offsets.

Instead, hoist the span out of the variants:

```rust
pub struct SemaError { pub kind: SemaErrorKind, pub span: Span }
pub enum SemaErrorKind { UnknownSymbol(String), Type(String) }
```

with `impl PartialEq<SemaErrorKind> for SemaError` comparing the kind alone. Then:

- The 38 literal constructions become a type rename — `SemaError::Type("…")` →
  `SemaErrorKind::Type("…")` — and keep working under `assert_eq!` with no span mentioned.
- The 165 wildcard patterns become `matches!(error.kind, SemaErrorKind::Type(_))`.
- No test ever writes a byte offset. Position is asserted only in the new, targeted span tests.

Both rewrites are mechanical. `ParseError` gets the same treatment.

## Steps

0. Fill the span arena: convert the parser's `push` sites to `push_spanned`, composite nodes
   covering their full extent. Retires 0015's compromise 1 and unblocks step 4.
1. Move `src/analysis/diagnostic.rs` → `src/diagnostic.rs`; `pub use` shim in `analysis/` to keep
   the churn local. Make `code` an `Option<DiagnosticCode>` and adjust rendering.
2. `LexError` — **replace** `line: u32, col: u32` with `span: Span`. A breaking change to public
   API, taken deliberately: it makes every layer speak one position type, and the lexer already
   knows the offsets it currently resolves eagerly. Stop the drop at parser/mod.rs:59.
3. `ParseError` → `ParseError { kind: ParseErrorKind, span }`; thread the span from the offending
   token at each of the construction sites, and mechanically rewrite the test patterns.
4. `SemaError` → `SemaError { kind: SemaErrorKind, span }`, span taken from the node being checked.
5. `to_diagnostic()` for all three, and `render(&line_index)` on each. No `Display`.
6. README / module-doc touch-up.

Steps 3 and 4 are the bulk; they are mechanical but wide.

## Tests

Per [CLAUDE.md](../CLAUDE.md), tests come first and adversarially. The contract worth attacking:

- **Position accuracy** — the span points at the offending token, not the statement start, not the
  line start. One case per `ParseError` and `SemaError` variant, on multi-line input with leading
  indentation so an off-by-one in column arithmetic cannot hide.
- **Round trip** — `to_diagnostic()` preserves message text exactly; severity is `Error` for all
  three older layers; `code` is `None`.
- **`LexError`'s span resolves to its old line/col.** The two existing assertions are the oracle:
  whatever `(line, col)` they assert today, `line_index.locate(error.span.start)` must still return.
  That pins the swap as lossless rather than merely compiling.
- **Layout errors still point at the column the rule is about.** The alignment rules compare
  `Token::col`; the diagnostic must report *that* column, not the statement start — the one place
  where the eager and lazy position paths have to agree.
- **Unknown spans** still render without a position, as [diagnostic.rs](../src/diagnostic.rs)
  already guarantees for warnings.
- **Tab and multi-byte input** — columns are counted consistently with what `LineIndex` already does
  for the analysis layer; a diagnostic on a line containing non-ASCII must agree with it.
- **No regression** — every existing test still asserts the same failure for the same input. The
  patterns change shape (see below); not one of them should change *meaning*.

## Implementation notes — where the design moved

Eight things settled differently from the plan above, each because the code said so:

1. **`push`/`push_spanned` collapsed into one `push(node, span)`.** Once every site passed a span,
   the unspanned variant was dead and the `_spanned` qualifier distinguished nothing.

2. **Parser spans are attached centrally, not per site.** The 42 construction sites return a
   `ParseErrorKind`; [`parse`](../src/parser/mod.rs) wraps it with the span. The parser unwinds the
   moment a rule fails and never advances afterwards, so the token it stopped on *is* the offending
   one — which makes 42 hand-threaded spans unnecessary and, being hand-threaded, less uniform.

3. **`error_span` skips synthetic layout tokens.** A rule can object while the cursor sits on a
   `Newline`/`Indent`/`Dedent`, which occupy no source; reporting one gives a zero-width position at
   the end of the previous line. Caught by `a_comma_starting_a_line_points_at_the_comma`, which
   reported `1:5 ""` instead of `2:1 ","`. The span now skips forward to the next token that
   occupies source.

4. **Sema positions come from a save/restore in `analyze_expr`.** The caller's `current_span` is
   restored **on success only**, so once a child is analysed the position is back on the parent and
   a rule failing after inspecting its operands points at the whole expression. On failure the
   innermost node's position stays and propagates. Without this, `'a' - 2` would report on `2`.

5. **A `Diagnose` trait, not three inherent impls.** `to_diagnostic` plus `render` on each of the
   three error types meant the same one-line `render` body copied three times. The trait carries
   `render` as a default method, so an implementor supplies only `to_diagnostic`, and the "why there
   is no `Display`" rationale lives in one place.

6. **Statement-level sema rules name what they blame.** Note 4's save/restore positions everything
   reached through `analyze_expr`, but a rule that fires from a *statement* runs after its operands
   have been typed, and each of those restored the caller's position on the way out — leaving the
   position of whatever enclosed the statement, which for a statement is nothing. Eight of the
   thirteen sema errors probed rendered with no position at all, including
   `Cannot assign Float to array element of type Int.` — the very message quoted above as the
   motivation for filling the span arena. So `error_at` / `error_at_expr` set the position at the
   point of failure, and each rule chooses its subject: the **value** for an assignment mismatch,
   the **index** for a bad subscript, the **target** for a name that is not bound or may not be
   written. Three spans had to be added to the AST to make that possible —
   `Stmt::IndexedAssign::target_span`, the same on `IndexedAssign2D`, and `FuncDef::name_span` —
   following the precedent `Stmt::Assign::target_spans` had already set, because a `Place` and a
   `SymId` have nowhere to hang a position.

7. **Codes renumbered to a 1000-wide band, and the rule data collapsed into one row.** The analysis
   codes moved `PINP0101`–`PINP0103` → `PINP1001`–`PINP1003`, which is why the bands above read
   `PINP2xxx`/`PINP3xxx` rather than `PINP02xx`/`PINP03xx` — a thousand per layer rather than a
   hundred, decided now because nothing has shipped and no suppression pragma exists to break.
   Separately, `code()`/`name()`/`description()`/`severity()` were four parallel matches over the
   same three variants, so a variant's data lived in five places; they now read from one
   `const fn info() -> RuleInfo`, leaving one row per variant and one edit to add a code.
   [0015](0015-static-analyses-start.md) § Step 2 carries a notice pointing here.

8. **`unused-binding` became `unused-variable`, and stopped suppressing dead stores.** Three
   defects in one rule, found while checking what a finding actually prints:

   - *Vocabulary.* "Binding `x` is never read." put an analysis-layer term in front of a user.
     `binding` stays as this codebase's internal word; the message, rule name, description, and
     `DiagnosticCode` variant now say **variable**, as `rustc` and clang do.
   - *Placement.* The finding landed on the symbol's **last** write, because the reporting loop
     walks each block backwards and took the first write it reached. The comment beside it had
     always claimed "where it is introduced". It now reports at the lowest write position.
   - *Suppression.* A symbol nothing reads produced one finding and swallowed the dead stores at
     its other writes. Those writes are dead regardless of whether anything ever reads the symbol,
     and `rustc` reports both. So `x = 1; x = 2; x = 3` with no read now yields one
     unused-variable at `x = 1` plus a dead store at each of the other two, where it previously
     yielded a single finding at `x = 3`.

   This reverses a documented [0015](0015-static-analyses-start.md) decision — that module doc had
   justified the suppression since it was written — so it is recorded here rather than quietly
   changed. It also breaks the "no behaviour change" constraint above, which was written about the
   three *error* layers being refactored; the analysis layer's output was not in view when that
   constraint was set.

Known imprecision: a rule that fires after its construct is fully consumed — `check_monotonic` on a
non-monotonic comparison chain — is positioned on the token *following* the construct. Strictly
better than the nothing it had before, and fixable per-site later without reshaping anything.

## Preserved defect: inconsistent-indentation placement

The lexer reports "Inconsistent indentation." at the newline *ending the previous line* — for a bad
indent on line 3, it says line 2, at the column one past that line's last character. It comes from
`emit_indent` being handed the pending newline's offset rather than the offending line's indent.

Preserved here rather than corrected, because "no behaviour change" is the constraint that makes the
rest of this iteration safe to review. Recorded as **KI-0001** and locked in by a test so the fix,
when it comes, is a deliberate edit to a named expectation rather than a silent drift.

## Deferred

- **Parse/sema code bands** (`PINP2xxx`/`PINP3xxx`) — deliberately not minted; see the key decision.
- **Batched errors / recovery** — still ruled out by the fail-fast rule.
- **Secondary notes on errors.** `Diagnostic::notes` exists and renders, but no error emits one yet.
  "Expected `)` to close this `(`" is the obvious first customer.
- **A consumer abstraction.** The C++ has pluggable `DiagConsumer` sinks; pinp returns values and
  renders on demand, which is simpler and easier to assert against. Revisit only if a second output
  format beyond SARIF appears.
- **`redundant-return`** — the C++ emits "Return on last line is redundant" (`src/sema.cc:491`) and
  pinp has no equivalent. It is AST-shape, not dataflow, so it belongs in neither `checks/` nor this
  iteration; a ledger row if wanted.
