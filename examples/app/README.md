# app

A two-module rune: `main` imports the `strutil` library and calls it with a
qualified name. The import brings `shout` into scope but confers no authority —
`main` (the root actor) holds the `Console` and decides what to do with the
library's pure result.

**Shows:** multi-module runes, qualified calls, the capability-free nature of
imports, `pub` library functions, and in-rune `test_*` functions.

## Run

```sh
witchy run                          # from this directory
witchy examples/app/src/app.witchy  # or by file, from the repo root
```

## Test

```sh
witchy test examples/app
```
