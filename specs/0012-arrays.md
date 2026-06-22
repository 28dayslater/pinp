[Human]

# Arrays

This spec covers arrays across multiple iterations. Design decisions for all ranks are recorded
here; implementation is phased — see the scope section below.

## Scope of this iteration (0012)

In scope:
- 1D array literal initialisation (deduced element type)
- 1D range initialisation with all-literal bounds and step
- 1D array comprehension with a literal-bound range source (no filter, no nesting)
- Heap allocation via mimalloc
- Read and write indexing: `arr[i]`, `arr[i] = x`
- 1D array slice access and assign
- Out-of-bounds runtime error
- `.len` built-in member
- Returning a 1D array to the host via `PinpValue`
- Replacing the 0008 error-global mechanism with `longjmp`-based runtime error handling

Deferred to subsequent iterations:
- `for idx in arr` and `for idx, val in arr`
- Comprehension over an array source (`[f(x) for x in arr]`)
- Comprehension filter clause (`[x for x in range if cond]`)
- Multi-rank comprehensions (`[expr for x in R1, y in R2, ...]`)
- 2D matrix syntax (`[...; ...]`) and `[i, j]` indexing
- Higher-rank fill form (`[MxNxP of type = val]`)
- `.len` returning a tuple for higher-rank arrays

## TODO

- **Full array comprehension** — filter clause and array-source form deferred until `for x in arr`
  iteration is in place.

- **Escape analysis and automatic deallocation** — heap-allocated arrays must be freed when they
  go out of scope and do not escape (returned, assigned to a global, or passed to a function).
  Sema annotates each array binding with an escape flag; codegen emits `pinp_free` at the
  enclosing scope boundary for non-escaping arrays. Alias propagation (`b = a` → `a` inherits
  `b`'s escape status) must be handled. For 0012, arrays are not freed (acceptable for a
  short-lived JIT run); this is a known leak.

- **Leak detection in tests** — the runtime already exposes `pinp_memory_info` (outstanding
  bytes, allocation count, free count). Once deallocation is implemented, end-to-end tests
  should assert `outstanding_bytes == 0` after each run to catch regressions.

## 1D arrays

### Initialisation

Literal initialisation — element type is deduced from the values:

```
ary = [0, 2, 13, 42]      # array of int
ary = [1.0, 2.5, -0.5]    # array of float
ary = [true, false, true]  # array of bool
```

Initialisation may break to following lines. In that case, 1st literal in the next line is required
to start at the colukn of the 1st literal of the 1st line. 

Range initialisation — only with all-literal bounds and step. Variable-bound ranges and range
variables are not permitted here:

```
ary = [0..20:2]    # 11 elements: 0, 2, 4, ..., 20
ary = [1..<10]     # 9 elements: 1, 2, ..., 9
```

Memory is heap-allocated via the mimalloc runtime. The element count is always known at compile
time (a consequence of the literal-only constraint).

### Indexing and mutation

```
x = ary[i]         # read element at zero-based index i
ary[i] = x         # write element at index i
```

Out-of-bounds access is a runtime error.

### Built-in member

```
n = ary.len        # number of elements (Int)
```

## Array slices

One can get a slice of array using a static (not a variable bound one) range without step.
Out of bound index test is performed by sema at compile time.
```
a,b = 1,4
fu = [0..12:2]    # [0,2,4,6,8,10,12]
bar = fu[a..b]    # Error: Variable-bound slice is not allowed.
bar = fu[2..6:2]  # Error: Step is not allowed in slice.
bar = fu[-1..3]   # Error: Slice index out of bound.
bar = fu[2..7]    # Error: Slice index out of bound.
bar = fu[2..4]    # [4,6,8]
```

## Array element assign by slice

```
fu = [0..12:2]    # [0,2,4,6,8,10,12]
fu[1..3] = 1      # [0,1,1,1,8,10,12]
```

IOOB checks just like in access.

We could support array copy into a slice later on.

NP array slice access is deferred.

### Comprehension

A comprehension builds an array by mapping an expression over a range. The range source must have
all-literal bounds and step — the same constraint as range-init, so the element count is always
known at compile time:

