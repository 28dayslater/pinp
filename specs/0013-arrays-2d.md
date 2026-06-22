[Human]

# 2D arrays — literals, indexing, slicing, built-in members, iteration

This spec continues [0012-arrays.md](0012-arrays.md). The 2D matrix literal syntax and `[i, j]`
indexing were designed in 0012 and are implemented here.

## Scope of this iteration (0013)

In scope:
- 2D matrix literal initialisation (designed in 0012; implemented here)
- `mat[i, j]` element read and write with runtime bounds check on both dimensions
- 2D slice access: all four index/range combinations (scalar, 1D, 2D result)
- `.ndim` built-in member on all arrays (1D → 1, 2D → 2)
- `.rows` and `.cols` built-in members on 2D arrays; sema error on 1D
- `.len` for 2D returns `rows * cols` (overrides the tuple design noted in 0012)
- `for val in arr` and `for idx, val in arr` — 1D array iteration
- `for val in mat` and `for row, col, val in mat` — 2D array iteration (row-major)
- `identity(n, type)` built-in for square identity matrices
- Returning a 2D array to the host via `PinpValue`

Deferred to subsequent iterations:
- `.shape` — full shape as a tuple; deferred until tuples exist
- Scalar-fill and array-copy slice-assign on 2D
- 2D comprehension
- ND (rank > 2) arrays

## Built-in members

```
All arrays:  .len   .ndim
1D only:     (nothing extra — .rows/.cols on 1D is a sema error)
2D only:     .rows  .cols
```

`.ndim` returns an `Int` — the number of dimensions. It is not the shape; a 2D matrix of any
size returns `2`.

For 2D, the invariant `.len == .rows * .cols` holds exactly. All three members are compile-time
constants (the shape is always known at compile time).

`.rows` or `.cols` on a 1D array is a sema error: `".rows is not defined for a 1D array. Use .len."`.

## 2D matrix literals

Unchanged from 0012 design. `;` separates rows, `,` separates elements within a row. Column count
is taken from the first row; subsequent rows must match or it is a compile-time error. A trailing
`;` is a parse error.

```
mat = [1.0, .0, .0;
       .0, 1.0, .0;
       .0, .0, 1.0]     # 3×3 float identity matrix

mat = [1, 2, 3; 4, 5, 6]     # 2×3 int matrix, single-line form
```

The continuation rule from 0012 applies: when the literal spans multiple lines, the first element
of each subsequent row must start at the column of the first element of the first row.

## Element indexing

```
x = mat[i, j]      # read element at row i, column j (zero-based)
mat[i, j] = x      # write
```

Both indices are runtime-checked against their respective dimension bounds. Out-of-bounds is a
runtime error (same mechanism as 1D).

## Slicing

Slice ranges must have literal bounds and no step — the same constraints as 1D slices. Sema
checks literal-bound slices against the compile-time shape and rejects out-of-bounds ranges.
A `:` in a dimension position means the full extent of that dimension (syntactic sugar for
`0..last`).

| Form | Result type | Description |
|------|-------------|-------------|
| `M[i, j]` | scalar | element at row `i`, column `j` |
| `M[i, c1..c2]` | 1D array | row `i`, columns `c1..c2` |
| `M[r1..r2, j]` | 1D array | rows `r1..r2`, column `j` |
| `M[r1..r2, c1..c2]` | 2D matrix | submatrix |

Both `M[i, c1..c2]` and `M[r1..r2, j]` return a plain flat 1D array. There is no row-vector or
column-vector type — orientation is only meaningful for matrix multiplication, which is not in
scope.

Variable-bound slices are a sema error (consistent with 1D policy).

## Iteration

Array iteration follows Python's `enumerate` convention: indices come before the value. The
parser collects all binders before `in` and sema validates the count against the array rank.

### 1D

```
for val in arr              # value only
for idx, val in arr         # index (zero-based Int), then value
```

### 2D

```
for val in mat              # value only, row-major order
for row, col, val in mat    # row index, column index, then value — all row-major
```

