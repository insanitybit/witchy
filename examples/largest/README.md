# largest

The generic `largest` function from *The Rust Programming Language*: find the
biggest element of a list, for any element type that is ordered. Rust writes the
bound `T: PartialOrd`; witchy writes `where a: Ord` over the std `Ord` trait, and
a user `Version` type becomes comparable by implementing a single `compare`
method. The comparisons are data-only (`pub`, no capabilities); only `main` touches
the `Console`.

**Shows:** generics with a trait bound (`where a: Ord`), implementing a trait for
a user type, `match`, and in-rune `test_*` functions.

## Run

```sh
witchy run                                  # from this directory
witchy examples/largest/src/largest.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/largest
```