```
squares  = [x*x for x in 1..5]       # [1, 4, 9, 16, 25]  (5 elements)
evens    = [x for x in 0..10:2]      # [0, 2, 4, 6, 8, 10]  (6 elements)
offsets  = [x + base for x in 0..4]  # base must already be in scope
```

The binder variable (`x` above) is scoped to the element expression only; it is read-only and
does not escape the comprehension. Its default type is `Int` (ranges are integer sequences).

An optional type annotation promotes the variable before it enters the element expression:

```
floats = [x * 0.5 for x:float in 0..4]   # [0.0, 0.5, 1.0, 1.5, 2.0]
```

Only upward promotions are valid: `float` is useful and accepted, `int` is accepted but
redundant, `bool` is rejected. This is the idiomatic way to get float arithmetic in a
comprehension without introducing a stray `1.0 *` in the element expression.

There are indentantion rules in case the comprehension needs to break into following lines:
Every next line 1st non-space character must be at the column where the expression after "[" starts:
```
fu = [x + 2*x + 3*x +
      4*x + 5*x 
      for x:float in 0..10]

bar = [x + 3*x + 5*x +
       7*x + 9*x 
       for x:float in 0..10]

baz = [
    x + 3*x + 5*x +
    7*x + 9*x 
    for x:float in 0..10
]
```

Filter clauses (`[x for x in range if cond]`) and comprehensions over array sources
(`[f(x) for x in arr]`) are deferred to the next iteration.

### Iteration

`for idx in arr` and `for idx, val in arr` are deferred to the next iteration.

### Program result

A 1D array may be returned to the host via `PinpValue`.

## 2D matrices

A 2D matrix uses the same `[...]` bracket as a 1D array. The presence of `;` (row separator)
distinguishes it: any `[...]` containing at least one `;` is a 2D matrix. No prefix keyword is
needed. The layout is row-major and the element type is deduced:

```
ary2d = [1.0, .0, .0, .0;
         .0, 1.0, .0, .0;
         .0, .0, 1.0, .0;
         .0, .0, .0, 1.0]   # 4×4 float identity matrix, multi-line form

ary2d = [1.0, .0, .0, .0; .0, 1.0, .0, .0; .0, .0, 1.0, .0; .0, .0, .0, 1.0]  # same, single-line
```

`;` is the row separator; `,` separates elements within a row. The column count is taken from the
first row; every subsequent row must have the same count, or it is a compile-time error. The shape
is therefore fully determined by the literal — no visual-alignment inference.

When the literal spans multiple lines, the same continuation rule as 1D applies: the first element
of each row on a new line must start at the column of the first element of the first row.

A trailing `;` (which would imply a 1×N matrix) is not supported and is a parse error. A 1×N
matrix is indistinguishable from a 1D array at the literal level; use a 1D array instead.

Indexing uses comma-separated indices in a single bracket pair:

```
x = ary2d[i, j]
ary2d[i, j] = x
```

Built-in member `.len` for a 2D matrix returns a tuple `(rows, cols)`.

## Higher-rank (ND) arrays

### Shape and fill

Higher-rank arrays are declared with dimensions, element type, and an optional fill value:

```
mtx = [4x6x8 of float = -1.0]   # 4×6×8 float array, all elements -1.0
mtx = [4x6x8 of int]            # default fill: 0
```

If no fill value is given, the default is `0`, `0.0`, or `false` depending on element type.

### Initialisation from flat data

For non-trivial element patterns, a ND array can be initialised from an embedded flat literal
via the compiler-known `.from_flat` method:

```
mtx = [4x4x2 of int].from_flat([1, 2, 3, ..., 32])   # 32 = 4×4×2 elements
```

The argument must be an inline array literal — not a named variable. `.from_flat` is a
compile-time construct: sema verifies the literal length against the dimension product
(4×4×2 = 32 here) and rejects a mismatch. Accepting a named array would require runtime
checks and is not supported.

A fill value and `.from_flat` are mutually exclusive — `[4x4x2 of int = 0].from_flat([...])`
is a compile-time error: the fill value would be immediately overwritten and serves no purpose.
Without a fill value, `.from_flat` allocates and fills in a single pass with no zero-init.

Element order is row-major: `from_flat` fills `[i, j, k]` in the order `i` varies slowest,
`k` fastest.

`.from_flat` requires method-call syntax (`expr.name(args)`) which is not yet in the language;
this is a prerequisite for the ND implementation iteration.