Elements are visited in row-major order in both forms: row index varies slowest, column fastest.

### Binder count rules (validated by sema)

| Binder count | 1D array | 2D matrix |
|---|---|---|
| 1 | value | value |
| 2 | index, value | sema error |
| 3 | sema error | row, col, value |

Any other count is a sema error regardless of rank.

The binder variables are read-only within the loop body (same guard as the range `for` loop).

## Built-in functions

### `identity`

```
mat = identity(3, int)    # 3×3 int identity matrix
mat = identity(4, float)  # 4×4 float identity matrix
```

`identity` is a compiler-known built-in, not a user-callable function defined in pinp source.
Its first argument must be a literal integer `n >= 2` (sema error otherwise); its second argument
must be a type name — `int` or `float` (`bool` is a sema error). The result type is
`Matrix(type, n, n)`. Codegen emits all elements as compile-time constants — no runtime
arithmetic.

The matrix shape `(rows, cols)` is encoded in the type (the AI section designs the exact
`PinpType` variant). This is required for `.rows`/`.cols` to be compile-time constants and for
slice bounds to be checked at compile time.

## PinpValue

A 2D matrix returned from a program is surfaced to the host with its shape. The exact
`PinpValue` variant is an AI design decision; it must convey element type, row count, column
count, and the element sequence in row-major order.

---

[AI]

## Goal

Implement **2D matrix** support: literal initialisation, element read/write, slicing (all four
index/range combinations), built-in members (`.ndim`, `.rows`, `.cols`, `.len`), the
`identity(n, type)` built-in, 1D and 2D array `for`-in iteration, and a returnable
`PinpValue::Matrix`. Every step is written test-first; the test plan appears at the end of this
section.

---

## Step 1 — Data model ([parser/ast.rs](src/parser/ast.rs))

### `PinpType::Matrix`

```rust
/// A 2D matrix of `rows × cols` elements of `ArrayElementType`, heap-allocated, row-major.
Matrix(ArrayElementType, usize, usize),   // (elem_type, rows, cols)
```

`Matrix` keeps `PinpType: Copy` for the same reason `Array` does: it stores `ArrayElementType`
(not a boxed `PinpType`) plus two `usize` shape fields. Every existing `match` on `PinpType`
gains a `Matrix` arm; the compiler enforces exhaustiveness.

### New `Node` variants

```rust
/// `[r0_e0, r0_e1, …; r1_e0, …; …]` — 2D matrix literal. Each inner Vec is one row; every
/// row must have the same length (checked by sema). Element type is deduced from all elements.
MatrixLiteral { rows: Vec<Vec<ExprId>> },

/// `matrix[row_sel, col_sel]` — 2D index or slice. Each selector is one of:
///   • a scalar expression (Int) → index that dimension
///   • a `Node::Range` with literal bounds → slice that dimension
///   • `Node::FullExtent` → full extent of that dimension (`:` syntax)
/// Sema determines the result type from the selector kinds.
Index2D { matrix: ExprId, row: ExprId, col: ExprId },

/// `:` in a 2D slice position — full extent of that dimension. Resolved by sema to
/// `0..rows-1` or `0..cols-1` depending on position. Never valid outside a 2D slice.
FullExtent,
```

### `Stmt` changes

```rust
/// Rename `range` → `source` in the existing range-for; sema now also accepts Array and
/// Matrix sources when binders == 1.
Stmt::For { var: SymId, source: ExprId, body: Block }

/// Multi-binder array/matrix iteration. binders.len() == 2 → 1D (idx, val);
/// binders.len() == 3 → 2D (row, col, val). Validated by sema.
Stmt::ForArray { binders: Vec<SymId>, source: ExprId, body: Block }

/// `mat[row, col] = value` — write one element of a 2D matrix in place.
Stmt::IndexedAssign2D { target: Place, row: ExprId, col: ExprId, value: ExprId }
```

`Stmt::For`'s field rename (`range` → `source`) is mechanical: two match sites in sema and
codegen, updated in this step. Existing range-for behaviour is unchanged.

