[Human]

# Strings — immutable `str` with small-string optimisation

Text data is the `str` type — an immutable character string. Initial encoding is ASCII only; UTF-8
is deferred far into the future. A non-ASCII byte is stored opaquely, not a parse error.

As with every other type, a binding's type is inferred from its initialiser.

## String literals

A literal may start with a single or a double quote; the closing quote matches the opening one.
`'hello'` and `"hello"` are identical. No mixed delimiters. Escape sequences (`\'`, `\"`, `\\`,
`\n`, …) are in scope of the language design but **deferred** — for now the only character a literal
cannot contain is its own delimiter.

A literal **may span multiple physical lines** — a newline between the delimiters is part of the
string. There is no separate triple-quote form; ordinary `'...'`/`"..."` (and `f'...'`/`f"..."`)
carry multiline content. Continuation lines are **auto-dedented** so the literal can be indented to
match its surrounding code without the indentation leaking into the value:

```
foo() is
    x = "first
         second
         third"          # x == "first\nsecond\nthird"
    x.len
```

Accepted tradeoff: because any quote may now span lines, a *forgotten* closing quote is detected at
end-of-file rather than end-of-line. The benefit is no prefix or triple-delimiter ceremony.

## f-strings (interpolation)

`bar = f'fu = {fu} bar = {bar}'` is supported. Each `{name}` hole names an existing binding —
a local/parameter, or a `{::name}` global. Whitespace around the name is ignored, so `{ fu }` and
`{fu}` are the same hole. An unresolved name is a **sema error**, since every name is statically
visible at compile time. Interpolating a non-`str` scalar (`bool`/`int`/`float`) stringifies it.

## Operations

- **Concatenation** via `str1 + str2 + … + strN`. When the left operand of `+` is `str`, the right
  operand is implicitly wrapped in `str(...)`; `x + str` where `x` is not `str` is a sema error.
