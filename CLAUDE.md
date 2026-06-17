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

## Messages & comments

- **Error messages** are capitalized and end with a period; keep them short.
- **Doc-comment public items** (`///`); on private internals, comment the *why*, not the *what*.
