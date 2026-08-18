# Performance: measuring and diagnosing

Performance work needs two kinds of evidence: a semantic check that the
optimized and reference paths agree, and a matched measurement that shows the
cost changed for the workload under study. A counter is evidence of a compiler
event, not proof of a user-visible speedup by itself.

## Measure the syntax choice first

Begin with two source programs that differ in one truthful contract. The
environment stays the same; the source is the experiment:

| Source comparison | Expected question | Useful evidence |
|---|---|---|
| default input vs `let` input | Did the read-only helper avoid an input owner? | `heap_bytes`, retains, output parity |
| default call vs `own` plus `move` | Did the existing collection buffer cross the call? | `reowns`, `heap_bytes`, output parity |
| ordinary mutable local vs `unique` update | Did mutation or extraction stay in place? | `reowns`, `extract_copied_bytes`, output parity |
| ordinary record vs `packed` record | Did the list receive an inline fixed layout? | `packed_alloc_calls`, `packed_alloc_bytes` |
| unscoped temporaries vs `region:` | Did the temporary burst rewind at scope exit? | `region_rewind_calls`, `region_copy_bytes` |
| normal value flow vs `mode opt` reference flow | Did the proven loan avoid materialization without escaping? | accepted program, carrier parity, materialization counters |

Do not start by searching for a flag. If the source contract is missing, an
environment setting cannot make an unsafe transformation valid. If the source
contract is present but the shape still does not optimize, then use the
diagnostic switches below to identify the backend pass or fallback.

## Compare one change at a time

Use the same source, input, backend, memory budget, and environment when
isolating a backend pass. Start with the release registry, then remove one
pass:

```sh
WITCHY_OPT=release witchy stats bench.witchy > /tmp/release.stats
WITCHY_OPT=release,-inplace witchy stats bench.witchy > /tmp/no-inplace.stats
diff -u /tmp/release.stats /tmp/no-inplace.stats
```

For a second pass, repeat from release rather than stacking unrelated removals:

```sh
WITCHY_OPT=release,-unbox witchy stats bench.witchy > /tmp/boxed.stats
WITCHY_OPT=release,-region witchy stats bench.witchy > /tmp/no-region.stats
```

`witchy stats` reports deterministic operation counts and the program output.
For wall-clock throughput or latency, time the same two invocations separately,
report cold and warm runs, and repeat enough times to show variance. A warm
compiler cache and a cold compiler build answer different questions.

## Counters by question

| Question | Useful evidence |
|---|---|
| Did ownership reuse fire? | `reowns`, `heap_bytes`, `extract_copied_bytes` |
| Did a region reclaim temporaries? | `region_rewind_calls`, `region_copy_bytes`, `heap_bytes` |
| Did packed storage get selected? | `packed_alloc_calls`, `packed_alloc_bytes` |
| Did overwrite reuse happen? | `rc_free_calls`, `rc_reuse_calls`, `rc_reused_bytes`, `live_cells` |
| Did extraction preserve the spine? | `extract_searches`, `extract_copied_bytes`, `indirect_ownership_calls` |
| Did direct write-back fire? | `direct_storage_var_accesses` |
| Did code shape change without allocation movement? | generated Wasm, codegen diagnostics, pass-specific fixture |

Counters can move in opposite directions. For example, a packed layout may
reduce heap allocations while increasing compile-time layout work; a loop
unroll may improve a hot loop while increasing code size. Record the complete
counter line and the workload, not only the number that improved.

## A repeatable investigation

1. **Find the boundary.** Run `witchy check` and identify the copy, loan,
   allocation, or representation boundary named by the diagnostic.
2. **Make a small fixture.** Keep the input deterministic and isolate one
   operation: one accumulation, one overwrite loop, one packed scan, or one
   borrowed result.
3. **Run the semantic oracle.** Compare interpreter, optimized Wasm, and the
   forced-copy configuration. Values, writes, drops, traps, and accepted or
   rejected programs must agree.
4. **Toggle one pass.** Use `WITCHY_OPT=release,-name` and compare counters or
   emitted code. If nothing changes, the pass may not match the shape.
5. **Measure a paired workload.** Time both versions with the same warm-up and
   input. Keep a result only if it survives repeated runs and does not regress
   a neighboring workload.
6. **Record the fallback.** Add the alias, escape, projection, or boundary that
   correctly prevents the pass from firing. A good optimization test includes
   both the positive and fail-closed shape.

## Shape-only passes need code evidence

`direct-call`, `bounds-elide`, `closure-elide`, and some `sroa` cases may not
change heap counters. For these, inspect the generated Wasm or use the focused
codegen fixture. Still run output and trap parity. A shorter Wasm function is
not sufficient if a bounds check or failure path changed meaning.

## Cold and warm builds

`wasm-opt` and the browser compiler are build-time work. Measure them separately
from guest execution:

```sh
time WITCHY_OPT=release witchy build --wasm bench.witchy
time WITCHY_OPT=release witchy run bench.witchy
```

The first build may pay dependency compilation, Binaryen, and cache setup. A
second build answers the warm-cache question. Do not compare a cold optimized
build with a warm debug build and call the difference a pass win.

## What a useful report contains

Keep a compact ledger entry with:

```text
fixture:        packed_point_scan
source:         commit or working-tree identifier
baseline:       WITCHY_OPT=release,-unbox
candidate:      WITCHY_OPT=release
backend:        interpreter + optimized Wasm + forced-copy Wasm
input:          deterministic point count and seed
evidence:       packed_alloc_bytes, output, trap/parity result, timings
result:         improved for this measured workload / no change / regression
fallback:       generic boundary remains boxed
```

Say “improved for this measured workload,” not “always faster.” The compiler's
proof is general, but the value of a proof depends on the workload's shape,
input size, cache state, and surrounding operations.
