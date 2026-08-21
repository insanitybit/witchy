# higher_order

First-class functions: a function can be passed as an argument (`apply`),
returned from another function while capturing variables — a closure
(`make_adder`), and folded over a list (`reduce`). The helpers are data-only (`pub`,
no capabilities); only `main` touches the `Console`.

**Shows:** function-typed parameters, closures, `for` folds, and in-rune
`test_*` functions.

## Run

```sh
witchy run                                              # from this directory
witchy examples/higher_order/src/higher_order.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/higher_order/src/higher_order_test.witchy
```