### Memory layout

Memory is a single contiguous allocation. Element `[i, j, k]` in an M×N×P array is at offset
`i*(N*P) + j*P + k` — row-major stride arithmetic, no intermediate pointer arrays.

### Indexing and `.len`

Indexing follows the same `arr[i, j, k, ...]` form as 2D. `.len` for higher-rank returns a
tuple of dimension sizes.

### Multi-rank comprehensions

A comprehension may produce a multi-rank array by supplying one range per dimension in the
`for` clause. The element expression is a single scalar (parentheses are for multi-line
grouping only):

```
mat = [
    (x*y + z)
    for x in 0..10, y in 0..12, z in 0..6
]
```

This yields a 3D array of shape 10×12×6. `x` varies slowest, `z` fastest (row-major,
consistent with `.from_flat` order).

When all dimensions share the same range, a shorthand binds multiple variables to one range:

```
mat = [x*y + z*t for x,y,z,t in 0..10]   # 4D, shape 10×10×10×10
```

The `var:type` annotation applies per variable: `for x:float, y:float in 0..10` or
`for x:float in 0..10, y:float in 0..12`.

All ranges must have literal bounds; the full shape is compile-time known. Multi-rank
comprehensions require ND array types and belong to the ND implementation iteration.

## Runtime errors

A runtime error — such as an out-of-bounds array index — stops execution immediately and is
reported as an error message to the caller, regardless of how deeply nested the offending code is.
The JIT session itself remains alive; a REPL can continue accepting input after a runtime error.

From the top level's point of view:

- `PinpJit::run` returns `Ok(value)` on success or `Err(message)` on a runtime error.
- A runtime error is never a panic or a signal — the process does not abort.
- Execution stops at the point of the error; no further statements or expressions are evaluated.

Current runtime errors and their messages:

| Condition                     | Message                          |
|-------------------------------|----------------------------------|
| Array index out of bounds     | `Array index out of bounds.`     |
| Range step is zero            | `Range step cannot be zero.`     |

The 0008 out-of-band error-global mechanism is replaced by this iteration: a native runtime
function `pinp_runtime_error` (linked alongside mimalloc) unwinds the JIT call stack via
`longjmp`, returning control to `PinpJit::run` at any nesting depth.

---

[AI]

## Goal

Add **1D arrays** as a first-class storable value: heap-allocated via mimalloc, size fixed at
compile time, with literal, range-init, and comprehension forms, read/write index access, a
`.len` member, and a returnable `PinpValue::Array`. Alongside, replace the 0008 out-of-band
error-global with a `longjmp`-based mechanism that unwinds at any nesting depth without crashing
the process.

## Data model ([parser/ast.rs](src/parser/ast.rs))

### `ElementType`

A new `Copy` enum for the element type of an array. A separate type keeps `PinpType: Copy` —
storing `PinpType` inside itself would require `Box` (recursive enum).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElementType {
    Bool,
    Int,
    Float,
}
```

### `PinpType::Array`

```rust
Array(ElementType, usize),   // (element type, element count)
```

Two arrays with the same element type but different lengths are distinct types; the full shape is
always known at compile time.

### New `Node` variants

```rust
/// `[e1, e2, ...]` — a literal 1D array. Element type and count are filled by sema.
ArrayLiteral { elements: Vec<ExprId> },

/// `[start..stop[:step]]` — range-init. The inner node must be a literal-bounds Range.
/// The element count and sequence are computed by sema.
ArrayFromRange { range: ExprId },

/// `arr[idx]` — element read; yields the array's element type.
Index { array: ExprId, index: ExprId },

/// `arr.len` — compile-time element count; yields `Int`.
Member { object: ExprId, member: SymId },

