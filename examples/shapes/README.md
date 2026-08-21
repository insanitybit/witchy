# shapes

Algebraic data types compiled to WebAssembly: each constructor (`Circle`,
`Square`) becomes a heap record `[tag][fields...]`, and `match` loads the tag
and binds the fields. `area` is data-only; only `main` touches the `Console`, so it
runs identically interpreted, compiled, and inside the capability sandbox.

**Shows:** algebraic data types, constructors, and `match` with field binding.

## Run

```sh
witchy run                                  # from this directory
witchy examples/shapes/src/shapes.witchy    # or by file, from the repo root
```
