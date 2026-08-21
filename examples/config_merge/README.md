# config_merge

Layered configuration the witchy way: a base config patched by a production
overlay, then pretty-printed. The overlay wins on the keys it sets and adds new
ones, while keys it never mentions are preserved. The configuration builders
only construct data as written; `main` holds the Console used for output. The
interpreter and compiled Wasm backend produce the same result.

**Shows:** `json.merge` (a shallow per-key override), `json.encode_pretty` and
`json.contains_key` from the std `json` module, `Json` values, and the `Console`
capability.

## Run

```sh
witchy run                                              # from this directory
witchy examples/config_merge/src/config_merge.witchy    # or by file, from the repo root
```
