# pinp — project conventions

This file is the source of truth for pinp's coding conventions, for human contributors and AI
agents alike.

## Naming conventions

The code is a teaching showcase — favor clear, explicit names over terse ones.

- **No single-letter names** for bindings, parameters, or lifetimes — e.g. `'ast`, not `'a`.
- **No abbreviations** for types/values: write `inferred`, `joined`, `left_type`/`right_type`,
  `signature` — not `ty`, `acc`, `lt`/`rt`, `sig`.
- **Id bindings end in `_id`** and read through `.value()`: `expr_id.value()`, `sym_id` — not a bare
  `expr`/`sym`, which reads as the expression/symbol itself rather than its id.
- **Role names are fine** for ids used in a role: `cond`, `lhs`, `rhs`, `callee`, `arg`, `operand`,
  `result`.
- **Don't truncate around reserved words.** When the natural name is a Rust keyword (`else`,
  `type`, `match`, …), spell out a descriptive alternative — `else_block`, `pinp_type` — not a
  truncation like `els`/`ty`. A bare truncation is a last resort only.
- **Grammar-production methods are prefixed `parse_*`** (e.g. `parse_block`); plain helpers stay bare.
- **Name a pass by what it produces — never a bare "lower".** *Lowering* is a family (AST → CFG is
  as much a lowering as AST → LLVM IR), so the unqualified word stopped being an identifier the day
  a second target existed. Each pass owns a distinct verb and method prefix:

  | pass | verb | prefix |
  |---|---|---|
  | AST → LLVM IR | *generate* / "lower **to LLVM IR**" | `gen_*` (`src/codegen/`) |
  | AST → control-flow graph | *build* / "CFG **construction**" | `build_*` (`src/analysis/cfg.rs`) |
  | CFG × analysis → facts | *solve* | `solve` (`src/analysis/dataflow.rs`) |
  | facts → diagnostics | *check* | `check_*` (`src/analysis/checks/`) |

  In prose, "lowering" without a named target is a review comment.

## Known issues ledger

[dev-docs/known-issues-todos.md](dev-docs/known-issues-todos.md) is the **live** list of deferred
problems — one `KI-NNNN` row each. `specs/` is the frozen decision record; the ledger is what is
wrong *now*. Add a row when consciously deferring something real, mark the code site with
`// TODO(KI-NNNN): …`, and **delete** the row when it is fixed. Its own header states the full
convention, including where an item belongs when it is a missing feature rather than a defect.

## Messages & comments

- **Error messages** are capitalized and end with a period; keep them short.
- **Doc-comment public items** (`///`); on private internals, comment the *why*, not the *what*.

## Unit test policy

Implementation must start with generating unit and e2e test coverage and presented to the user,
before any code is written.
It is of utmost importance to generate as many "adversarial" tests as reasonably possible.
Those should cover as much of the code as possible, including internal code.
Tests covering corner cases of internal functionality in a file, should be placed in that file.
`#[cfg(test)] mod tests` blocks inside a module file cover that module's internal contract;
`tests/*.rs` files cover higher-level end-to-end behavior via `PinpJit`.

Every new feature requires an **extensive** test suite of the sema layer.