### `PinpValue::Matrix`

```rust
Matrix { rows: usize, cols: usize, elements: Vec<PinpValue> },
```

Elements are in row-major order. `PinpValue` already gave up `Copy` in 0012; adding `Matrix`
(which also contains a `Vec`) needs no further changes to the derive list.

---

## Step 2 — Lexer ([src/lexer.rs](src/lexer.rs))

Add `Semicolon` to both `Lexeme` and `TokenKind`; map `;` → `Lexeme::Semicolon` in the
character dispatcher before the number/identifier/symbol matchers. The `;` character is
currently unrecognised; it appears in no existing test source, so no existing test is affected.

---

## Step 3 — Parser: matrix literal ([src/parser/expr.rs](src/parser/expr.rs))

Extend the `LBracket` null denotation (which already handles `ArrayLiteral` and
`Comprehension`). After parsing the first row's element list:

- If `Semicolon` follows → matrix mode.
  - Parse subsequent `;`-separated rows; each row is a `,`-separated expression list using the
    same continuation/alignment rule as 1D (`parse_expr_list`).
  - A trailing `;` with no elements after it → `ParseError` ("Trailing `;` in matrix literal.").
  - Consume `]`.
  - Push `Node::MatrixLiteral { rows }`.
- Otherwise → existing 1D path (no change).

The continuation-column rule uses the first element of the first row as the anchor; every row
that starts on a new line must align its first element to that column.

---

## Step 4 — Parser: 2D indexing and slicing ([src/parser/expr.rs](src/parser/expr.rs))

Extend the `LBracket` left denotation (which already handles 1D `Node::Index`). After parsing
the first dimension selector:

- If `,` follows → 2D: parse the second dimension selector, then `]`. Push `Node::Index2D`.
- Otherwise → existing 1D path (no change).

Parsing a dimension selector (used for both dimensions):

- If the current token is `Colon` (and the next token is `Comma` or `RBracket`) → consume
  `Colon`, push `Node::FullExtent`.
- Otherwise → `parse_expr(0)` (yields a scalar expression or a `Node::Range`).

---

## Step 5 — Parser: ForArray and IndexedAssign2D ([src/parser/stmt.rs](src/parser/stmt.rs))

### `Stmt::ForArray`

In `parse_for`, collect a comma-separated list of identifiers before `in`. If the list has
exactly one identifier, build `Stmt::For { var: binders[0], source, body }` (existing shape,
with the field renamed from `range` to `source`). If the list has two or three identifiers,
build `Stmt::ForArray { binders, source, body }`. Four or more identifiers → `ParseError`
("Too many binders in for loop.").

### `Stmt::IndexedAssign2D`

In `parse_stmt`, the indexed-assign path already inspects the parsed leading expression. If the
leading expression is `Node::Index2D` and `=` follows (no compound op), extract `target` (must
be `Var`/`Global`, else `ParseError`), parse the RHS, and build `Stmt::IndexedAssign2D`.

---

## Step 6 — Sema: Matrix type integration ([src/sema/](src/sema/))

Extend `join` and `assignable`:

- `join(Matrix(e, r, c), Matrix(e, r, c))` → `Some(Matrix(e, r, c))` (exact match only —
  two matrices with different shapes are distinct types, like `Array`).
- `join(Matrix(…), anything-else)` → `None`.
- `assignable(Matrix(e, r, c), Matrix(e, r, c))` → `true`; any other combination → `false`.

Extend `gen_entry` to permit `PinpType::Matrix` as a valid return type (alongside `Array`).

---

## Step 7 — Sema: matrix literal ([src/sema/expr.rs](src/sema/expr.rs))

`Node::MatrixLiteral { rows }`:

- Each row must have the same number of elements as the first row; a mismatch is
  `SemaError::Type("All matrix rows must have the same number of columns.")`.
- Infer every element's type; compute the common type via `join` across all elements in all
  rows (same promotion lattice as `ArrayLiteral`). A non-joinable element type is
  `SemaError::Type("Inconsistent element types in matrix literal.")`.
