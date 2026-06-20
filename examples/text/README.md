# text

Splitting a row, mapping a transform over the fields, and re-joining: the
`split`/`replace`/`to_upper` string builtins combined with the `list` and
`string` standard libraries. Only `main` touches the `Console`, so the program
runs identically interpreted, compiled, and inside the capability sandbox.

**Shows:** the `list` and `string` modules, `list.map` with a closure, and
string builtins (`split`, `join`, `repeat`, `replace`, `to_upper`).

## Run

```sh
witchy run                              # from this directory
witchy examples/text/src/text.witchy    # or by file, from the repo root
```
