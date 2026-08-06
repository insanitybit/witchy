# lazy_fib

Lazy infinite sequences with the std `iter` library — witchy's answer to Rust's
`Iterator` adapter chains. `fibs()` is an *infinite* iterator built with
`unfold`; the consumers bound it (`take`, `take_while`, `filter`, `find`), so
only the demanded prefix is ever computed.

**Shows:** the `iter` stdlib — `unfold`, `take`, `take_while`, `filter`, `sum`,
`find`, `collect` — plus closures, tuples, and `Option`. Only `main` touches the
`Console`.

## Run

```sh
witchy run                                      # from this directory
witchy examples/lazy_fib/src/lazy_fib.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/lazy_fib
```
