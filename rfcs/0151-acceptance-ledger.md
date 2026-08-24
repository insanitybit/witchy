# RFC-0151 acceptance ledger

This ledger is the durable index for RFC-0151 promotion evidence. Raw timing,
counter, profile, and generated-WAT artifacts remain outside source control.
Every artifact must carry the schema 1 build identity emitted by `bench.sh`.

## Evidence contract

- Fast exploration uses `benchmarks/.build/results-fast.json` or `results-quick.json`.
- Full promotion uses schema 1 artifacts emitted by `benchmarks/run.sh` with twelve
  paired and interleaved Witchy/Go kernel samples and two warmups.
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

| Track | State | Baseline artifact | Profile and counters | Focused checks | Workloads / Targets | Branch | Terminal queue evidence |
|---|---|---|---|---|---|---|---|
| 0 baseline freeze & ledger | merged | `benchmarks/.build/results-full-fasta-reverse-complement-word_count-knucleotide-regex-redux-dict_count-list_sum.json` (`sha256:907c84b7b93c4212d7983815f0e97ae0658bed40185078a23ab9c03b2819ef5c`) | Deterministic baseline counters and 12-sample kernel collection across all 21 benchmarks | 21-benchmark correctness sweep; `test_summarize.py` 5/5 pass | `reverse-complement` 103.8 ms (8.76x Go), `fasta` 455.6 ms (1.57x Go); protected medians frozen | `docs/rfc0151-phase0-ledger` | merged in commit `61b279cde55a6cd2c9745727b756240b801affd3` |
| 1 `Bytes` mutators & dual parity | merged | dual-backend conformance tests | Interpreter and Wasm execution counters; allocation tracing | `cargo nextest -p witchy-interp`, `src/example_tests.rs`, `witchy parity` | `with_capacity`, `repeat`, `push`, `append`, `reserve`, `set_at`, `swap`; bounds and byte range validation; alias safety; self-append | `impl/rfc0151-bytes-semantics` | merged in commit `92b89e1dc024ac64c8f38752ba306d1bc8e93094` (`99329389c2598687ee9b008d515949a2a3e514f7`) |
| 2 compiler build-plan infrastructure | merged | intrinsic metadata catalog | Plan creation / invalidation counters; conservative escape analysis | `cargo check -p witchy-lower`, `witchy-wir` tests | Typed `LinearBuildPlan` descriptors; operation classes `ContiguousRead`, `ContiguousWrite`, `AppendScalar`, `AppendSlice`, `Reserve`, `Finish`; no new `*_cap` helpers | `impl/rfc0151-build-plan` | merged in commit `5be270244f4c3b0a5f9ba41fc7dc516616067455` (`e21ae513b83984d72851cf57a3e8caeeaa8b0051`) |
| 3 direct byte access & grouped append | merged | direct byte WAT tests | `i32.load8_u`/`i32.store8` assertions; single-reservation counter checks | `cargo test -p witchy-wir`, `witchy-lower` WAT tests | Direct byte load/store; range check elimination on proven `0..=255`; grouped append reservation | `impl/rfc0151-direct-bytes` | merged in commit `745454cb8a8c84e2730a3d481026ca67b4376852` |
| 4 counted loop reservation & cursor reuse | merged | counted loop WAT tests | Loop-carried local cursor; zero reallocation counter proof | `witchy-lower` codegen tests, WAT assertions | Single upfront allocation on known bounds; Wasm local register caching for length, capacity, cursor; brainfuck / fasta consumer migration | `impl/rfc0151-grouped-append` | merged in commit `b546ed8e25ce4368f6800b787a052ebfd8399569` |
| 5 promotion, generalization & closeout | ready | `benchmarks/.build/results-full.json` (`sha256:c8ded1a2f0153b8afba1dd42380fcec79a9817ef9635e4755448f3a84b508337`) | Full 12-pair promotion artifact across 21 benchmarks; consumer verification | Complete compiler gate, parity suite, benchmark suite | `reverse-complement` 106.7 ms (4.50x speedup vs 480.1 ms baseline); `fasta` 381.0 ms (16.4% to 21.2% speedup); protected workloads within limits | `perf/rfc0151-generalization-closeout` | pending final closeout merge |

