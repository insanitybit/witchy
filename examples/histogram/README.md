# histogram

Tally how often each word appears, then print the whole table. Accumulating a
frequency map (`dict.insert` + `dict.get_or`) and then walking `dict.keys` to
report it is the everyday "group by a key" shape; keys iterate in insertion
order, identically on every backend.

**Shows:** `Dict` accumulation, `string.split`, `for` loops, and a
capability-gated `Console`.

## Run

```sh
witchy run                                        # from this directory
witchy examples/histogram/src/histogram.witchy    # or by file, from the repo root
```
