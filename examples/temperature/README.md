# temperature

A Fahrenheit-to-Celsius conversion table — the first real program in K&R's *The C
Programming Language* and a staple of the Go tour, in witchy. `math.format_float`
renders each `Float` with one decimal place in the ordinary admitted Wasm run.
The conversion is data-only (`pub`, no capabilities); only `main` touches the
`Console`. `witchy parity` can compare the Wasm result with canonical SafeWIR.

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
