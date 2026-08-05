# traits

Define your own trait and write code generic over it. `Shape` declares `area`
and `name`; three types implement it; and `describe` / `total_area` work for
*any* Shape via a `where s: Shape` bound. Witchy monomorphizes such a generic
per concrete type, so trait code stays fast and compiles to WASM identically to
the interpreter. Pure (Console only).

**Shows:** trait declaration and `impl`, generics with a `where` bound, and
`match` over enum variants.

## Run

```sh
witchy run                                  # from this directory
witchy examples/traits/src/traits.witchy    # or by file, from the repo root
```
