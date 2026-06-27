[Human]

Function definition has the following format:

```
function_name(param:type [ ,param:type]*)[: return-type] is
  <indented-body>
```

Square brackets denote optional.
If no return type is provided, `void` is assumed.
The arguments can go into next lines, but each one MUST start at the column where the first argument started.
The first argument must be defined in the first line.
Body lines must keep the indent the same as the first line.
The last expression is the return value.
`return` keyword will be available in the body inside control flow expressions - deferred.

Single line definitions are allowed. In such case `is` must be followed by return type definition and an expression.

Global variable access is achieved by `::<symbol>`

Examples:

```
fu(a:float, b:float, c:float): float is b^2 - 4*a*c

_fu_bar_baz_1(a:int, b:int): int is
    xx = a+b*b
    xx

_fu_bar_baz_2(a: float,
              b: float): float is
    a^2 + b^2

global1 =12

_i_use_a_global(a:int): int is
  ::global1 += 1
  a + ::global1
```

Note that compound assignment was introduced.
The scope also contains `+=` `-=` `*=` `/=` `^=` `mod=` `div=` operators.

```
x += 1
y *= x*2
z ^= x-1
w div= 3
v mod= 2
```

[AI]

> Decisions from sign-off are under **Resolved**; what still needs a ruling is under **Open**.

## In-scope grammar

```
program    = { top-level }
top-level  = func-def | statement                 // top-level statements run in the global scope
func-def   = signature "is" func-body
signature  = Ident "(" params? ")" ( ":" type )?  // ":" type is REQUIRED for the single-line form (#2)
func-body  = expression Newline                   // single-line form: `is` then an expression
           | Newline Indent block Dedent           // block form: indented body
block      = { statement Newline } expression Newline   // zero+ statements, then the result expression
params     = param { "," param }                  // newlines allowed between params (see "Multi-line params")
param      = Ident ":" type
type       = Ident                                // resolved against the type table: int|float|void

statement  = place ( "=" | aug-op ) expression    // assignment / compound assignment
           | expr-stmt
place      = Ident | "::" Ident                   // assignable location: local/param, or a global
aug-op     = "+=" | "-=" | "*=" | "/=" | "^=" | "div=" | "mod="   // compound assignment

primary    = Int | Float | Ident | call | "::" Ident | "(" expression ")"
call       = Ident "(" ( expression { "," expression } )? ")"
```

