# dice

Deterministic pseudo-randomness with the std `prng` module: a seed replays the
same sequence, so this prints the same rolls on each ordinary admitted Wasm run.
`witchy parity` can compare that result with canonical SafeWIR. The generator
state is threaded explicitly through an `Rng` value (no hidden global, no
capability needed).

**Shows:** the std `prng` module, explicit state threading, `while` loops, and
`list.map`.

## Run

```sh
witchy run                              # from this directory
witchy examples/dice/src/dice.witchy    # or by file, from the repo root
```
