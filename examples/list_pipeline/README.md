# list_pipeline

The standard `list` library compiled to WebAssembly, not just the interpreter:
`map` / `filter` / `fold` and a comparator-driven `sort_by` all run in the
compiled backend, with closures (including a *capturing* one) crossing the
compiled boundary.

**Shows:** the `list` stdlib module with closures — `map`, `filter`, `fold`,
`sort_by`, `at`. Data-only (`import list` grants no capabilities); only `main` touches
the `Console`.

## Run

```sh
witchy run                                              # from this directory
witchy examples/list_pipeline/src/list_pipeline.witchy  # or by file, from the repo root
```
