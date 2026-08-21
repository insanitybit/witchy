# parse_kv

Splits a `key=value` setting by finding the `=` with `string.index_of` and
taking each side with `string.substring` (char-based, and it clamps to bounds so
a bad index never panics). Data-only (only `main` touches the `Console`), so it runs
identically interpreted, compiled, and inside the capability sandbox.

**Shows:** the `string` module (`index_of`, `substring`, `length`,
`ends_with`), string interpolation.

## Run

```sh
witchy run                                      # from this directory
witchy examples/parse_kv/src/parse_kv.witchy    # or by file, from the repo root
```
