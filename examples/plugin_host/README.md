# plugin_host

The capability thesis, made concrete. A compute-only plugin has the checked
type `pure fn(Int) -> Int`. The contract rejects capability operations,
authority-bearing captures, and calls through opaque ordinary callbacks. The
host owns the root authority and decides which narrower operations it delegates;
authority is never ambient.

**Shows:** function values as plugins, closures capturing a capability
(`console`), higher-order functions, `for` loops.

## Run

```sh
witchy run                                          # from this directory
witchy examples/plugin_host/src/plugin_host.witchy  # or by file, from the repo root
```
