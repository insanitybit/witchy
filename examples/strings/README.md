# strings

Builds a greeting by concatenating string literals. Compiled to WASM, literals
live in linear memory and `+` concatenates via a bump allocator. The building is
pure (`pub`, no capabilities); only `main` touches the `Console`, so the program
runs identically interpreted, compiled, and inside the capability sandbox.

**Shows:** `String` concatenation, `pub` functions across modules, and in-rune
`test_*` functions.

## Run

```sh
witchy run                                    # from this directory
witchy examples/strings/src/strings.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/strings
```