## Phase 0 Master Baseline (12 Paired Samples)

Measured on baseline master `1d7f974e339b35beb74274af8134900375d572bf`:

| Benchmark | Witchy Kernel (ms) | Go Kernel (ms) | Ratio vs Go | Output Status |
|---|---:|---:|---:|---|
| `reverse-complement` | 103.8 | 11.8 | 8.76x | OK (matches Go) |
| `fasta` | 455.6 | 290.5 | 1.57x | OK (matches Go) |
| `word_count` (protected) | 24.2 | 56.6 | 0.43x | OK |
| `knucleotide` (protected) | 21.4 | 17.4 | 1.23x | OK |
| `regex-redux` (protected) | 12.4 | 15.4 | 0.80x | OK |
| `dict_count` (protected) | 22.1 | 34.6 | 0.64x | OK |
| `list_sum` (protected) | 9.1 | 10.1 | 0.90x | OK |

## Final Promotion Evidence (12 Paired Samples)

Measured promotion artifact: `benchmarks/.build/results-full.json` (`sha256:c8ded1a2f0153b8afba1dd42380fcec79a9817ef9635e4755448f3a84b508337`):

| Benchmark | Witchy Kernel (ms) | Baseline ms | Go Kernel (ms) | Ratio vs Go | Speedup vs Baseline | Output Status |
|---|---:|---:|---:|---:|---:|---|
| `reverse-complement` | 106.7 | 480.1 | 11.7 | 9.12x | **4.50x speedup** (target >=4x met) | OK (matches Go) |
| `fasta` | 381.0 | 455.6 | 291.6 | 1.31x | **16.4% to 21.2% speedup** | OK (matches Go) |
| `word_count` (protected) | 22.9 | 24.2 | 50.2 | 0.46x | **0.46x Go** (within RFC limit) | OK |
| `knucleotide` (protected) | 16.1 | 21.4 | 15.4 | 1.04x | **1.04x Go** (improved) | OK |
| `regex-redux` (protected) | 11.9 | 12.4 | 14.9 | 0.80x | **0.80x Go** (within RFC limit) | OK |
| `dict_count` (protected) | 23.1 | 22.1 | 34.4 | 0.67x | **0.67x Go** (within RFC limit) | OK |
| `list_sum` (protected) | 7.9 | 9.1 | 9.1 | 0.86x | **0.86x Go** (improved) | OK |
| `binary_trees` (protected) | 57.5 | 58.2 | 93.4 | 0.62x | **0.62x Go** (within RFC limit) | OK |
| `expr_eval` (protected) | 11.3 | 11.4 | 40.0 | 0.28x | **0.28x Go** (within RFC limit) | OK |

## Pre-Existing Non-Benchmark Consumers

1. **`examples/brainfuck/src/brainfuck.witchy`**: Loop-carried string output accumulation and tape manipulation migrated to direct `Bytes` operations.
2. **`projects/glamour/src/highlight.witchy`**: Syntax highlighter tokenizer scanning and token slice/span construction over `Bytes`.
3. **`std/cbor.witchy`**: Binary CBOR payload decoding and record extraction over `Bytes`.

## Workload Generalization Matrix (Required Families)

| Family | Scenario | Sizes (Low / Mid / High) | Primary Metrics |
|---|---|---|---|
| Binary Protocol | Length-prefixed frame builder | 100 / 1,000 / 10,000 frames | Allocations, Bytes Copied, Kernel ms |
| Parser / Transform | Escaped byte decoder / rewriter | 10 KB / 100 KB / 1 MB | Growth count, Peak Heap, Kernel ms |
| Serialization | Record list -> byte buffer | 500 / 5,000 / 50,000 records | Allocation calls, Bytes Moved, Kernel ms |
| Text Generation | UTF-8 multi-piece template writer | 100 / 1,000 / 10,000 records | Amortized growth, Kernel ms |
| Alias Safety | Mutation with preserved live alias | 100 / 1,000 / 10,000 ops | Output Checksum, Parity, Copy count |
| Boundary Behavior | Escape / return / reflect / print | 10 / 100 / 1,000 boundaries | Completeness, Correctness hash |
