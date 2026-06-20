# opt_pipeline — `mode opt` across modules

A two-module program where **both** modules declare `mode opt`. That isn't a
style choice: `mode opt` is **transitive** — an `opt` module may only import
other `opt` modules (the bundled standard library is the one exemption). So the
optimization discipline is a guarantee over the *whole reachable graph*, not just
one file.

Run it:

```sh
witchy examples/projects/opt_pipeline/main.witchy
```

Prove the guarantee — delete the `mode opt` line at the top of `stats.witchy` and
run again. It no longer compiles:

```
link error: `mode opt` module `main` imports `stats`, which is not `mode opt` …
```

That's the point: under `mode opt` you can't accidentally pull in a
non-optimized dependency. See [`../../opt_mode.witchy`](../../opt_mode.witchy)
for what `mode opt` enforces *inside* a single file, and
[`rfcs/performance-modes.md`](../../../rfcs/performance-modes.md) for the full
model.
