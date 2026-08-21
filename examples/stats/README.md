# stats

Summary statistics over a list of floats: count, mean, population variance,
standard deviation, min, and max. Floats flow through arithmetic and `math.sqrt`,
and values are rendered with `math.format_float` (which works on both backends,
unlike `to_string` on a `Float`). The computations are data-only (`pub`, no
capabilities); only `main` touches the `Console`.

**Shows:** `Float` arithmetic, `for` loops, `math` (`sqrt`, `format_float`,
`float_min`/`float_max`), `pub` functions across modules, and in-rune `test_*`
functions.

## Run

```sh
witchy run                                # from this directory
witchy examples/stats/src/stats.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/stats
```
