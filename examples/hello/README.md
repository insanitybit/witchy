# hello

A first taste of witchy: pure functions, pattern matching, string
interpolation, and capability-gated effects. `print` requires a `Console`
capability, which only `main` is granted — the pure helpers `double` and
`classify` need none, so they run identically interpreted, compiled, and inside
the capability sandbox.

**Shows:** `match`, capability-gated `Console`, string interpolation, `pub`
functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                # from this directory
witchy examples/hello/src/hello.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/hello/src/hello_test.witchy
```
