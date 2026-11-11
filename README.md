# PINP - Pinp Is Not Python

## A programming language project that starts small but aims beyond a toy implementation.

**Platform:** only Linux is supported at this time.

### Project values

pinp is open-source software released under the MIT License. The license imposes no restrictions
based on nationality, government, organization, or field of use, and this statement is not part
of it and does not modify it.

The project's maintainers nevertheless intend pinp to serve democratic, educational, scientific,
and other civilian purposes, and the defence of free societies. We oppose the use of pinp by the
governments, armed forces, intelligence services, or security apparatuses of Russia, Iran,
North Korea, China, or other authoritarian states for aggression, repression, surveillance,
censorship, or other violations of human rights. This is not directed at the people of these
countries, who are often the first victims of such regimes.

We explicitly support the use of pinp by nations, their armed forces and allies, civil-defence
organizations, and individuals to defend against aggression by the mentioned states, including
military, cyber, and intelligence defence.

We recognize that authoritarian regimes are unlikely to respect a software license or a
non-binding statement such as this, and that preventing such use may be practically impossible.

If you use our work to harm or oppress others, the license may not stop you. God will judge you.

### My ambition is to implement a language that:

- Reads like pseudocode.
- Minimizes syntactic noise while preserving explicit semantics.
- Feels like Python without trying to be Python.
- Is statically typed with local type inference.
- Is fast enough for everyday use: programs are compiled to native code the moment they run.

Development follows a professional AI-augmented software engineering workflow:

- Specification-driven development.
- TDD (red → green → refactor) and review-driven development using both human and alternative AI
  models.
- Continuous human supervision throughout.
- Small iterations, further divided when necessary (reviews at each step).
- Proper git workflow and scope control.
- Test coverage among the industry's best.

### How pinp runs your code

Pinp is a just-in-time compiler: every time you run a program, it is compiled to native machine
code and executed straight away. Life is too short to re-invent backend compiler technology, so
pinp builds on an existing code generator rather than writing its own.

That code generator is [Cranelift](https://cranelift.dev). LLVM is an amazing piece of engineering,
and pinp started out on it — but it is too bulky for a compiler that always compiles at run time,
which is what pinp wants to be. LLVM is large, slow to start, and built to squeeze the last bit of
speed out of programs compiled ahead of time. Cranelift is designed for exactly pinp's case: it
compiles quickly, keeps pinp a small, self-contained program, and, like pinp, is written in Rust.

Ahead-of-time (AOT) compilation is planned for the future.

### An early taste of how it feels

Indentation blocks, inferred local types, no braces or semicolons.
Types appear on parameters and return values, and wherever needed to disambiguate.

```
sum_to(n: int): float is
    i, s = 0, .0 # Python style declarations, but types are inferred (i: int, s: float)
    # No () around condition like in C and no trailing : like in Python
    while i < n
        s, i = s + i/2, i+1
    s  # The last expression is the function's value

sum_to(5)  # 5.0
```

`if`/`else` is an expression, functions can be one-liners:

```
max(a: int, b: int): int is a if a > b else b

max(3, 9)
```

`return` leaves a function early:

```
sign(n: int): int is
    if n < 0
        return -1
    if n == 0
        return 0
    1
```

Ranges and `for … in`:

```
total = 0
for idx in 1..13:2  # for idx in 1,3,5,7,9,11,13
    total += idx
total  # total: 49
```

There is no language manual yet. For the time being, the specs and tests give an idea of the
intended syntax.

Note: this is very much a work in progress. There is a long way ahead.