- Result type: `PinpType::Matrix(element_type, row_count, col_count)`.

---

## Step 8 — Sema: 2D indexing and slicing ([src/sema/expr.rs](src/sema/expr.rs))

`Node::Index2D { matrix, row, col }`:

- `matrix` must resolve to `PinpType::Matrix(elem_type, rows, cols)` — else
  `SemaError::Type("2D index target is not a matrix.")`.
- `Node::FullExtent` in `row` position → rewritten internally to the range `0..rows-1`;
  `Node::FullExtent` in `col` position → `0..cols-1`.
- Classify each selector after resolving `FullExtent`:
  - Scalar (`Int`/`Bool`) → **index**: will be runtime-checked.
  - `Node::Range` with all-literal bounds, no step → **slice**: bounds checked at compile time
    against `rows` or `cols` respectively; an out-of-range literal is
    `SemaError::Type("Slice index out of bounds.")`.
  - Anything else → `SemaError::Type("Slice requires a literal-bound range without step.")`.
- Result type:

  | row selector | col selector | result |
  |---|---|---|
  | index | index | scalar `PinpType` for `elem_type` |
  | index | slice (len L) | `PinpType::Array(elem_type, L)` |
  | slice (len L) | index | `PinpType::Array(elem_type, L)` |
  | slice (len R) | slice (len C) | `PinpType::Matrix(elem_type, R, C)` |

---

## Step 9 — Sema: built-in members ([src/sema/expr.rs](src/sema/expr.rs))

Extend `Node::Member` handling:

| member | on `Array(_, n)` | on `Matrix(_, r, c)` | on anything else |
|---|---|---|---|
| `len` | `Int` (existing) | `Int` (= r\*c) | existing error |
| `ndim` | `Int` | `Int` | `SemaError::Type("`.ndim` is not defined on …")` |
| `rows` | sema error: `".rows is not defined for a 1D array. Use .len."` | `Int` | sema error |
| `cols` | sema error: `".cols is not defined for a 1D array. Use .len."` | `Int` | sema error |
| anything else | existing unknown-member error | same | same |

---

## Step 10 — Sema: `identity` built-in ([src/sema/expr.rs](src/sema/expr.rs))

`Node::Call` with `callee == "identity"` is intercepted before the normal function-call path:

- Argument count must be exactly 2; else `SemaError::Type("identity() takes exactly 2 arguments.")`.
- Arg 0 must be `Node::Int(n)` with `n >= 2`; else
  `SemaError::Type("identity() size must be a literal integer >= 2.")`.
- Arg 1 must be `Node::Var` whose name is `"int"` or `"float"`; `"bool"` →
  `SemaError::Type("identity() does not support bool element type.")`;
  anything else → `SemaError::Type("identity() type must be int or float.")`.
- Result type: `PinpType::Matrix(element_type, n as usize, n as usize)`.

A user-defined function named `identity` is shadowed by the built-in; this is intentional and
documented.

---

## Step 11 — Sema: `ForArray` and extended `For` ([src/sema/analyzer.rs](src/sema/analyzer.rs))

### Extended `Stmt::For` (1-binder, array/matrix source)

When `source` resolves to `PinpType::Array(elem_type, _)` or `PinpType::Matrix(elem_type, _, _)`:
- Seed the loop scope with `var: PinpType::from(elem_type)` (read-only via `loop_vars`).
- Analyse `body`.

When `source` resolves to `PinpType::Range` → existing range-for behaviour unchanged.

Any other source type → `SemaError::Type("Cannot iterate over …")`.

### `Stmt::ForArray`

Source must resolve to `Array` or `Matrix`:

- 2 binders + `Array(elem_type, _)`:
  - `binders[0]` (index) → `PinpType::Int`, read-only.
  - `binders[1]` (value) → `PinpType::from(elem_type)`, read-only.
- 3 binders + `Matrix(elem_type, _, _)`:
  - `binders[0]` (row) → `PinpType::Int`, read-only.
  - `binders[1]` (col) → `PinpType::Int`, read-only.
  - `binders[2]` (value) → `PinpType::from(elem_type)`, read-only.
