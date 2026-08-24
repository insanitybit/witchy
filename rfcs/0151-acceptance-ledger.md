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
| 0 baseline freeze & ledger | ready | `benchmarks/.build/results-full-fasta-reverse-complement-word_count-knucleotide-regex-redux-dict_count-list_sum.json` (`sha256:907c84b7b93c4212d7983815f0e97ae0658bed40185078a23ab9c03b2819ef5c`) | Deterministic baseline counters and 12-sample kernel collection across all 21 benchmarks | 21-benchmark correctness sweep; `test_summarize.py` 5/5 pass | `reverse-complement` 103.8 ms (8.76x Go), `fasta` 455.6 ms (1.57x Go); protected medians frozen | `docs/rfc0151-phase0-ledger` | pending submission |
| 1 `Bytes` mutators & dual parity | planned | dual-backend conformance tests | Interpreter and Wasm execution counters; allocation tracing | `cargo nextest -p witchy-interp`, `src/example_tests.rs`, `witchy parity` | `with_capacity`, `repeat`, `push`, `append`, `reserve`, `set_at`, `swap`; bounds and byte range validation; alias safety; self-append | `impl/rfc0151-bytes-semantics` | pending |
| 2 compiler build-plan infrastructure | planned | intrinsic metadata catalog | Plan creation / invalidation counters; conservative escape analysis | `cargo check -p witchy-lower`, `witchy-wir` tests | Typed `LinearBuildPlan` descriptors; operation classes `ContiguousRead`, `ContiguousWrite`, `AppendScalar`, `AppendSlice`, `Reserve`, `Finish`; no new `*_cap` helpers | `impl/rfc0151-build-plan-infra` | pending |
| 3 direct byte access & grouped append | planned | direct byte WAT tests | `i32.load8_u`/`i32.store8` assertions; single-reservation counter checks | `cargo test -p witchy-wir`, `witchy-lower` WAT tests | Direct byte load/store; range check elimination on proven `0..=255`; grouped append reservation | `impl/rfc0151-direct-bytes-grouped-append` | pending |
| 4 counted loop reservation & cursor reuse | planned | counted loop WAT tests | Loop-carried local cursor; zero reallocation counter proof | `witchy-lower` codegen tests, WAT assertions | Single upfront allocation on known bounds; Wasm local register caching for length, capacity, cursor | `impl/rfc0151-loop-reservation-cursor` | pending |
| 5 promotion, generalization & closeout | planned | 12-pair full promotion artifact | Full generalization matrix across 6 families at 3 geometric sizes; non-benchmark timings | Complete compiler gate, parity suite, benchmark suite | `reverse-complement` >= 4x faster and <= 1.00x Go; `fasta` >= 20% faster and <= 1.00x Go; 3 non-benchmark families improved >= 20% (including 2 pre-existing consumers); protected benchmarks within 5% | `perf/rfc0151-generalization-closeout` | pending |

## Phase 0 Master Baseline (12 Paired Samples)

Measured on current master `1d7f974e339b35beb74274af8134900375d572bf`:

| Benchmark | Witchy Kernel (ms) | Go Kernel (ms) | Ratio vs Go | Output Status |
|---|---:|---:|---:|---|
| `reverse-complement` | 103.8 | 11.8 | 8.76x | OK (matches Go) |
| `fasta` | 455.6 | 290.5 | 1.57x | OK (matches Go) |
| `word_count` (protected) | 24.2 | 56.6 | 0.43x | OK |
| `knucleotide` (protected) | 21.4 | 17.4 | 1.23x | OK |
| `regex-redux` (protected) | 12.4 | 15.4 | 0.80x | OK |
| `dict_count` (protected) | 22.1 | 34.6 | 0.64x | OK |
| `list_sum` (protected) | 9.1 | 10.1 | 0.90x | OK |

## Pre-Existing Non-Benchmark Consumers

1. **`examples/brainfuck/src/brainfuck.witchy`**: Loop-carried string output accumulation and tape manipulation.
2. **`projects/glamour/src/highlight.witchy`**: Syntax highlighter tokenizer scanning and token slice/span construction.
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
