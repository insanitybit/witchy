# fizzbuzz

The classic FizzBuzz for 1..15. A `while` loop with a mutable counter drives the
output, using `%` (modulo) and `if`/`else if` to choose what to print. The
`Console` is threaded in from `main` (the root actor) — `fizzbuzz` can only print
because it was handed the capability.

**Shows:** `while` loops, mutable variables, `%` and `if`/`else if`, and the
`Console` capability.

## Run

```sh
witchy run                                      # from this directory
witchy examples/fizzbuzz/src/fizzbuzz.witchy    # or by file, from the repo root
```
