# predicates

`list.all` and `list.any` test a predicate (a closure) against a list and
short-circuit via early return. They are generic over the element type. Data-only, so
it runs identically interpreted and compiled to WASM.

**Shows:** the `list` module (`all`, `any`), closures, generics, string
interpolation.

## Run

```sh
witchy run                                        # from this directory
witchy examples/predicates/src/predicates.witchy  # or by file, from the repo root
```
