# records

Records are product types with named fields. A `Point` is constructed
positionally (`Point(2, 3)`) and read by name (`p.x`); `manhattan` sums its
fields and `shift` returns a moved copy. These records are ordinary data, and
the helper bodies only compute over their inputs as written; `main` holds the
`Console`. A checked effect-free API would declare the helpers `pure fn`.

**Shows:** record types, positional construction, and field access.

## Run

```sh
witchy run                                    # from this directory
witchy examples/records/src/records.witchy    # or by file, from the repo root
```
