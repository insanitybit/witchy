# bank

Records, lists, and `Result`/`?` together. `total` folds account balances over a
list; `debit` returns a `Result`, and `?` in `pay_both` short-circuits the whole
computation on the first overdraft. The logic is pure; only `main` prints.

**Shows:** record types, an enum `Result` with `match`, the `?` early-return
operator, `for` folds, and the `Console` capability.

## Run

```sh
witchy run                            # from this directory
witchy examples/bank/src/bank.witchy  # or by file, from the repo root
```
