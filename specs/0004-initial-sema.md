[Human]

This is an internal iteration: **no new language features**. It introduces semantic-analysis
infrastructure — a dedicated `sema` pass — so that upcoming iterations (control flow first) land on a
clean `lex -> parse -> sema -> codegen` pipeline instead of piling more type/scope logic into an
already-overloaded parser. Doing it now, while the language is small, is the cheapest time to extract
it and avoids a larger refactor later.

[AI]

## Goal

Move semantic analysis out of the parser into its own pass, **preserving behaviour**: the same
language, the same accepted/rejected programs, and the same error messages. The end-to-end tests in
`tests/` must pass unchanged — they are the proof that nothing observable changed.

## Pipeline

```
lex  ->  parse (syntax)  ->  sema (types, scopes, checks)  ->  codegen
```

- **parse** becomes purely syntactic: tokens to a structural `Ast`. It still distinguishes `::name`
  from a bare name (that is lexical, via the `::` token) and still desugars `place <op>= e` (that is
  a structural rewrite, no types needed). It leaves the `types` arena unpopulated.
- **sema** walks the `Ast`, infers every node's `PinpType` (filling the `types` arena), owns the
  scope stack and the function-signature table, and runs every semantic check.
- **codegen** is unchanged — it still reads `ast.type_of(...)`, now populated by sema rather than the
  parser.

## What moves to `sema`

- Inline type inference (populating the parallel `types` arena).
- The scope stack and binding resolution — i.e. *is this name bound?*, parameters/locals vs globals.
- The function-signature table and call checking (arity, argument types, return type).
- The `assignable` rule (`Int -> Float` promotion) and all type validation.
- The semantic `PinpError` cases: `Type` and `UnknownSymbol`.

## What stays in `parser`

- Tokens to structural AST, the Pratt expression loop, layout handling.
- The syntactic `PinpError` cases: `Unexpected` and `Layout` (and `Lex`, forwarded from the lexer).

## Behaviour preservation

No semantic changes this iteration. In particular **define-before-use / no recursion stays as-is**
(see 0002). A sema pass *would* make forward references and recursion almost free (a declaration
phase before checking), but relaxing that is a deliberate language change for a later iteration, not
something to smuggle into a behaviour-preserving refactor.

## Test impact

- **Move** the semantic-error unit tests from `parser.rs` to a new `sema.rs` `#[cfg(test)]` module —
  e.g. `call_arity_mismatch_is_error`, `return_float_to_int_is_error`, `unassigned_symbol_is_error`,
  call/arg-type and global/scope checks. Per the test-organization convention, semantic-error tests
  belong in the sema layer.
- **Keep** the parser's syntactic-error tests (`misaligned_param_is_error`,
  `inconsistent_dedent_is_error`, `duplicate_param_is_error`, `unknown_type_name_is_error`) on the
  parser side — duplicate params and unknown type names are caught while building the structural AST.
- **Untouched:** every end-to-end test in `tests/` and the lexer tests.

## Resolved (sign-off)

1. **Error model is split:** `parse(src) -> Result<Ast, ParseError>` (syntactic — `Lex`,
   `Unexpected`, `Layout`) and `analyze(&mut Ast) -> Result<(), SemaError>` (semantic — `Type`,
   `UnknownSymbol`). `PinpJit` maps both into its `Result<_, String>`. The four structural checks the
   parser keeps (duplicate parameter, unknown type name, single-line-needs-return-type,
   body-must-end-with-expression) become `ParseError::Unexpected` (messages unchanged); no extra
   variant is added.
4. **Assignment checking is uniform (no plain/compound distinction).** The parser desugars
   `x <op>= e` to `x = (x <op> e)` and sema does not track which it was; both require the result type
   be assignable to the target's existing type. This slightly tightens plain re-assignment to an
   incompatible type (no test exercises it) in exchange for a simpler sema.
2. **Define-before-use / no recursion is retained** this iteration; relaxing recursion is a separate
   future decision.
3. **Type-annotation resolution stays in the parser.** Mapping a `int`/`float`/`void` annotation to a
   `PinpType` is a fixed lexical lookup needed to build the structural AST (`Param.param_type`,
   `FuncDef.return_type`), so it — and the `unknown_type_name_is_error` test — stay on the parser
   side rather than forcing the AST to carry unresolved type names.

## Open (need a ruling)

None — ready to implement.

---

> **Disclaimer:** Human + AI generated documentation. Treat it with a little salt — it may not
> always reflect reality.