- Any other binder count / source rank combination →
  `SemaError::Type("Binder count does not match array rank.")`.

All binders are pushed to `loop_vars` for the read-only guard.

### `Stmt::IndexedAssign2D`

- `target` must resolve to `PinpType::Matrix(elem_type, rows, cols)`.
- `row` and `col` must each be int-like.
- `value` must be assignable to `elem_type` (same promotion as 1D `IndexedAssign`).

---

## Step 12 — Codegen: Matrix type ([src/codegen/mod.rs](src/codegen/mod.rs))

- `basic_type(PinpType::Matrix(_, _, _))` → opaque pointer (same as `Array`; a matrix is a
  heap pointer to a contiguous row-major block).
- `zero(Matrix(_, _, _))` → null pointer constant.
- `declare_globals`: `Matrix` globals use `basic_type`, which already returns a pointer; no
  additional change needed.

---

## Step 13 — Codegen: matrix literal ([src/codegen/expr.rs](src/codegen/expr.rs))

`Node::MatrixLiteral { rows }`:

1. `total = row_count * col_count`; `byte_count = total * element_size`.
2. Emit `call i8* @pinp_alloc(i64 byte_count)`.
3. For each element at `(r, c)`: compute flat offset `r * col_count + c`, evaluate the element
   expression, `store` via GEP. Element coercion follows the common `elem_type` determined by
   sema (same promotion as `ArrayLiteral`).

---

## Step 14 — Codegen: `identity` ([src/codegen/expr.rs](src/codegen/expr.rs))

`Node::Call` intercepted for `identity`:

1. Extract `n` and `element_type` from the node's resolved `PinpType::Matrix`.
2. Allocate `n * n * element_size` bytes.
3. Emit `n*n` stores: at offset `i*n + j`, store `1` (or `1.0`) if `i == j`, else `0` (or
   `0.0`). All values are LLVM compile-time constants; no branches emitted.

---

## Step 15 — Codegen: 2D indexing and slicing ([src/codegen/expr.rs](src/codegen/expr.rs))

`Node::Index2D` — dispatch on sema-determined result type:

**Scalar result** (`mat[i, j]`):
1. Load matrix pointer. Evaluate `row` and `col` as `i64`.
2. Runtime bounds check: `row < 0 || row >= rows` or `col < 0 || col >= cols` → `gen_runtime_error_call`.
3. Flat offset `row * cols + col`; GEP + load.

**1D row slice** (`mat[i, c1..c2]`):
1. Runtime-check `row` index; compile-time bounds already verified by sema.
2. Slice length `L = c2 - c1 + 1`; allocate `L * element_size` bytes.
3. Loop `k in 0..L`: GEP source at `row * cols + c1 + k`, GEP dest at `k`, store.

**1D column slice** (`mat[r1..r2, j]`):
1. Runtime-check `col` index; sema verified row range.
2. Slice length `L = r2 - r1 + 1`; allocate `L * element_size` bytes.
3. Loop `k in 0..L`: GEP source at `(r1 + k) * cols + col`, GEP dest at `k`, store.

**2D submatrix** (`mat[r1..r2, c1..c2]`):
1. Both ranges sema-verified; compute `R = r2-r1+1`, `C = c2-c1+1`.
2. Allocate `R * C * element_size` bytes.
3. Nested loop `r in 0..R, c in 0..C`: GEP source at `(r1+r)*cols + c1+c`, GEP dest at
   `r*C + c`, store.

The copy loops for slices follow the same counted-loop shape as `Comprehension`.

---

## Step 16 — Codegen: built-in members ([src/codegen/expr.rs](src/codegen/expr.rs))

`Node::Member` extended:

- `.ndim` on `Array(_, _)` → `i64` constant `1`.
- `.ndim` on `Matrix(_, _, _)` → `i64` constant `2`.
- `.rows` on `Matrix(_, rows, _)` → `i64` constant `rows`.
- `.cols` on `Matrix(_, _, cols)` → `i64` constant `cols`.
- `.len` on `Matrix(_, rows, cols)` → `i64` constant `rows * cols`.