/// `[element_expr for var[:type] in range]` — comprehension over a literal-bound range.
/// `var` is bound as `var_type` (default `Int`) within `element_expr` and is read-only.
/// The element count equals the range length, computed at sema time.
Comprehension { element: ExprId, var: SymId, var_type: PinpType, source: ExprId },
```

### `Stmt::IndexedAssign`

`arr[i] = x` is structurally distinct from the existing multi-assign (whose targets are names).

```rust
IndexedAssign {
    target: Place,   // Local(sym) or Global(sym) — must resolve to an Array
    index: ExprId,
    value: ExprId,
}
```

The parser extracts `target` from the `array` child of a parsed `Index` node (which must be a
`Var`/`Global`); only simple named targets are supported. Compound indexed assignment (`arr[i] +=
x`) is deferred.

### `PinpValue`

`PinpValue::Array(Vec<PinpValue>)` is added, holding the elements as scalar `PinpValue`s. This
variant contains a `Vec`, so `Copy` is removed from `PinpValue`; `Clone` and `PartialEq` are
kept. No existing test assertions break — they all compare via `==`.

## Lexer ([lexer.rs](src/lexer.rs))

Three new `Lexeme` / `TokenKind` pairs:

- `[` → `LBracket`
- `]` → `RBracket`
- `.` → `Dot`

`Dot` does not conflict with `Float` (the Float regex requires a digit after `.`; `.l` is `Dot
Identifier`) or with `DotDot`/`DotDotLt`/`DotDotGt` (longest-match still wins for `..`).

## Parser ([parser/](src/parser/))

### Array literal / range-init / comprehension (null denotation of `LBracket`)

`parse_expr` gains a null denotation for `LBracket`:

- Consume `[`; check for `]` immediately → error `"Array literal cannot be empty."`.
- Parse the first expression.
- **Comprehension check**: if the next token is `KwFor`, this is a comprehension:
  - Consume `for`, an `Identifier` (interned as `var`).
  - If `Colon` follows, consume it and parse the type name as an `Identifier` — resolved to
    `PinpType` using the same name→type mapping as function parameters (`"int"` → `Int`,
    `"float"` → `Float`, `"bool"` → `Bool`; unknown name → `ParseError::Unexpected`). Store
    as `var_type`. If no `Colon`, `var_type` defaults to `PinpType::Int`.
  - Consume `in`, then `parse_expr(0)` for the source range, then `]`.
  - Push `Node::Comprehension { element: first_expr, var, var_type, source }`.
- Otherwise it is a literal: continue parsing `,`-separated expressions with the same
  continuation/alignment rule as 0007's `parse_expr_list`.
  - Consume `]`.
  - Push `Node::ArrayLiteral { elements }`. Sema distinguishes range-init (sole element is a
    `Range` node) from the scalar literal form.

### Index access (left denotation of `LBracket`)

When `LBracket` is encountered after a parsed expression (left denotation, binding power above
comparison):

- Consume the index expression, then `]`.
- Push `Node::Index { array: left, index }`.

### Member access (left denotation of `Dot`)

When `Dot` is encountered after a parsed expression (same binding power band as index):

- Consume the following `Identifier` (else `ParseError::Unexpected`).
- Push `Node::Member { object: left, member: interned name }`.

### Indexed assignment

In `parse_stmt`, after parsing the leading expression list: if the list is a single `Index` node
and `=` follows (not a compound op), extract the `array` child — it must be `Var`/`Global` (else
`ParseError::Unexpected("Invalid indexed assignment target ...")`) — and build
`Stmt::IndexedAssign { target, index, value }` after parsing the RHS.

## Sema ([sema/](src/sema/))

### `assignable` / `join`

`Array(elem, len)` is assignable only to an identical `Array(elem, len)`; `join` of `Array`
with anything else returns `None`.

### `Node::ArrayLiteral`

- Empty element list: `ParseError` already rejects it; no sema check needed.
- If sole element types as `Range`: delegate to the `ArrayFromRange` path — see below.
- Otherwise: infer each element's type; compute the common type via `join` (same promotion lattice
  as elsewhere — `[1, 2.0]` yields `Float`). A type that does not join to any scalar (`Void`, 
  `Range`, mismatched) is `SemaError::Type("Inconsistent element types in array literal.")`.
- Result type: `PinpType::Array(element_type, elements.len())`.

### `Node::ArrayFromRange`

- The inner expression must be a `Node::Range` (not a `Var` of type `Range`) and every part
  (`start`, `stop`, `step`) must be a `Node::Int` literal. A variable bound is
  `SemaError::Type("Array range initialisation requires literal bounds.")`.
- Compute the element sequence from the literal parts; determine length. An empty range (zero
  elements) is `SemaError::Type("Array range initialisation yields an empty array.")`.
- Result type: `PinpType::Array(ElementType::Int, len)`.

### `Node::Index`

- `array` must resolve to `PinpType::Array(elem_type, _)` — else `SemaError::Type("Index target
  is not an array.")`.
- `index` must be `Int` or `Bool` (int-like) — else `SemaError::Type("Array index must be an
  integer.")`.
- Result type: the scalar `PinpType` matching `elem_type`.

### `Node::Member`

- `object` must be `PinpType::Array(_, _)` — else `SemaError::Type("Member access on a
  non-array.")`.
- Member name must be `"len"` — else `SemaError::Type("Unknown member \`{name}\`.")`.
- Result type: `PinpType::Int`.

### `Node::Comprehension`

- `source` must be a `Node::Range` with all-literal bounds and step (same validation as
  `ArrayFromRange`). A variable-bound range or a `Var` of type `Range` is
  `SemaError::Type("Comprehension source must be a literal-bound range.")`.
- Compute the element count from the range — must be non-zero (same empty-range check as
  `ArrayFromRange`).
- Validate `var_type`: `Int` → `var_type` must be `Int` or `Float` (i.e., `assignable(Int,
  var_type)` must hold). `Bool` is rejected: `SemaError::Type("Cannot promote range variable
  to bool.")`.
- Analyse `element` in a pushed scope frame seeded with `var: var_type`; `var` is
  **read-only** (added to `loop_vars`, same guard as `Stmt::For`).
- `element` must not be `Void` or `Range`.
- Result type: `PinpType::Array(element_type, len)` where `element_type` is the
  `ElementType` of the inferred element expression.

### `Stmt::IndexedAssign`

- Resolve `target` (local/global): must be `PinpType::Array(elem_type, _)`.
- `index` must be int-like.
- `value` must be assignable to the element type (same promotion as ordinary assignment).

### Entry result type

`gen_entry` currently rejects `PinpType::Range`; `PinpType::Array` is now permitted.

## Codegen ([codegen/](src/codegen/))

### LLVM type for `Array`

`basic_type(PinpType::Array(_, _))` returns an opaque pointer (`context.ptr_type(AddressSpace::default())`). An array value in the JIT is a heap pointer; a local/global slot holds that pointer. `zero(Array)` returns a null pointer constant.

`declare_globals` must use `basic_type` for array globals, which already works once `basic_type`
handles the variant.

### `Node::ArrayLiteral` and `Node::ArrayFromRange`

Both forms:

1. Determine element count `len` and LLVM element type from `ElementType`.
2. Compute `byte_count = len * element_size` (8 for `Int`/`Float`, 1 for `Bool`).
3. Emit `call i8* @pinp_alloc(i64 byte_count)` — returns the heap pointer.
4. For each element (or each value in the pre-computed range sequence): evaluate or construct the
   LLVM constant, then `store` via GEP at offset `i`.

For `ArrayFromRange`, the element values are compile-time `i64` constants (the bounds are
literals); no IR arithmetic is needed.

### `Node::Index`

1. Evaluate `array` → pointer; evaluate `index` as `i64`.
2. **Bounds check**: `if index < 0 or index >= len` call `pinp_runtime_error` (below) then
   `unreachable` (longjmp never returns — `build_unreachable()` tells LLVM this).
3. GEP to the element at `index`; `load` and return. For `Bool`, mask the low bit (same guard as
   the `i1` entry-return).

The length `len` is extracted from `ast.type_of(array)` — a compile-time constant, emitted as an
`i64` immediate.

### `Node::Member` (`.len`)

Emit the element count as a constant `i64`. No pointer dereference needed.

### `Node::Comprehension`

1. Extract the pre-computed range sequence (same literal arithmetic as `ArrayFromRange`) to
   determine `len` and the `i64` start/step values.
2. Allocate `len * element_size` bytes via `pinp_alloc`.
3. Emit a counted loop (same shape as `Stmt::For`): an `i64` counter starting at `start`,
   advancing by `step`, running `len` times. The counter is promoted to `var_type` (via the
   existing `promote` helper) before being stored in `var`'s entry-block alloca, so the
   element expression sees the annotated type without any extra work.
4. On each iteration: evaluate `element`, store via GEP at the current index, advance counter.

The loop is bounded by count (not by the range's stop comparison), so the termination condition
is simply `iter < len` — no direction logic needed.

### `Stmt::IndexedAssign`

Same bounds check as `Node::Index`, then GEP + `store` the (promoted) value.

### `PinpJit::run` — Array case

The entry function returns a pointer to the heap-allocated array. After calling it:

```rust
PinpType::Array(element_type, len) => {
    let f: extern "C" fn() -> *const u8 = self.jit.lookup(ENTRY)?;
    let ptr = f() as *const u8;
    let elements = (0..len).map(|i| unsafe {
        match element_type {
            ElementType::Int   => PinpValue::Int(*ptr.add(i * 8).cast::<i64>()),
            ElementType::Float => PinpValue::Float(*ptr.add(i * 8).cast::<f64>()),
            ElementType::Bool  => PinpValue::Bool(*ptr.add(i) != 0),
        }
    }).collect();
    PinpValue::Array(elements)
}
```

## Runtime upgrade ([runtime/](runtime/))

### Why the 0008 mechanism fails for nested calls

`raise_runtime_error` stores a code in a module global and emits `ret` — unwinding only one
stack frame. A bounds check inside a called function returns a garbage zero to its caller, which
keeps running. `longjmp` fixes this by unwinding the entire JIT call stack in one step.

### `gen_entry` signature change

The entry function changes from a type-returning function to `void __pinp_main(i8* result_ptr)`.
It writes its result through the pointer instead of returning it. For void programs the pointer
is not written. This uniform signature enables a single C trampoline to wrap any entry function.

### C additions (`runtime/shim.c`, `runtime/pinp_runtime.h`)

```c
// pinp_runtime.h
void pinp_runtime_error(const char *message);  // longjmps — never returns
void pinp_run(void (*entry)(void *), void *result, const char **error_out);

// shim.c
#include <setjmp.h>

static jmp_buf  pinp_jmpbuf;
static const char *pinp_pending_error = NULL;

void pinp_runtime_error(const char *message) {
    pinp_pending_error = message;
    longjmp(pinp_jmpbuf, 1);
}

void pinp_run(void (*entry)(void *), void *result, const char **error_out) {
    pinp_pending_error = NULL;
    *error_out = NULL;
    if (setjmp(pinp_jmpbuf) == 0) {
        entry(result);
    } else {
        *error_out = pinp_pending_error;
    }
}
```

`pinp_run` holds the `setjmp` frame and calls the entry function directly — the two are in the
same C activation record, which is required for `longjmp` to be well-defined.

### `build.rs`

`EXPORTED_SYMBOLS` gains `"pinp_runtime_error"` and `"pinp_run"`.

### Codegen changes

- Remove `RUNTIME_ERROR_SYMBOL`, `RUNTIME_ERROR_ZERO_STEP`, `runtime_error_message`,
  `runtime_error_global`, and `raise_runtime_error` from `codegen/mod.rs` and `codegen/stmt.rs`.
- Add `gen_runtime_error_call(message: &str)`: declares `@pinp_runtime_error(i8*)` as an
  external function (once, cached), adds a global string constant for `message`, emits a `call`,
  then `build_unreachable()`.
- The zero-step range guard (`guard_zero_step`) is updated to call `gen_runtime_error_call`
  instead of `raise_runtime_error`.
- `runtime_error` method removed from `PinpJit`; `run` no longer reads the error global.

### `PinpJit::run`

```rust
pub fn run(&self) -> Result<PinpValue, String> {
    let entry: extern "C" fn(*mut u8) = unsafe { self.jit.lookup(ENTRY)? };
    let mut result_buf = [0u8; 8];
    let mut error: *const i8 = std::ptr::null();

    extern "C" { fn pinp_run(entry: extern "C" fn(*mut u8), result: *mut u8, error: *mut *const i8); }
    unsafe { pinp_run(entry, result_buf.as_mut_ptr(), &mut error); }

    if !error.is_null() {
        let msg = unsafe { std::ffi::CStr::from_ptr(error) }.to_string_lossy().into_owned();
        return Err(msg);
    }

    Ok(unsafe {
        match self.result_type {
            PinpType::Bool  => PinpValue::Bool(result_buf[0] & 1 != 0),
            PinpType::Int   => PinpValue::Int(i64::from_ne_bytes(result_buf)),
            PinpType::Float => PinpValue::Float(f64::from_ne_bytes(result_buf)),
            PinpType::Void  => PinpValue::Void,
            PinpType::Array(element_type, len) => { /* pointer read as above */ }
            PinpType::Range => unreachable!("a program cannot evaluate to a range"),
        }
    })
}
```

## Test plan (TDD: red → green → refactor)

- **lexer.rs**: `[` `]` `.` tokenise; `.5` stays `Float`; `arr.len` → `Identifier Dot Identifier`;
  `1..5` unaffected.
- **parser.rs**: `[1, 2, 3]` → `ArrayLiteral`; `[0..5]` → `ArrayLiteral` with single `Range`
  child (sema decides); `arr[i]` → `Index`; `arr.len` → `Member`; `arr[i] = x` →
  `IndexedAssign`; `[]` is a parse error; multi-line literal with aligned continuation parses;
  misaligned continuation is a `Layout` error; `1[i]` (index on non-name) is a parse error.
- **sema.rs**: `[1, 2, 3]` → `Array(Int, 3)`; `[1, 2.0]` → `Array(Float, 2)` via promotion;
  mixed non-promotable types error; `[0..5]` → `Array(Int, 6)`; `[a..b]` (variable bounds)
  errors; `[range_var]` (variable of Range type) errors; empty range-init errors; `arr[i]` →
  element type; `arr.len` → `Int`; `arr[i] = 42` type-checks; `arr[i] = 1.0` for an `Int`
  array errors; index on non-array errors; unknown member errors.
- **parser.rs**: `[x*x for x in 1..5]` → `Comprehension`; `[x for x in arr]` is a parse error
  (array source not yet supported — `arr` parses as a `Var`, not a `Range`, sema catches it);
  `[x for x in 1..5 if x > 2]` is a parse error (no filter syntax yet).
- **sema.rs**: `[x*x for x in 1..5]` → `Array(Int, 5)`; `[x*0.5 for x in 1..4]` →
  `Array(Float, 4)`; `[x for x:float in 0..3]` → `Array(Float, 4)`; `[x for x:bool in
  0..3]` errors; variable-bound source errors; assigning to `x` inside the element
  expression is `"Cannot assign to loop variable."`; `Void` element expression errors.
- **codegen / tests/arrays.rs**: a literal `Int` array read back element-by-element; `.len`
  returns the count; mutation (`arr[i] = x`, then read back); `Float` array; `Bool` array;
  range-init `[1..5]` yields `[1, 2, 3, 4, 5]`; `[x*x for x in 1..5]` yields
  `[1, 4, 9, 16, 25]`; comprehension with a stepped range; comprehension element expression
  uses a variable from outer scope; OOB read yields `Err("Array index out of bounds.")`; OOB
  write yields same; negative index errors; a zero-step range (pre-existing) still yields
  `Err("Range step cannot be zero.")`; array as program result surfaces as
  `PinpValue::Array(...)`.

## Resolved (sign-off)

1. **`PinpType::Array(ElementType, usize)`** — element type + compile-time length; `ElementType`
   is a new `Copy` enum keeping `PinpType: Copy`.
2. **Literal init** (`[e1, e2, ...]`) and **range-init** (`[literal-range]`) both `ArrayLiteral`
   at parse time; sema distinguishes by inspecting the sole element's node kind.
3. **All sizes compile-time constant** — heap-allocated via `pinp_alloc`; no stack overflow risk.
4. **Index read** (`arr[i]`) and **indexed assignment** (`arr[i] = x`) with runtime bounds check.
5. **`.len`** emitted as an `i64` constant (no runtime indirection).
6. **`PinpValue::Array(Vec<PinpValue>)`** — `Copy` dropped from `PinpValue`.
7. **Comprehension** `[expr for var[:type] in literal-range]` — `var` bound as `var_type`
   (`Int` by default, or an explicitly annotated upward promotion); read-only, scoped to the
   element expression; size equals range length; codegen promotes the `i64` counter to
   `var_type` via `promote` before storing into `var`'s alloca.
8. **Runtime error upgrade**: `void __pinp_main(i8*)` + `pinp_run` C trampoline with `setjmp`;
   `pinp_runtime_error` longjmps at any depth; 0008 error-global removed.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.

