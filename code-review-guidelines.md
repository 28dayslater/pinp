# Review checklist

- DRY violations.
- Naming of variables and functions: too short, not descriptive and not corresponding to the usage of the symbol.
- Missing tests for iteration features, including tests against a code sample that should result in an error being reported. 
- No panic on parser input. Tests are exception.
- Check against SVE databases.
- Usage of single line code snippets containing \n instead of indoc! { "multi-line-code-snippet" }
