# RFC-0146 acceptance ledger

This ledger is the durable index for RFC-0146 promotion evidence. Raw timing,
counter, profile, and generated-WAT artifacts remain outside source control.
Every artifact must carry the schema 1 build identity emitted by `bench.sh`.

## Evidence contract

- Fast exploration uses `benchmarks/.build/results-fast.json` by default.
- Full promotion uses `benchmarks/.build/results-full.json`, twelve paired and
  interleaved Witchy/Go kernel samples, and two warmups.
- Timing artifacts include source and binary identity, toolchains, host,
  optimization configuration, source digests, output checksums, raw samples,
  median, range, MAD, and a deterministic paired bootstrap 95 percent interval.
- Counter evidence is accepted only when three repetitions agree exactly.
- Heap evidence subtracts the one fixed 8 KiB format reservation before
  comparing workload growth. Bounded fixtures use two iteration counts and a
  stated constant tolerance.
- A queue row is complete only after its terminal `merged` journal event and
  landed commit are recorded below.

## Track ledger

| Track | State | Baseline artifact | Profile and counters | Focused checks | Protected workloads | Branch | Terminal queue evidence |
|---|---|---|---|---|---|---|---|
| 0 measurement integrity | locally verified | `/tmp/rfc0146-track0-quick-all.json` (`sha256:5a8e32f1bc12665170542503be0c23405b3b08137d23e67910172cb35bf4f85a`) | `stats::tests::deterministic_counters_repeat_exactly`; profile not applicable to harness correctness | 54-test `stats::tests` shard; five focused Python integrity tests; 17-result quick sweep | all 17 benchmark results | `fix/bench-stale-binary` | pending submission |
| 1 awaited select fusion | not started | pending | pending | pending | `select_fanin`, `chan_throughput` | pending | pending |
| 2 sequence plans | not started | pending | pending | pending | `fannkuch`, `list_index`, `list_sum`, `binary_trees` | pending | pending |
| 3 WIR inlining | not started | pending | pending | pending | `closure_calls`, `expr_eval`, `binary_trees` | pending | pending |
| 4 strength reduction | independently owned | pending | pending | pending | `collatz`, `loop_sum`, `mandelbrot`, `expr_eval` | `impl/rfc0146-track4-range` | pending |
| 5 recursive inlining | not started | pending | pending | pending | `fib`, `binary_trees`, `expr_eval` | pending | pending |
| 6 packed Bool kernels | blocked on Track 2 | pending | pending | pending | `nsieve`, `list_sum`, `list_index` | pending | pending |
| 7 borrowed string/hash residual | entry criterion not evaluated | pending | pending | pending | `knucleotide`, `dict_count`, `word_count` | pending | pending |

## Track 0 acceptance detail

| Requirement | Evidence | State |
|---|---|---|
| Default missing or stale compiler rebuilds before timing | tracked and untracked input census covers `.cargo`, root manifests/build script, `src`, `crates`, `std`, embedded `projects`, `menus`, and `web` inputs | locally verified |
| Explicit `WITCHY` never silently measures a stale or missing executable | pre-timing rejection names the path or first newer compiler input | implemented |
| Machine-readable fast/full artifact | schema 1 JSON emitted beside both human paths; raw samples are retained and five focused integrity tests reject incomplete evidence | locally verified |
| Artifact identity and checksums | source revision/dirty state, binary path/mtime/hash, Rust/Cargo/Go/embedded Wasmtime, source-derived opt level, Binaryen, harness, driver argv, host, environment, benchmark source hashes, and both result hashes | locally verified |
| Promotion sampling contract | full mode uses two warmups and twelve paired interleaved samples | implemented |
| Fixed-arena-aware memory assertions | all heap ratios compare workload deltas; fixed-memory fixtures compare two sizes with a 64-byte tolerance | locally verified |
| Deterministic counters | three complete `Stats` values agree exactly | locally verified |
| Result correctness | `taskpolicy -c utility ./bench.sh --quick --json /tmp/rfc0146-track0-quick-all.json`: 17/17, artifact hash `5a8e32f1bc12665170542503be0c23405b3b08137d23e67910172cb35bf4f85a` | locally verified |
| Merge queue | terminal merged event and landed commit | pending submission |

## External artifact retention

Before closing a performance track, copy its raw local artifact bundle to a
durable path outside the repository and replace `pending` in its row with that
path. The ledger records summaries and hashes, not machine-specific raw data.

## Track 0 local verification

- `python3 -m unittest benchmarks/test_summarize.py -v`: 5 passed.
- `CARGO_TARGET_DIR=target cargo nextest run -p witchy 'stats::tests' --no-fail-fast`:
  54 passed, including the exact arena control and three-run counter equality.
- `taskpolicy -c utility ./bench.sh --quick --json
  /tmp/rfc0146-track0-quick-all.json`: 17/17 result matches.
- `python3` artifact invariant check: complete suite, seventeen records, matching
  output hashes, one positive sample per kernel benchmark, and the declared
  wall-only exception.
