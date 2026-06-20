[Human]

parser, codegen and sema are becoming large and need to be split into logical modules.
parts need to go into sub-directories for each subsystem.

[AI]

## Goal

A **behavior-preserving** refactor: split the three large single-file modules — `parser.rs`
(~2100 LOC), `codegen.rs` (~1500 LOC), and `sema.rs` (~1150 LOC) — each into a subdirectory of
cohesive files, organised **by responsibility**. No language, grammar, or codegen behavior changes.
The public surface is unchanged: every existing path (`pinp::parser::{Ast, Node, …}`,
`pinp::codegen::{PinpJit, PinpValue}`, `pinp::sema::{analyze, SemaError}`) resolves exactly as
before, because each new `mod.rs` re-exports its submodules' public items. The 175 existing tests
must pass unchanged.

## Approach

`lexer.rs` (~550 LOC) stays a single file — it is already a cohesive unit. Each of the three large
modules becomes a directory `src/<name>/` with a `mod.rs` plus responsibility files. The big
`impl Parser` / `impl CodeGen` / `impl Analyzer` blocks are split across files as **multiple `impl`
blocks on the same type** — Rust allows this. Inherent private methods are visible to the module
their `impl` sits in *and its descendants*; so the shared cursor/helper core lives in the parent
`mod.rs` (freely reachable from every child), and only the handful of methods called *across* sibling
child files are widened to `pub(super)`. Each child file opens with `use super::*;` (the pieces of
one module) plus any direct external imports it needs. Tests stay one `tests.rs` per subsystem.

## parser → `src/parser/`

- `mod.rs` — module doc, `mod`/`pub use ast::*` re-exports, `ParseError`, `pub fn parse`, the
  `Parser` struct, `MAX_NESTING_DEPTH`, and the cursor core (`peek`/`at`/`advance`/`enter_nesting`/
  `expect`/`skip_separators`).
- `ast.rs` — the data model and arena: `PinpType`, `SymId`, `ExprId`, `BinOp`, `UnOp`, `Node`,
  `RangeKind`, `IfArm`, `Place`, `Stmt`, `Param`, `Block`, `FuncDef`, `TopLevel`, `Ast` (+ its
  `node`/`type_of`/`push`/`intern`), and `From<Place> for Node`.
- `expr.rs` — Pratt expression parsing: `parse_expr` (`pub(super)`), `parse_comparison_chain`,
  `parse_range_tail`, `parse_prefix`, `parse_primary`, `parse_call`, and the if-**expression**
  (`parse_if`, `parse_conditional`); the binding-power tables and band consts (`infix`,
  `comparison_op`, `range_kind`, the `*_BP` / `MAX_IF_ARMS` consts, `ChainDirection`,
  `chain_direction`, `comparison_symbol`); and the literal converters (`parse_int`, `parse_float`,
  `check_monotonic`).
- `item.rs` — program structure and statements: `parse_program`, `parse_func_def`/`parse_params`/
  `parse_type`/`parse_func_body`, `parse_block` (`pub(super)`), `parse_while`/`parse_loop`/
  `parse_for`, `parse_stmt` and the multi-assignment list/continuation/place helpers; plus
  `compound_assign_op`.
- `tests.rs` — the existing `#[cfg(test)] mod tests`.

Cross-file surface is just two methods: `item` → `expr` via `parse_expr`; `expr` → `item` via
`parse_block`.

## sema → `src/sema/`

- `mod.rs` — module doc, re-exports, `SemaError`, the `pub fn analyze` entry, the `Analyzer` struct
  and `Signature`, the free type helpers (`assignable`/`join`/`numeric`/`int_like`), and the small
  shared accessors.
- `analyzer.rs` — the `Analyzer` impl (statement and expression checking).
- `tests.rs` — the existing tests.

## codegen → `src/codegen/`

- `mod.rs` — module doc, re-exports, the `ENTRY`/runtime-error constants and `runtime_error_message`,
  the host-facing `PinpJit`/`PinpValue`, and free helpers (`place_sym`/`basic_value`/`err`).
- `jit.rs` — the `Jit` ORC LLJIT wrapper and its `Drop`.
- `lower.rs` — the `CodeGen` struct, construction, scope/type helpers (`int_type`/`float_type`/
  `bool_type`/`range_type`/`basic_type`/`alloca_at_entry`/…), and the top-level driver
  (`generate`/`declare_globals`/`declare_functions`/`gen_function`/`gen_entry`).
- `stmt.rs` — statement and control-flow lowering (`gen_stmt`/`gen_block`/`resolve_place`/`gen_if`/
  `record_branch`, the range-loop machinery `gen_range`/`range_continue`/`default_step`/
  `guard_zero_step`/`raise_runtime_error`/…).
- `expr.rs` — expression lowering (`gen_expr`/`gen_call`/`gen_bin`/`gen_compare`/`gen_short_circuit`/
  `gen_membership`/`load_var`/`build_range`/`extract_range`, and the numeric coercions
  `as_int`/`as_float`/`promote`/`expect_value`/…).
- `tests.rs` — the existing tests.

## Verification

`cargo build` and `cargo build --no-default-features` (front-end only) both compile; `cargo test`
shows the same 175 passing. `cargo fmt` normalises layout; clippy stays clean.