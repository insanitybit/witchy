# record_update

Named-field construction and the spread `..base`. `Account(name: "x", ..a)`
makes a new record like `a` with some fields replaced — it never mutates `a`
(records are immutable values), and the new field values can reference `a`.
`deposit` and `rename` each return an updated copy. The updates are data-only (`pub`,
no capabilities); only `main` touches the `Console`.

**Shows:** record types, named-field construction, the spread `..base`, and
immutable value semantics.

## Run

```sh
witchy run                                                # from this directory
witchy examples/record_update/src/record_update.witchy    # by file, from repo root
```
