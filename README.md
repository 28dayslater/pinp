# PINP - Pinp Is Not Python

## A programming language project that starts small but aims beyond a toy implementation.

### Project values

pinp is open-source software released under the MIT License. The license imposes no restrictions
based on nationality, government, organization, or field of use, and this statement is not part
of it and does not modify it.

The project's maintainers nevertheless intend pinp to serve peaceful, civilian, democratic,
educational, and scientific purposes. We oppose the use of pinp by the governments, armed forces,
intelligence services, or security apparatuses of Russia, Iran, North Korea, China, or other
authoritarian states for warfare, repression, surveillance, censorship, or other violations of
human rights. This is not directed at the people of these countries, who are often the first
victims of such regimes.

We recognize that such regimes are unlikely to respect a software license or a non-binding
statement such as this, and that preventing such use may be practically impossible.

If you use our work to harm or oppress others, the license may not stop you. God will judge you.

### My ambition is to implement a language that:

- Reads like pseudocode.
- Minimizes syntactic noise while preserving explicit semantics.
- Feels like Python without trying to be Python.
- Statically typed with local type inference.
- Fast enough for everyday use (LLVM JIT from day one).

Development follows a professional AI-augmented software engineering workflow:

- Specification-driven development.
- TDD (red → green → refactor) and review-driven development using both human and alternative AI models.
- Continuous human supervision throughout.
- Small iterations, further divided when necessary (reviews at each step).
- Proper git workflow and scope control.
- Test coverage among the industry's best.


Pinp targets LLVM exclusively by design. Life is too short to re-invent backend compiler technology.

Ahead of Time compilation (AOT) is planned in the future.

### An early taste of how it feels

Indentation blocks, inferred local types, no braces/semicolons. 
Types appear on parameters/returns and if required to disambiguate.

```
sum_to(n: int): float is
    i, s = 0, .0 # Python style declarations, but types are inferred (i: int, s: float)
    # No () around condition like in C and no trailing : like in Python
    while i < n
        s, i = s + i/2, i+1
    s  # Last expression in scope is the return value. "return" will be available.

sum_to(5)  # 5.0
```

`if`/`else` is an expression, functions can be one-liners:

```
max(a: int, b: int): int is a if a > b else b

max(3, 9)
```

Ranges and `for … in`:

```
total = 0
for idx in 1..13:2  # for idx in 1,3,5,7,9,11,13
    total += idx
total  # total: 49
```

Language manual is not there yet. For the time being, you can read the specs and tests.
That may give you an idea of what the intended language syntax is.
Note: It's a very WIP project. There is a long way ahead.