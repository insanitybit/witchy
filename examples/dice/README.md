# dice

Deterministic pseudo-randomness with the std `random` module: a seed replays the
same sequence, so this prints the same rolls every run — and identically on the
interpreter and compiled to WASM. The generator state is threaded explicitly
through an `Rng` value (no hidden global, no capability needed).

**Shows:** the std `random` module, explicit state threading, `while` loops, and
`list.map`.

## Run

```sh
witchy run                              # from this directory
witchy examples/dice/src/dice.witchy    # or by file, from the repo root
```
