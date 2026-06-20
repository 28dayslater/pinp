[Human]

# Comments and blank lines

## Blank lines

```
total = 0

for value in 1..10

    total += value

total
```

## Comments

A comment begins with `#` and runs to the end of that line. 
There is no separate syntax for multi-line comments.

```
# Sum the first ten numbers
#  in pinp looks like:
total = 0
for value in 1..10
    total += value      # running total

total                   # the program's result
```

[AI]

# Implementation

Both features are handled entirely in the lexer; the parser and later stages are untouched. The work
splits into recognising comments and making content-free lines invisible to the indentation logic.

## Comments

A comment is lexed as a skipped pattern — `#` followed by any run of non-newline characters. Because
the pattern stops at the newline, the line's terminating newline is left intact, so the indentation
machinery still sees every line boundary. A comment therefore produces no token at all: a trailing
comment simply vanishes after the code it follows, and a comment occupying a whole line leaves only
that line's leading and trailing newlines behind.

There is no multi-line comment because the pattern cannot cross a newline; spanning several lines
requires a `#` on each.

## Blank and comment-only lines

The lexer turns line boundaries into the synthetic `Newline`, `Indent`, and `Dedent` tokens that mark
where statements and blocks begin and end. The hazard is a line with no real content — empty,
all spaces, or only a comment: emitting its indentation would open or close a block that the author
never intended (a blank line sitting at the left margin inside an indented block would otherwise look
like the block had ended).

To avoid this, a newline is not acted on the instant it is seen; it is *recorded as pending* and only
turned into tokens once a line with actual content arrives. A run of newlines with nothing but skipped
whitespace or a skipped comment between them keeps overwriting the same pending record, so the
intermediate lines collapse and disappear. What survives is a single `Newline` carrying the
indentation of the next content-bearing line — exactly the boundary the parser needs. A content-free
line thus contributes nothing, whatever its indentation, which is what lets a comment-only line be
ignored no matter how it is indented.

A newline still pending when the input ends terminates the final statement, as a trailing newline did
before. Content-free lines at the very start of the file leave a leading separator that the parser's
existing separator-skipping absorbs. Inside parentheses a newline is line-joining and continues to be
dropped outright, comments included.
