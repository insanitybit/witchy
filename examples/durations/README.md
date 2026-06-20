# durations

Durations the witchy way: write times as `30s`, `500ms`, `2hr` — a distinct type,
so the compiler stops you adding a Duration to a bare number, yet they combine and
compare with the ordinary operators (`d * 2`, `a < b`, `a + b`). The example builds
a capped exponential `backoff` schedule and formats it with the std `duration`
module. `backoff` is pure (`pub`, no capabilities); only `main` touches the
`Console`, so it runs identically interpreted, compiled, and inside the capability
sandbox.

**Shows:** duration literals, operator overloading on a custom type, the std
`duration` module, `pub` functions across modules, and in-rune `test_*` functions.

## Run

```sh
witchy run                                        # from this directory
witchy examples/durations/src/durations.witchy    # or by file, from the repo root
```

## Test

```sh
witchy test examples/durations
```
