# Expression statements and discarded values

A block evaluates to the value of its final expression. Earlier expressions are permitted and may exist solely for their side effects.

The language itself should not require syntactic noise (such as explicit discard or dummy assignments) merely to satisfy compiler diagnostics.

Detection of suspicious discarded values belongs in a later static analysis / linter pass rather than the core compiler. The analyzer should distinguish between:

Expressions with observable side effects (e.g. function calls, assignments) — no diagnostic.

Pure expressions whose result is discarded — potentially suspicious and may warrant a warning.

This keeps the core language minimal while still allowing high-quality diagnostics as tooling evolves.
