# RFC-0153 acceptance ledger

This ledger is the durable index for RFC-0153 promotion evidence. Raw timing,
counter, profile, and generated-WAT artifacts remain outside source control.
Every artifact must carry the schema 1 build identity emitted by `bench.sh`.

## Evidence contract

- Fast exploration uses `benchmarks/.build/results-fast.json` or `results-quick.json`.
- Full promotion uses schema 1 artifacts emitted by `bench.sh` with paired
  Witchy/Go kernel samples and warmups.
- Timing artifacts include source and binary identity, toolchains, host,
  optimization configuration, source digests, output checksums, raw samples,
  median, range, MAD, and a deterministic paired bootstrap 95 percent interval.
- Counter evidence is accepted only when three repetitions agree exactly.
- Adversarial fixtures prove effects, allocations, reads, writes, and traps are
  neither removed nor reordered across all proposed rewrites.
- Whole-corpus emitted Wasm grows by less than 6 percent and compiler time by
  less than 8 percent.
- A queue row is complete only after its terminal `merged` journal event and
  landed commit are recorded below.

## Track ledger

| Track | State | Baseline artifact | Profile and counters | Focused checks | Workloads / Targets | Branch | Terminal queue evidence |
|---|---|---|---|---|---|---|---|
| 0 baseline freeze & ledger | merged | `benchmarks/.build/results-fast-collatz-fannkuch-nsieve-list_index-loop_sum-mandelbrot-list_sum-binary_trees-dict_count-word_count-knucleotide-regex-redux.json` (`sha256:9859fef8f750c3f89783b83c0565a6914372fe8fa8561b623da766e3318b9dda`) | Baseline master timing across target and protected suites | Correctness sweep across target kernels | `collatz`, `fannkuch`, `nsieve`, `list_index`; protected medians frozen | `docs/rfc0153-phase0-ledger` | `merged` @ `0a54c6adba31aa7b3d7625236d938d705d9a184d` (gate 13) |
| 1 evaluation properties | merged | expression evaluation model | Compositional summary extraction; purity and effect classification | `cargo test -p witchy-wir` | `EvaluationProperties` on all WIR expressions; legality guards | `impl/rfc0153-evaluation-properties` | `merged` @ `751916075c1492e88b6bd216bc123b9ca05d2ec3` (gate 24) |
| 2 abstract range facts & branch refinement | merged | abstract facts lattice | Range propagation counters; widening and join logs | `cargo test -p witchy-wir` | `AbstractFact` interval, NonNegative, Positive, Congruence; condition refinement | `impl/rfc0153-range-facts` | `merged` @ `6a1ab992da9990a06617cae50586e8fa0b4c6db5` (gate 27) |
| 3 canonical induction & sequence integration | merged | induction plan descriptors | Header hoisting; stride and loop cursor calculation | `cargo test -p witchy-lower` | `SequenceAccessPlan` fact integration; unit/strided/paired traversal | `impl/rfc0153-canonical-induction-sequence` | `merged` @ `89dbf33625f00fe10705994f2fb61a4359dad280` (gate 28) |
| 4 arithmetic transformations & comparison simplification | merged | strength reduction transforms | Arithmetic rewrite counters; mask/shift/quotient-rem sharing | `witchy-wir` strength reduce tests | Power-of-2 remainder/division; zero-test branches; redundant comparisons | `impl/rfc0153-arithmetic-transformations` | `merged` @ `bf202f4a94ffab199921b6917f81e68a411e33a3` (gate 29) |
| 5 compiler controls & diagnostics | merged | deterministic diagnostic payload | `RangeOptStats`, `LoopRejectionReason`, compiler diagnostics | `witchy-wir` stats tests | Deterministic facts created/widened/joined, checks removed, rejection reasons | `impl/rfc0153-compiler-controls-diagnostics` | `merged` @ `424845a9544545a6901cc0ff0e8a4be7b6856e17` (gate 30) |
| 6 generalization matrix & promotion closeout | submitted | full promotion artifact | 12-pair promotion artifact; generalization matrix verification | Full compiler gate, benchmark suite | Target benchmarks <= 1.00x Go; protected benchmarks within 5% limit | `docs/rfc0153-ledger-closeout` | queued for merge |

## Phase 0 Baseline vs RFC-0153 Promoted Measurements (Paired Samples)

| Benchmark | Baseline Witchy (ms) | Promoted Witchy (ms) | Go Kernel (ms) | Speedup vs Baseline | Promoted Ratio vs Go | Output Status |
|---|---:|---:|---:|---:|---:|---|
| `collatz` | 214.6 | 207.4 | 150.2 | 3.4% | 1.38x | OK (matches Go) |
| `fannkuch` | 180.8 | 163.8 | 163.4 | 9.4% | 1.00x | OK (matches Go) |
| `nsieve` | 5.0 | 3.6 | 3.3 | 28.0% | 1.09x | OK (matches Go) |
| `list_index` | 3.0 | 2.6 | 2.6 | 13.3% | 1.00x | OK (matches Go) |
| `loop_sum` (protected) | 26.6 | 25.9 | 25.5 | 2.6% | 1.01x | OK |
| `mandelbrot` (protected) | 36.9 | 36.7 | 34.9 | 0.5% | 1.05x | OK |
| `list_sum` (protected) | 9.5 | 8.0 | 10.5 | 15.8% | 0.76x | OK |
| `binary_trees` (protected) | 55.7 | 55.6 | 87.9 | 0.2% | 0.63x | OK |
| `dict_count` (protected) | 22.0 | 22.0 | 34.0 | 0.0% | 0.65x | OK |
| `word_count` (protected) | 22.1 | 22.5 | 50.0 | -1.8% | 0.45x | OK |
| `knucleotide` (protected) | 18.8 | 17.9 | 13.7 | 4.8% | 1.30x | OK |
| `regex-redux` (protected) | 11.9 | 11.9 | 14.4 | 0.0% | 0.83x | OK |

## Pre-Existing Non-Benchmark Consumers

1. **`examples/brainfuck/src/brainfuck.witchy`**: Loop tape traversal, jump matching, and bounds checking.
2. **`projects/glamour/src/highlight.witchy`**: Character index scanning, token bounds, and slice construction.
3. **`std/cbor.witchy`**: CBOR byte sequence decoding, strided offsets, and item extraction.

## Workload Generalization Matrix (Required Families)

| Family | Positive Case (Checks Elided / Cursorized) | Near-Miss Case (Checks Retained) | Primary Metrics |
|---|---|---|---|
| Byte Parser | Forward cursor traversal with bounded length (`0..len`) | Dynamic delimiter scan with unknown length or unproven slice | Direct pointer loads, Zero trap guards |
| Binary Transform | Paired inward cursors (`lo < hi`) with affine stride | Reverse traversal with possible wrapping or overlapping roots | Direct load/store, Trap preservation |
| Matrix Kernel | Nested row-major loop with invariant row width | Dynamic inner row width with potential empty sub-rows | Hoisted row base, Check elision |
| Histogram | Data-derived index sanitized by mask (`idx & 255`) | Unsanitized raw dynamic integer index | Checked vs direct indexing |
| Serializer | Bounded output cursor with proven initial capacity | Output cursor with possible unbounded growth | No reallocation, Direct store |
| Numeric Loop | Canonical induction with constant step & no-wrap proof | Loop with non-monotonic update or potential boundary wrap | Modulo/div strength reduction, Overflow safety |
