# temperature

A Fahrenheit-to-Celsius conversion table — the first real program in K&R's *The C
Programming Language* and a staple of the Go tour, in witchy. It needs real float
output: `math.format_float` formats a `Float` on both the interpreter and the
compiled WASM backend (where `to_string` can't). The conversion is data-only (`pub`, no
capabilities); only `main` touches the `Console`.

**Shows:** `Float` arithmetic, a `while` loop, `math` (`to_float`,
`format_float`), `pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                            # from this directory
witchy examples/temperature/src/temperature.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/temperature
```
