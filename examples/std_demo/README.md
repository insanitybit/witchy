# std_demo

Composes the bundled `list` module: `range`, `map`, `fold`, `filter`, `reverse`,
and `at`. Because the module was never handed a capability it cannot perform
effects — only `main`, which holds the `Console`, decides what to print.

**Shows:** importing the standard library, data-only `list` combinators, and closures
passed to `map`/`fold`/`filter`.

## Run

```sh
witchy run                                        # from this directory
witchy examples/std_demo/src/std_demo.witchy      # or by file, from the repo root
```
