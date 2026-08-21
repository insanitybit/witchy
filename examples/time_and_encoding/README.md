# time_and_encoding

A tiny showcase of the std `time` and `encoding` modules: turn a unix timestamp
into a civil UTC date, parse ISO-8601 (rejecting impossible dates), and
base64/hex a short payload with a decode round-trip. Its root footprint is just
`Console`, so it runs identically on both backends.

**Shows:** the `time` and `encoding` modules, `match` on a `Result`, and string
concatenation.

## Run

```sh
witchy run                                                        # from this directory
witchy examples/time_and_encoding/src/time_and_encoding.witchy    # or by file, from the repo root
```
