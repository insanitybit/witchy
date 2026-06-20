# record_compiled

Records compiled to WebAssembly. A `Point` is a heap record and `p.x` loads a
field by its offset; `dist_squared` reads fields and `shift_x` builds a new
record with the spread `Point(x: …, ..p)`. The whole program compiles to a WASM
module and runs on the witchy runtime — records, not just sum-type matches, work
in the compiled backend. The computations are pure (`pub`, no capabilities); only
`main` touches the `Console`.

**Shows:** record types, field access, the spread `..base`, and compiled-backend
records.

## Run

```sh
witchy run                                                  # from this directory
witchy examples/record_compiled/src/record_compiled.witchy  # by file, from repo root
```
