# rpn

A reverse-Polish (postfix) calculator: a number is pushed, an operator pops its
two operands and pushes the result. Errors — stack underflow, division by zero,
an unknown token, a leftover stack — flow through `Result` instead of crashing.
Pure (`Console` only); identical on both backends.

**Shows:** algebraic data types, `match`, stack-machine evaluation, error
propagation via `Result`, `pub` functions across modules, and in-rune `test_*`
functions.

## Run

```sh
witchy run                            # from this directory
witchy examples/rpn/src/rpn.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/rpn/src/rpn_test.witchy
```
