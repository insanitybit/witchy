# list_more

More combinators from the standard `list` library: membership (`contains`),
search (`index_of`), and slicing (`take`/`drop`). `contains` and `index_of`
compare by value, so they work for any element type.

**Shows:** the `list` stdlib module — `contains`, `index_of`, `take`, `drop`,
`at`. Data-only (`import list` grants no capabilities); only `main` touches the
`Console`.

## Run

```sh
witchy run                                      # from this directory
witchy examples/list_more/src/list_more.witchy  # or by file, from the repo root
```
