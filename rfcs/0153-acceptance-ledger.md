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
| 0 baseline freeze & ledger | queued | `benchmarks/.build/results-fast-collatz-fannkuch-nsieve-list_index-loop_sum-mandelbrot-list_sum-binary_trees-dict_count-word_count-knucleotide-regex-redux.json` (`sha256:9859fef8f750c3f89783b83c0565a6914372fe8fa8561b623da766e3318b9dda`) | Baseline master timing across target and protected suites | Correctness sweep across target kernels | `collatz`, `fannkuch`, `nsieve`, `list_index`; protected medians frozen | `docs/rfc0153-phase0-ledger` | pending queue |
| 1 evaluation properties | planned | expression evaluation model | Compositional summary extraction; purity and effect classification | `cargo test -p witchy-wir` | `EvaluationProperties` on all WIR expressions; legality guards | `impl/rfc0153-evaluation-properties` | pending queue |
| 2 abstract range facts & branch refinement | planned | abstract facts lattice | Range propagation counters; widening and join logs | `cargo test -p witchy-wir` | `AbstractFact` interval, NonNegative, Positive, Congruence; condition refinement | `impl/rfc0153-range-facts` | pending queue |
| 3 canonical induction & sequence integration | planned | induction plan descriptors | Header hoisting; stride and loop cursor calculation | `cargo test -p witchy-lower` | `SequenceAccessPlan` fact integration; unit/strided/paired traversal | `impl/rfc0153-canonical-induction-sequence` | pending queue |
| 4 cursor and bounds promotion | planned | direct kernel lowering | Direct load/store counts; bounds-check elimination counters | `witchy-lower` codegen tests | Direct pointer loops; bounds-check elision on proven nonnegative range | `impl/rfc0153-cursor-bounds-opt` | pending queue |
| 5 arithmetic transformations | planned | strength reduction transforms | Arithmetic rewrite counters; mask/shift/quotient-rem sharing | `witchy-wir` strength reduce tests | Power-of-2 remainder/division; zero-test branches; redundant comparisons | `impl/rfc0153-arithmetic-simplification` | pending queue |
| 6 generalization matrix & promotion closeout | planned | full promotion artifact | 12-pair promotion artifact; generalization matrix verification | Full compiler gate, benchmark suite | Target benchmarks <= 1.00x Go; protected benchmarks within 5% limit | `perf/rfc0153-generalization-closeout` | pending queue |

## Phase 0 Master Baseline (Paired Samples)

Measured on baseline master `83dd859cf478692db90b54407ca35416c33aa374`:

| Benchmark | Witchy Kernel (ms) | Go Kernel (ms) | Ratio vs Go | Output Status |
|---|---:|---:|---:|---|
| `collatz` | 214.6 | 150.2 | 1.43x | OK (matches Go) |
| `fannkuch` | 180.8 | 163.4 | 1.11x | OK (matches Go) |
| `nsieve` | 5.0 | 3.3 | 1.52x | OK (matches Go) |
| `list_index` | 3.0 | 2.6 | 1.15x | OK (matches Go) |
| `loop_sum` (protected) | 26.6 | 25.5 | 1.04x | OK |
| `mandelbrot` (protected) | 36.9 | 34.9 | 1.06x | OK |
| `list_sum` (protected) | 9.5 | 10.5 | 0.90x | OK |
| `binary_trees` (protected) | 55.7 | 87.9 | 0.63x | OK |
| `dict_count` (protected) | 22.0 | 34.0 | 0.65x | OK |
| `word_count` (protected) | 22.1 | 50.0 | 0.44x | OK |
| `knucleotide` (protected) | 18.8 | 13.7 | 1.37x | OK |
| `regex-redux` (protected) | 11.9 | 14.4 | 0.83x | OK |

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