In both forms the body's final **expression** is the return value; in the block form it follows
zero or more statements. After `is`, a `Newline` selects the block form, otherwise the single-line
form (which must declare a return type, #2). `return` stays deferred. **Calls** `f(args)` are
expressions (#1); **`::name`** reads or assigns a global (#5); **`<op>=`** is lowered to
`place = place <op> expr`.

## Type system (iteration 2)

```
enum PinpType { Int, Float, Void }
```

- **Type annotations** resolve an `Ident` against `{ "int" -> Int, "float" -> Float, "void" -> Void }`,
  only in type position, so `int` stays a legal variable name elsewhere. Generalises to user types.
- **Return type:** block form may omit it ⇒ `Void`; single-line form must declare it (#2).
- **Return check:** the body result type must match the declared return type. `Int` is promotable to
  a declared `Float`; `Float -> Int` is an error (#3). Void: see Open #A.
- **Calls (#1):** arity must match the definition; each argument's type must be assignable to its
  parameter type (`Int -> Float` promotion allowed, `Float -> Int` error); the call expression's type
  is the function's return type. **Define-before-use** — no forward references, no recursion (#6).
- **Compound assignment:** `place <op>= e` is checked as `place = place <op> e`; `place` must already
  be bound (it is read), and the result must stay assignable to its type. The op inherits its
  operand rules, so `int_place /= …` is a type error (`/` yields `Float`) and `div=`/`mod=` require
  `Int` operands.

## Scoping (explicit globals)

The single flat table becomes a **scope stack**; the interner (`names`/`SymId`) stays global and
immutable.

- `scopes: Vec<Scope>`, `scopes[0]` is the global scope. Entering a function pushes a frame seeded
  with its parameters; the frame pops when the body ends.
- **Inside a function body, a bare `Ident` resolves to parameters/locals only — it does NOT reach
  globals (#5).** A global is read or assigned **only** through `::name`, which always targets
  `scopes[0]`. So `x` and `::x` are different things inside a function.
- A bare assignment in a body creates/updates a **local**; `::name = …` (or `::name <op>= …`) targets
  the global. Params/locals are invisible after the definition.
- Top-level statements operate directly on the global scope (a bare name at top level is global).

## Data model

- **Types:** `PinpType` gains `Void`.
- **Top-level elements:** `top_level: Vec<TopLevel>`, `enum TopLevel { Func(FuncDef), Stmt(Stmt) }`.
- **Function:**
  ```
  struct Param   { name: SymId, param_type: PinpType }
  struct Block   { stmts: Vec<Stmt>, result: ExprId }    // single-line: empty stmts, just result
  struct FuncDef { name: SymId, params: Vec<Param>, return_type: PinpType, body: Block }
  ```
  Definitions are registered in a name → signature table as they are parsed, so calls can be checked
  (arity + arg/return types) under define-before-use.
- **New expression nodes:** `Call { callee: SymId, args: Vec<ExprId> }`, `Global(SymId)` (a `::name`
  reference). Iteration-1 nodes unchanged.
- **Assignment target generalised:** `enum Place { Local(SymId), Global(SymId) }`,
  `Stmt::Assign { target: Place, rhs: ExprId }`. `<op>=` is expanded at parse time into an `Assign`
  whose `rhs` is a `Bin { op, lhs = <read of target>, rhs }`.

## Lexer

- **Re-add** `Colon` (`:`) and `Comma` (`,`); add `ColonColon` (`::`) — longest-match over `:`.
- **Compound-assign tokens:** `PlusEq` `+=`, `MinusEq` `-=`, `StarEq` `*=`, `SlashEq` `/=`,
  `CaretEq` `^=`, `DivEq` `div=`, `ModEq` `mod=`. Each is a single, **contiguous** token (longest-match
  over its bare operator + `=`). `div=`/`mod=` are explicit tokens too: a 4-char `div=` beats the
  3-char identifier `div`, while bare `div`/`mod` still fall through to the keyword pass — so spaced
  `div =` is NOT compound assignment.
- **Keyword table** gains `is -> KwIs`.
- **Source positions (#4a):** each `Token` carries a 1-based `line`/`column`, computed from a
  line-start table. This un-defers the source-location work; it feeds both the column-alignment check
  and future parser/sema diagnostics.
- **Multi-line params:** track paren depth and suppress structural `Newline`/`Indent`/`Dedent` while
  depth > 0 (bracket line-joining), so the indent stack reacts only to block indentation.
- **Column alignment (#4a):** the parser checks each continuation parameter begins at the first
  parameter's column (using token columns); a mismatch is an error.
- **Stricter body indent:** an unindent that matches no enclosing level is an `IndentError`.

## Test plan (TDD: red → green → refactor)

- single-line def with declared return type (`fu`, corrected): `FuncDef` shape + result type;
- block def with a local (`_fu_bar_baz_1`): statements + trailing `result`, result matches return;
- multi-line params (`_fu_bar_baz_2`): alignment accepted; plus a **misaligned** continuation arg → error;
- calls: correct arg types; arity mismatch → error; arg-type mismatch → error; `Int -> Float` arg
  promotion; calling a function defined later → define-before-use error;
- globals: `::global1` read in a body; `::global1 += 1` writes the global; a **bare** `global1` in a
  body → undefined (does not see the global);
- compound assignment: `+=` on a local and on `::global`; `<op>=` on an unbound target → error;
- scope: a param is usable; a local is visible later and invisible after; shadowing;
- type errors: result ≠ return (non-promotable); unknown type name; undefined symbol; duplicate param.

## Resolved (sign-off)

1. **Calls are in scope** — `f(args)` expressions, arg/arity/return type-checked.
2. **Single-line form must declare a return type;** block form may omit it ⇒ `Void`.
3. **`Int -> Float` promotion** for both returns and call arguments; `Float -> Int` is an error.
4. **(a) Source positions reintroduced now,** and the column-alignment rule is enforced.
5. **Globals are reached only via `::name`;** bare identifiers in a body never resolve to globals.
6. **No recursion, no nested/sub-function definitions** — top-level defs only, define-before-use.
7. **Void body:** a function with an omitted return type is `Void`, and its body's final expression
   must be `Void`-typed (e.g. a call to a void function); a value-typed final expression there is a
   type error — value-returning functions must declare their type.
8. **Compound assignment in scope (0002):** op-set `+= -= *= /= ^= div= mod=`; introduced here, not
   retrofitted into the merged 0001.

## Open (need a ruling)

None — ready to implement.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
