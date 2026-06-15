# PINP - Pinp Is Not Python

## A toy programming language with longer-term ambitions of growing beyond a simple toy

This project serves as a showcase of what an experienced human dev could achieve using AI tools.

Real world pro dev workflow:
- TDD red → green → refactor
- Iterative development with specs
- Proper git workflow and scope control
- Human oversight throughout

The project is tightly dependent on LLVM as the back-end.
The usage pattern is intended to be direct source code execution via a front-end that processes
the code and sends it directly to LLVM JIT.
AOT is planned in the future.
