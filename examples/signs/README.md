# signs

Maps a direction (`-1`, `0`, `1`) to a word with a `match` that pattern-matches
negative-literal patterns directly. The conversion is data-only (`pub`, no
capabilities); only `main` touches the `Console`, so the program runs identically
interpreted, compiled, and inside the capability sandbox.

**Shows:** `match` with negative-number patterns, a wildcard `_` arm, `pub`
functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                # from this directory
witchy examples/signs/src/signs.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/signs
```
