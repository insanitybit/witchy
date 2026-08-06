# equality

The standard `Eq` trait gives content-correct equality that holds up in compiled
code, not just the interpreter. `member` and `index_of` search a list by the
element type's `Eq` impl, so they find runtime-built strings and your own types
where a generic `==` search would fall back to comparing pointers. The example
implements `Eq` for a `Color` type and searches a palette with the std `eq` module.

**Shows:** traits and `impl`, `match` on a custom type, generics over a trait
bound, and the std `eq` module (`member`, `index_of`, `ne`).

## Run

```sh
witchy run                                        # from this directory
witchy examples/equality/src/equality.witchy      # or by file, from the repo root
```
