# wordcount

A word-frequency count using the `Dict` associative map: `dict.new` makes an
empty map, `dict.get_or` reads with a default, and `dict.insert` returns a new
map with a key updated (dicts are immutable values, like lists). Keys are
compared by value, so the same word lands in the same bucket. Data-only (only `main`
touches the `Console`).

**Shows:** the `dict` module, `for` loops, and `var` rebinding.

## Run

```sh
witchy run                                        # from this directory
witchy examples/wordcount/src/wordcount.witchy    # or by file, from the repo root
```
