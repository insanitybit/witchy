# BUG-163: `compile` and `emit-wat` bypass `mode opt`

Severity: MED
Status: FIXED
Fixed: 2026-07-09 — `compile` now enforces performance modes before artifact generation, and grant-document sandbox launches enforce the same contract before execution. CLI regressions cover rejection and absence of output.
Verified fixed: 2026-07-09 — focused `mode_opt_enforced_across_subcommands` test and `./scripts/check.sh --fast` (1,598/1,598) green.
Verified: 2026-07-06 REPRO on master 7bb3ee7 — half-fixed by c102e90: `emit-wat` now rejects the repro (exit 1, same diagnostic as `check`), but `witchy compile --out` still exits 0 and writes the `.wasm` (`src/main.rs` compile handler links/typechecks without `enforce_performance_modes`)
Component: CLI consistency, `mode opt`, artifact generation, package build
Discovered: 2026-07-05

## Summary

`mode opt` turns performance cliffs into hard errors on the normal validation
and artifact paths, but two compiler surfaces skip that enforcement:

- `witchy compile`
- `witchy emit-wat`

The same invalid `mode opt` source is rejected by `witchy check` and
`witchy emit-wasm`, but `compile --out` still writes a `.wasm` artifact and
`emit-wat` still emits WAT.

This is separate from BUG-119, which tracks the `parity` harness bypassing
`mode opt`.

## Reproduction

`scratch/repro-mode-opt-compile-emit-wat.witchy`:

```witchy
mode opt

fn main(console: Console):
    var xs = []
    var snaps = []
    for i in [1, 2, 3]:
        snaps = list.push(snaps, xs)
        xs = list.push(xs, i)
    print(console, __render(list.length(xs)))
```

Validation/artifact surfaces:

```console
$ ./target/debug/witchy check scratch/repro-mode-opt-compile-emit-wat.witchy
error: in `main` (line 7): `xs` is rebuilt by copy on every iteration of this loop — it is stored back into the list [mode opt]

$ ./target/debug/witchy emit-wasm scratch/repro-mode-opt-compile-emit-wat.witchy /tmp/witchy-mode-opt-repro.wasm
error: in `main` (line 7): `xs` is rebuilt by copy on every iteration of this loop — it is stored back into the list [mode opt]

$ ./target/debug/witchy emit-wat scratch/repro-mode-opt-compile-emit-wat.witchy >/tmp/witchy-mode-opt-repro.wat
# exits 0 and writes WAT

$ ./target/debug/witchy compile scratch/repro-mode-opt-compile-emit-wat.witchy --out /tmp/witchy-mode-opt-compile.wasm
scratch/repro-mode-opt-compile-emit-wat.witchy: compiled -> /tmp/witchy-mode-opt-compile.wasm
# exits 0 and writes WASM
```

## Code Evidence

- `src/main.rs:1299-1302` runs `typeck::check` and then
  `enforce_performance_modes` for `witchy check`.
- `src/main.rs:2463-2466` does the same for `emit-wasm`.
- `src/main.rs:490-493` handles `witchy compile` by linking, type-checking, and
  compiling to WASM without calling `enforce_performance_modes`.
- `src/main.rs:1907-1910` handles `emit-wat` by linking, type-checking, and
  assembling WIR without calling `enforce_performance_modes`.
- `projects/pm/src/pm.witchy:154` and `projects/pm/src/pm.witchy:194` drive
  project build/run through `witchy compile`, so the bypass reaches package
  workflows.

## Expected Behavior

Every command that validates or emits an executable artifact from source should
honor `mode opt` consistently. A source file rejected by `witchy check` for a
`mode opt` violation should also be rejected by `compile`, `emit-wat`,
`emit-wasm`, source run/sandbox, package build, and package run.

`emit-wat` can remain a diagnostic surface, but it should not silently produce
an optimized artifact for a source file whose declared optimization contract is
violated.

## Impact

`mode opt` is presented as a source-level contract. Letting lower-level artifact
commands bypass it makes release builds and PM-driven builds weaker than the
developer's validation command, which is exactly the kind of inconsistency that
will make the language feel unfinished in a public release.

## Suggested Fix

Call `enforce_performance_modes(&linked, &stem)` in the `compile` and
`emit_wat_file` paths after type checking and before artifact generation. Add
CLI/e2e coverage for the repro above, plus coverage that PM build/run inherits
the same rejection through `witchy compile`.

## Acceptance Criteria

- `witchy compile scratch/repro-mode-opt-compile-emit-wat.witchy --out ...`
  exits non-zero with the same `mode opt` diagnostic as `check`.
- `witchy emit-wat scratch/repro-mode-opt-compile-emit-wat.witchy` exits
  non-zero with the same diagnostic.
- `witchy emit-wasm` and `witchy check` keep rejecting the same repro.
- Tests cover `compile` and `emit-wat`; PM coverage is added or explicitly
  tracked if package fixtures make it too costly for the narrow fix.