All are compile-time constants; no pointer load needed.

---

## Step 17 — Codegen: `for`-array iteration ([src/codegen/stmt.rs](src/codegen/stmt.rs))

### Extended `Stmt::For` (1-binder, Array/Matrix source)

Source is `Array(elem_type, n)` or `Matrix(elem_type, rows, cols)` with total count
`total = n` or `rows*cols`:

1. Load the heap pointer.
2. Emit a counted loop `i in 0..total` (same shape as `Comprehension`).
3. On each iteration: GEP at offset `i`, load element, store into `var`'s alloca (promoted to
   `elem_type` for `Bool`). Body executes with `var` in scope.

### `Stmt::ForArray`

**2 binders, `Array(elem_type, n)`** (`for idx, val in arr`):

1. Counter `i` is both the index and the GEP offset.
2. Alloca for `idx` (Int) and `val` (elem_type). On each iteration: store `i` into `idx`,
   load element and store into `val`.

**3 binders, `Matrix(elem_type, rows, cols)`** (`for row, col, val in mat`):

1. Single flat counter `i in 0..rows*cols`.
2. Alloca for `row`, `col`, `val`. On each iteration:
   - `row = i / cols` (integer division), `col = i mod cols`.
   - Load element at offset `i`, store into `val`.
   - Store `row` and `col` into their allocas.

All binders are pushed into the loop scope read-only (via `loop_vars`).

---

## Step 18 — Codegen: `IndexedAssign2D` ([src/codegen/stmt.rs](src/codegen/stmt.rs))

Same structure as `IndexedAssign` for 1D:

1. Load matrix pointer from `target`'s alloca.
2. Evaluate `row` and `col` as `i64`; runtime bounds check (same `gen_runtime_error_call` path).
3. Flat offset `row * cols + col`; GEP + store (promoted value if element type requires it).

---

## Step 19 — `PinpJit::run` — Matrix result ([src/codegen/mod.rs](src/codegen/mod.rs))

```rust
PinpType::Matrix(element_type, rows, cols) => {
    let f: extern "C" fn() -> *const u8 = self.jit.lookup(ENTRY)?;
    let ptr = f() as *const u8;
    let total = rows * cols;
    let elements = (0..total).map(|i| unsafe {
        match element_type {
            ArrayElementType::Int   => PinpValue::Int(*ptr.add(i * 8).cast::<i64>()),
            ArrayElementType::Float => PinpValue::Float(*ptr.add(i * 8).cast::<f64>()),
            ArrayElementType::Bool  => PinpValue::Bool(*ptr.add(i) != 0),
        }
    }).collect();
    PinpValue::Matrix { rows, cols, elements }
}
```

---

## Test plan (TDD: red → green → refactor)

Tests are written first. In-module unit tests cover component contracts; `tests/matrices.rs`
covers end-to-end behaviour via `PinpJit`.

### Lexer (`src/lexer.rs`)

- `;` → `Semicolon`; appears between `]` and a digit without triggering float or other rules.
- Existing tokens (`.`, `..`, `:`, `,`, `[`, `]`) still lex correctly around `;`.

### Parser unit tests (`src/parser/`)

- `[1, 2; 3, 4]` → `MatrixLiteral { rows: [[1,2],[3,4]] }`.
- Multi-line matrix with aligned rows parses; misaligned row is a layout error.
- Trailing `;` → parse error.
- `mat[i, j]` → `Index2D` with two scalar selectors.
- `mat[i, 1..3]` → `Index2D` with one scalar, one Range.
- `mat[1..2, 1..3]` → `Index2D` with two Ranges.
- `mat[i, :]` and `mat[:, j]` → `Index2D` with one scalar and one `FullExtent`.
- `mat[:, :]` → `Index2D` with two `FullExtent`.
- `mat[i, j] = x` → `IndexedAssign2D`.
- `for idx, val in arr` → `ForArray { binders: [idx, val], … }`.
- `for row, col, val in mat` → `ForArray { binders: [row, col, val], … }`.
- `for val in mat` → `Stmt::For { var: val, … }`.
- `for a, b, c, d in x` → parse error (too many binders).