- **`.len`** — the byte length (ASCII length == byte length). Reuses pinp's existing built-in
  member syntax: `x.len`, **not** `x.len()` (pinp lowered `.len` to a member access in 0012; we
  keep that convention rather than the prototype's method-call `.len()`).
- **Comparisons** `== != < <= > >=` — both operands must be `str`; the result is `Bool`. `str` vs a
  numeric operand is a sema error.
- **`str(x)`** — converts a `bool`/`int`/`float`/`str` to its `str` form.
- **`meminfo()`** — diagnostic built-in: prints mimalloc stats to stderr; evaluates to nothing
  (a `void` statement).

## Memory

A `str` of 15 bytes or fewer is stored inline (small-string optimisation); a longer one is
heap-allocated through the existing `pinp_alloc`/`pinp_free` shim, so string allocations are
covered by the same byte-exact bookkeeping (`pinp_memory_info`) the leak check already uses — no
separate string counter.

When a `str` binding goes out of scope its storage must be freed; the same holds for every
temporary produced along the way. After

```
foo(beers: int) is
  x = f'{beers}    bottles    ' + ' of              ' + 'beer    on    the    wall'
  0
foo(99)
```

runs, the runtime's outstanding-byte count must return to zero (allocation count == free count).

Bonus: a chain `str + str + … + str` should lower to a single allocation, not N − 1 pairwise ones.

---

[AI]

## Clarifications from design discussion

- **Encoding**: ASCII only; non-ASCII bytes stored opaquely. UTF-8 deferred.
- **Quotes**: single and double interchangeable, matching delimiter required, no mixing.
- **Scope of 0014** (decided with the user): full prototype-0008 surface — literals, f-strings,
  `str + str` (with the chain-flatten optimisation), `.len`, `str(x)`, the six comparisons, and
  `meminfo()` — **plus** two things the prototype deferred: deterministic free-on-scope-exit (new
  machinery, see below) and **multiline literals with auto-dedent** (no triple-quote form; any
  quote spans lines).
- **`str` function parameters are deferred.** pinp has typed parameters, so `greet(name: str)` is
  within reach, but it is held back to a follow-up to keep this iteration's freeing model to a
  single function frame at a time. `str` flows as locals, globals, f-string interpolations, and a
  function's inferred **return** type (the top-level program itself returns a `str` this way).
- **`.len`, not `.len()`** — reuses [`BuiltinMember::Len`]; `str.len` is a `Node::Member`, lowered
  to `pinp_str_len`. The prototype's method-call spelling is not adopted.
- **`str + x` auto-conversion**: if the left operand is `str`, the right is implicitly `str(...)`-
  wrapped (must itself be a scalar or `str`). `x + str` with non-`str` `x` is a sema error.
- **`meminfo()` is `Void`** here (pinp has a real `Void`), not the prototype's `0.0`.
- **No separate `get_allocated_string_count()`** — string heap traffic routes through `pinp_alloc`/
  `pinp_free`, so `pinp_memory_info` already accounts for it. The leak guarantee is
  `allocation_count == free_count` after the run.
- **Deferred**: escape sequences, **raw strings** (a no-dedent / no-escape form), `str` parameters,
  `str`-typed array elements, string interpolation of aggregates (arrays/ranges), UTF-8. (Multiline
  is now in scope; only the *raw* variant is held back.)

## Runtime — `src/runtime/string.rs` (new, Rust)

The prototype's `ImmutableString`/`PinpStr` is ported to Rust as `extern "C"` functions resolved by
the JIT's existing process-symbol generator (the same path that already finds `pinp_alloc`). The
16-byte `PinpStr` wire struct keeps the prototype's layout exactly:

```
inline (tag_len bit7 == 0):  buf[0..15] data, tag_len = length (0..15)
heap   (tag_len bit7 == 1):  ptr (8) | len (4) | cap (4, bit31 = is_heap)
```

`#[repr(C)]`, `size_of == 16`, discriminated on byte 15. Heap path calls `pinp_alloc`/`pinp_free`
(declared `extern "C"`), so the inline path costs nothing and the heap path is leak-tracked.

Exported symbols (all `#[unsafe(no_mangle)] pub extern "C"`):

- `pinp_str_from_cstr(*const c_char) -> PinpStr`
- `pinp_str_free(*mut PinpStr)`
- `pinp_str_concat(*const PinpStr, *const PinpStr) -> PinpStr`
- `pinp_str_concat_n(*const PinpStr, usize) -> PinpStr` — single allocation for the whole chain
- `pinp_str_len(*const PinpStr) -> i64`
- `pinp_str_eq(*const PinpStr, *const PinpStr) -> i32`
- `pinp_str_cmp(*const PinpStr, *const PinpStr) -> i32` — `memcmp`-style three-way
- `pinp_str_from_int(i64) -> PinpStr`, `pinp_str_from_float(f64) -> PinpStr`
- `pinp_meminfo()` — `mi_stats_print(NULL)` to stderr

`pinp_str_from_bool` is not a runtime call — codegen renders a `Bool` as `"true"`/`"false"` via
`pinp_str_from_cstr` on a constant.

## PinpStr LLVM type

Represented in IR as `{ i64, i64 }` — two INTEGER eightbytes — so the SysV ABI returns it in
`rax:rdx`, matching the C struct. `[16 x i8]` would classify as MEMORY (sret) and mismatch.

## Lexer

Two new logos lexemes (and `TokenKind`s), captured whole; **the delimiters may enclose newlines**,
so a literal spans lines:

- `Str` — `'[^']*'` or `"[^"]*"`. An unterminated literal matches nothing (no closing quote before
  EOF) and is a `LexError` reported at the opening quote.
- `FStr` — `f'[^']*'` or `f"[^"]*"`. Longest-match keeps a bare `f` an `Identifier`.

A multiline string token consumes its own interior newlines, so they never reach the
indent/dedent (`NewlineIndent`) logic — indentation tracking is undisturbed mid-string, and the
newline *after* the closing quote is the ordinary statement separator. The lexer still records the
token's start `line`/`col`; subsequent tokens locate from their own offsets, so positions stay
correct across the multiline span.

The raw token slice is zero-copy as always, but the literal's *content* is no longer a source slice
(auto-dedent rewrites it) — see AST below.

## AST

Auto-dedent (and, later, escapes) rewrite a literal's bytes, so the content cannot be a borrowed
source slice. Literal content is therefore **owned**, in a new arena parallel to the others:
`Ast.string_literals: Vec<String>`, addressed by a `StrId(u32)`. `Node` stays flat and cheap to
clone (it carries a `StrId`, not a `String`); the identifier interner is untouched.

- `Node::Str(StrId)` — the final, dedented literal content.
- `Node::FStr { segments: Vec<FStrSegment> }` where
  `FStrSegment = Literal(StrId) | Interp(Place)` — `Place::Local`/`Place::Global` already encodes
  the `::` distinction. The parser dedents the whole `f'...'` content, then splits it into
  alternating literal/hole segments, pushing each literal run into `string_literals`.
- `PinpType::Str` added to the enum (payload-free, stays `Copy`).

`Node::Member` with member `len` is reused for `.len`; no new node.

## Grammar, multiline content, and auto-dedent

`str_lit` and `f_str_lit` join `number`/`bool`/`name` as atoms in `parse_primary`. f-string holes
are `{identifier}` or `{::identifier}`; an empty hole `{}`, an unbalanced brace, or a non-name hole
is a parse error. A hole's text is re-lexed (the lexer is the single source of truth for "name"),
so surrounding whitespace is tolerated — `{ x }` is the same as `{x}`.

The parser turns a raw token into content in two steps: strip the delimiters (and leading `f`),
then **auto-dedent**:

1. Split the stripped content on `\n`.
2. One line (no `\n`) → verbatim; nothing is stripped.
3. Otherwise remove the longest common run of leading **space** characters shared by every *non-blank
   line from the second onward*. The first line is exempt (it begins immediately after the opening
   quote and carries no representative indent); whitespace-only lines do not constrain the common
   prefix. Tabs are not spaces and are left as-is (pinp indentation is space-based).
4. The first line is emitted unchanged; each continuation line has the common prefix removed.

For f-strings the dedent is applied to the full inter-delimiter content (holes included) *before*
segment splitting, so a hole that sits mid-line is unaffected by dedent and the literal runs around
it are dedented consistently.

## Sema

- `infer`: `Node::Str` and `Node::FStr` → `Str`; a `Bin{Add}` whose lhs is `Str` → `Str`.
- `assignable`/`join`: `Str` relates only to `Str` (no promotion in or out).
- f-string: each `Interp(place)` must resolve and be a scalar (`Bool`/`Int`/`Float`) or `Str`;
  anything else (aggregate/`Void`/`Range`) is an error.
- `Bin{Add}`: if `left == Str`, require the rhs to be `str()`-convertible (scalar or `Str`) and the
  result is `Str`; if `left != Str` but `right == Str`, error.
- Comparisons: if either side is `Str`, both must be `Str` (→ `Bool`); `Str`-vs-numeric is an error.
- `Member{len}` on `Str` → `Int` (a new arm in `member_type`, recording `BuiltinMember::Len`).
- `str(x)` and `meminfo()` join `identity` as name-intercepted built-ins in `call_type`:
  `str` takes one scalar/`str` arg → `Str`; `meminfo` takes none → `Void`.
- **`str` parameters are rejected here** (the deferral the parser comment defers to): a function
  signature with a `PinpType::Str` parameter is a sema error this iteration, since the cross-call
  freeing model is out of scope. The annotation parses (`parse_type` accepts `str`); sema is what
  turns a `str` *parameter* into an error.

## Codegen — values and the freeing model

This is the new machinery. A `str` binding gets a `{ i64, i64 }` alloca; reads/writes are by value.

- `emit(Str)` → `pinp_str_from_cstr(global)`.
- `emit(FStr)` → emit each segment as a `PinpStr` (`from_cstr` for literals; `from_int`/`from_float`/
  constant for scalar holes; a load for `str` holes), then one `pinp_str_concat_n`.
  - Scalar→`str` rendering: `int` formats via the dependency-free `StackBuf` (decimal, no heap);
    `bool` renders as `"true"`/`"false"`; `float` moves from `to_string` to `ryu` in step 8 (the
    deliberate `2.0`-not-`2` change). Cover each with tests as its step lands.
- `emit(Bin Add)` with `str` lhs → `collect_str_parts` walks the left spine, then a single
  `pinp_str_concat_n`. (The flatten bonus.)
- Comparisons → `pinp_str_cmp` (or `pinp_str_eq` for `==`/`!=`) against 0.
- `.len` → `pinp_str_len`. `str(x)` → `from_int`/`from_float`/constant.

**Ownership.** A freshly produced `PinpStr` (any runtime call result) is an *owned temporary*. The
operation that consumes it frees it immediately afterwards (`pinp_str_free` on a spill slot) unless
it is being stored into a named binding, which takes ownership. Each scope frame records the slots
that own a `str`; on scope exit — block end, loop-iteration end, and function return — those slots
are freed. A `str` **returned** from a function (or the top-level program) is *moved out*: its slot
is not freed on the way out; ownership passes to the caller / the host harness.

This is pinp's first deterministic-free type; arrays remain unfreed (out of scope, future work).

## JIT result dispatch

`PinpValue::Str(String)` is added. The entry's out-pointer ABI already returns the program's value
through a caller buffer; for a `str` result the buffer is the 16-byte `PinpStr`. `run` reads
`data`/`len` out of it, copies the bytes into a Rust `String`, then calls `pinp_str_free` on the
buffer (the move-out's matching free). `PinpJit::result_type == Str` selects this path.

## Implementation steps

The iteration is split into self-contained, separately-reviewable steps. Each follows the project
flow: its tests are written and reviewed *first*, then implemented red→green→refactor. A step
compiles and its tests pass before the next begins. Single-line strings are taken end-to-end first;
multiline + auto-dedent lands last as an isolated delta.

1. **Runtime** (`src/runtime/string.rs`). The Rust `PinpStr` and its `extern "C"` functions, with no
   compiler integration. Tested directly from Rust: SSO inline/heap boundary, `concat`/`concat_n`,
   `len`/`eq`/`cmp`, `from_int`/`from_float`, and the leak invariant (heap traffic balances through
   `pinp_alloc`/`pinp_free`). Self-contained.

2. **Lexer**. Single-line `Str`/`FStr` tokens (both quotes), `f`-disambiguation, unterminated →
   `LexError`. In-module tests. (Multiline deferred to step 7.)

3. **Parser + AST**. `PinpType::Str`, the `string_literals` arena and `StrId`, `Node::Str`,
   `Node::FStr` with segment splitting, atoms in `parse_primary`, f-string hole grammar and its
   errors. In-module parser tests. (Single-line; no dedent yet.)

4. **Sema**. `str` inference; concat typing with rhs auto-wrap; the six comparisons; `.len` on `str`;
   the `str(x)` and `meminfo()` built-ins; f-string interpolation name resolution and its errors.
   Extensive + adversarial in-module sema tests.

4b. **Review fixes** (added after a full-project review between steps 4 and 5; tests first, as
   always):
   - Reject duplicate binders in a multi-binder `for` (`for i, i in arr` is currently accepted
     and the second binder silently wins). New sema check plus tests.
   - Reject a comment inside an f-string hole: `f'{x#note}'` currently interpolates `x` because
     the hole is re-lexed with comment skipping. Make it a parse error.
   - Add the leak-invariant runtime test that step 1 promised but never landed: after alloc/free
     traffic through the string runtime, `pinp_memory_info` counts must balance. Also cover
     `concat_n` crossing the 15→16 inline/heap boundary exactly.
   - Pin down corners that already behave correctly but have no test: `s += 'b'` (compound
     concat), `s *= 'b'` error, unary `-`/`not` on `str`, `str` as a range bound, `for` over a
     `str`, a chained `str` comparison (`'a' < 'b' < 'c'`), a mixed `str`/`int` ternary (joins to
     `Void`), and `str` array elements being rejected.

5. **Codegen + host return**. The `{i64,i64}` type; `emit(Str)`/`emit(FStr)`; concat with chain-
   flatten; `str(x)`; `.len`; comparisons; `meminfo()`. `PinpValue::Str`, the JIT string-return
   dispatch, and the `eval_str` helper (the host frees the returned `PinpStr`). First e2e milestone
   (`tests/strings.rs`): content, length, comparisons. **No in-JIT freeing yet** — temporaries leak
   within a run, as arrays already do. (May split into 5a literals/`.len`/return path and 5b concat/
   `str()`/f-string/comparisons if the diff is large.)
   - **Prerequisite:** export the string runtime's symbols in `build.rs`. The `pinp_str_*` and
     `pinp_meminfo` functions are Rust `#[no_mangle]` functions in the binary, and a Linux
     executable does not put those in its dynamic symbol table by default — so the JIT's
     process-symbol lookup cannot find them until each gets an `--export-dynamic-symbol` link
     argument, like the five C `pinp_*` symbols already have.
   - **Prerequisite:** handle allocation failure in the string runtime. `pinp_alloc` returns null
     when memory runs out, and `make_with` currently writes through the result unchecked. A null
     result should raise `pinp_runtime_error("Out of memory.")` instead.
   - **Decided — `str(float)` uses ryu's spelling throughout.** ryu and `to_string` agree exactly on
     the non-finite cases (`"NaN"`, `"inf"`, `"-inf"`, and no sign on a negative NaN), so there was
     nothing to trade off there; they differ only on finite values. Rather than write float
     expectations against `to_string` and rewrite them in step 8, `pinp_str_from_float` moved to ryu
     up front — step 8 is therefore already done (see below).

6. **Freeing model**. Deterministic free-on-scope-exit for owned `str` slots, temporary frees after
   the consuming op, and move-out for a returned `str`. Leak-check e2e via `pinp_memory_info`
   (`allocation_count == free_count` after concat-heavy programs, `str`-local functions, and loops).

7. **Multiline + auto-dedent**. Lexer regex spans newlines (unterminated → EOF error); parser
   auto-dedent (owned content is already in place from step 3). Lexer/parser/e2e tests for multiline
   round-trips and the dedent edge cases.
   - **TODO (revisit here):** decide whether the lexer should also assert the *inner* string
     content, or whether stripping/content checks belong solely to the parser layer. Deferred from
     step 2 pending concrete parser dedent/segment tests to judge against.
   - **TODO (end-of-iteration coverage review):** run `cargo llvm-cov` and close the
     `runtime/string.rs` gap (~81% lines at step 2 — `pinp_meminfo`, the over-long guard behind the
     `#[ignore]`d test, and the concat paths still un-exercised). Confirm the later steps lifted it
     and add targeted tests for anything still uncovered.
   - **TODO (decide while touching the lexer):** tabs and CRLF. Today a tab in indentation is
     silently ignored (a tab-indented block fails with the unhelpful "Expected Indent"), and a
     CRLF file fails with "Unexpected character `\r`". Either reject both with clear messages or
     accept them — and test whichever we choose.

8. **Float formatting via `ryu`.** *(Done — pulled forward to step 4b, ahead of the first float
   expectations in step 5, once the spelling question above was settled.)* Replace
   `pinp_str_from_float`'s `to_string`
   (a transient heap `String`) with `ryu::Buffer`, a stack buffer — no heap traffic, thread-safe (a
   per-call local), and stack-bounded because ryu uses scientific notation for extreme magnitudes.
   This is a *deliberate format change*: ryu renders `2.0` (not std's `2`) and `1e300`-style
   exponents, which the user prefers (a float reads as a float). Update the `from_float` test
   accordingly (`2.0` → `"2.0"`) and any e2e float-interpolation expectations. `ryu` is float-only,
   so integer formatting **stays** on the dependency-free `StackBuf` already in place — no `itoa`.

## Tests

Per the project test policy, an extensive sema suite plus adversarial coverage precedes the code.
The per-step tests above are the authoritative breakdown; the list below is the same coverage viewed
by layer:

- **lexer** (in-module): both quote styles, empty literal, content with spaces/punctuation,
  unterminated → error (at EOF), a **multiline literal** is one token and leaves the surrounding
  indent/dedent stream intact, `f'...'`/`f"..."`, bare `f` stays an identifier, `.len` unaffected.
- **parser** (in-module): `Node::Str` content, **auto-dedent** (single-line no-op; multi-line common-
  prefix strip; first-line exemption; blank lines ignored; tabs untouched), f-string segment
  splitting (literal-only, single/multiple holes, `::global` hole, leading/trailing literal, dedent
  around a mid-line hole), empty/unbalanced/non-name hole errors.
- **sema** (in-module, extensive + adversarial): `str` inference; concat typing and rhs auto-wrap
  (`"n=" + 5`); `5 + "x"` error; comparisons `str==str`, `str<str`, `str`-vs-`int` error; `.len`
  on `str` → `Int`; `str(x)` typing and bad-arg error; f-string unknown-name and aggregate-hole
  errors; `str = int` rebind error; `meminfo()` → `Void`.
- **e2e** (`tests/strings.rs` via `PinpJit`): literal round-trip (both quotes), empty, SSO boundary
  (15 inline vs 16 heap), long heap string correctness; **multiline literals** (dedented content
  round-trips exactly; `.len` counts the embedded newlines; multiline f-string); concat (pair, 3+
  chain, auto-wrap int/float/bool); `.len` of literal/var/concat; f-strings (literal-only, one/many
  holes, `::global`, scalar holes); all six comparisons incl. in conditions; `str(int/float/bool/str)`;
  locals/globals/rebind;
  **leak checks** through `pinp_memory_info` (`allocation_count == free_count` after: a concat-heavy
  program, a function with `str` locals, a returned-and-freed `str`, and a loop building a `str`
  each iteration).

## Deferred

- `str` function parameters (the freeing model across call boundaries).
- Escape sequences; multiline / raw strings; UTF-8.
- Interpolating aggregates (arrays/ranges) into f-strings.
- `str`-typed array elements.
- **Mutable strings** (a separate future iteration). Surface is a compiler built-in
  `mutstr(initial_content, [cap: nnn])`, *not* a literal prefix like `m"content"` — chosen after
  deliberation as cleaner and more explicit (capacity is a named argument, no new literal syntax).
  The `cap` argument also drives allocation: **no `cap` ⇒ heap-only**; a `cap` of a reasonable
  compile-time size makes the buffer **eligible for stack allocation** instead of the heap. The
  `PinpStr` layout already reserves the `is_mutable` flag (bit30 of `cap` / bit6 of `tag_len`) for
  this.
- **Internal refactors** (out of this iteration's scope, queued for a follow-up; noted here so
  they are not forgotten):
  - Give built-in *functions* the same enum treatment built-in members already have: sema resolves
    `identity`/`str`/`meminfo` by name once and records an enum, so codegen stops repeating string
    comparisons and a new built-in cannot be added in one place but forgotten in another.
  - Stop sema cloning every node it visits (`analyze_expr` clones whole `if` arms and argument
    lists today); read the small id fields out instead.
  - Extract the counted-loop scaffold in codegen into a shared helper — six emitters currently
    hand-roll the same header/body/exit blocks.

---

> **Disclaimer:** This is Human + AI generated documentation. Treat it with a little bit of salt — it may not always reflect reality.
