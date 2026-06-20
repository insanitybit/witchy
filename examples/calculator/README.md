# calculator

A complete arithmetic calculator that parses and evaluates expressions like
`100 - 2 * (3 + 4)` straight from a string, honoring precedence and parentheses.
A hand-written recursive-descent parser/evaluator using mutual recursion, tuples
to thread the cursor, and character scanning — all pure (only `main` touches the
Console), so it runs identically interpreted, compiled, and sandboxed.

**Shows:** mutual recursion, tuple returns and destructuring, `while` loops,
string scanning, `pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                            # from this directory
witchy examples/calculator/src/calculator.witchy      # or by file, from the repo root
```

## Test

```sh
witchy test examples/calculator
```