### Sema unit tests (`src/sema/`)

- `[1, 2; 3, 4]` → `Matrix(Int, 2, 2)`.
- `[1, 2; 3, 4.0]` → `Matrix(Float, 2, 2)` via promotion.
- Jagged rows → sema error.
- `mat[i, j]` on a `Matrix(Int, 3, 3)` → `Int`.
- `mat[i, 1..2]` → `Array(Int, 2)`.
- `mat[0..1, j]` → `Array(Int, 2)`.
- `mat[0..1, 1..2]` → `Matrix(Int, 2, 2)`.
- `mat[:, j]` → `Array(Int, rows)`.
- Out-of-bounds literal slice → sema error.
- Variable-bound slice → sema error.
- `.ndim` on `Array` → `Int`; on `Matrix` → `Int`.
- `.rows` / `.cols` on `Matrix` → `Int`; on `Array` → sema error.
- `.len` on `Matrix(_, 3, 4)` → `Int` (= 12).
- `identity(3, int)` → `Matrix(Int, 3, 3)`.
- `identity(3, float)` → `Matrix(Float, 3, 3)`.
- `identity(1, int)` → sema error (n < 2).
- `identity(3, bool)` → sema error.
- `identity(3)` → sema error (wrong arg count).
- `for val in mat` → `val` has elem type; write to `val` → sema error.
- `for idx, val in mat` → sema error (2 binders on 2D).
- `for row, col, val in arr` → sema error (3 binders on 1D).

### End-to-end tests (`tests/matrices.rs`)

- 2×3 int matrix reads back as `PinpValue::Matrix { rows:2, cols:3, … }` with correct elements.
- Float matrix round-trips.
- Element read `mat[i, j]` returns correct value.
- Element write `mat[i, j] = x` persists.
- OOB row index → `Err("Array index out of bounds.")`.
- OOB col index → same.
- Row slice `mat[0, 1..2]` → correct 1D array.
- Column slice `mat[0..1, 0]` → correct 1D array.
- Submatrix slice `mat[0..1, 0..1]` → correct 2D matrix.
- Full-extent slice `mat[:, 0]` → full first column as 1D.
- `.ndim` returns 1 for 1D array, 2 for 2D matrix.
- `.rows` / `.cols` return correct shape.
- `.len` on matrix returns `rows * cols`.
- `identity(3, int)` → 3×3 identity matrix.
- `identity(3, float)` → 3×3 float identity.
- `for val in arr` visits all elements in order.
- `for idx, val in arr` gives correct index alongside value.
- `for val in mat` visits all elements in row-major order.
- `for row, col, val in mat` gives correct row/col/value triples.
- Loop variable write → sema error (enforced by existing guard).

---

## Resolved (sign-off)

1. **`PinpType::Matrix(ArrayElementType, usize, usize)`** — keeps `PinpType: Copy`; shape
   baked into type enables compile-time member constants and slice bounds checking.
2. **`.len` on 2D** returns `rows * cols` (not a tuple as noted in 0012 — overridden here).
3. **`.rows` / `.cols` on 1D** is a sema error; `.ndim` is valid on all ranks.
4. **Slice selectors** — scalar (runtime-checked), literal range (compile-time checked),
   FullExtent (`:`, resolved by sema to the full dimension).
5. **Flat 1D result** for mixed index+range slice — no row/column vector distinction.
6. **Iteration binder order**: index(es) first, value last — Python `enumerate` convention.
7. **`Stmt::For`** extended to accept Array/Matrix source with 1 binder; `Stmt::ForArray`
   added for 2- and 3-binder forms. Parser decides by binder count.
8. **`identity(n, type)`** — compiler-known built-in; `n >= 2` literal; `int` or `float` only;
   all elements emitted as compile-time LLVM constants.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
