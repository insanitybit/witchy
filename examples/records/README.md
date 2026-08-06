# records

Records are product types with named fields. A `Point` is constructed
positionally (`Point(2, 3)`) and read by name (`p.x`); `manhattan` sums its
fields and `shift` returns a moved copy. Like every value in witchy they are
plain, capability-free data. The functions are pure (`pub`, no capabilities);
only `main` touches the `Console`.

**Shows:** record types, positional construction, and field access.

## Run

```sh
witchy run                                    # from this directory
witchy examples/records/src/records.witchy    # or by file, from the repo root
```
