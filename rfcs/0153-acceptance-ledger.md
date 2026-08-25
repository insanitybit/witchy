# RFC-0153 acceptance ledger

This ledger is the durable index for RFC-0153 promotion evidence. Raw timing,
counter, profile, and generated-WAT artifacts remain outside source control.
Every artifact must carry the schema 1 build identity emitted by `bench.sh`.

## Evidence contract

- Fast exploration uses `benchmarks/.build/results-fast.json` or `results-quick.json`.
  Fast and quick artifacts select work only; they never satisfy a promotion row.
- Full promotion uses schema 1 artifacts emitted by `bench.sh` with paired
  Witchy/Go kernel samples and warmups.
- Timing artifacts include source and binary identity, toolchains, host,
  optimization configuration, source digests, output checksums, raw samples,
  median, range, MAD, and a deterministic paired bootstrap 95 percent interval.
- Counter evidence is accepted only when three repetitions agree exactly.
- A target optimization starts only after a function-level profile and counters
  attribute at least 10 percent of that workload to an RFC-0153-owned cost. If
  they do not, the row transfers to a named RFC without an unrelated rewrite.
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
| 6A current-master attribution refresh | merged | fast snapshot at `d03861fd5`; promotion baseline pending | `samply` profile plus optimization counters for each target | Output hashes and focused benchmark correctness | `collatz`, `fannkuch`, `nsieve`, `list_index` | `perf/rfc0153-attribution-refresh` | `merged` @ `b248c60528a20a4331e137b61d162e39e57a539f` |
| 6B scalar range/arithmetic closure | blocked on 6A | matched pre-change artifact | Range-fact, arithmetic-rewrite, branch, and instruction attribution | `witchy-wir` facts/strength-reduce tests plus `collatz` output | Only targets with an attributed scalar cost, initially `collatz` | `perf/rfc0153-scalar-close` | pending |
| 6C sequence-plan closure | merged | matched pre-change artifact | Cursor, retained-check, header-load, allocation, and instruction attribution | `witchy-lower` codegen tests plus target outputs | Only targets with an attributed sequence cost: `nsieve`, `fannkuch`, `list_index` | `perf/rfc0153-sequence-close` | `merged` @ `4f3e516f349bc600fc6dd07befd0eee8d8d71130` |
| 6D generalization and adversarial matrix | ready | merged positive and near-miss unit fixtures are the starting point | Three identical stats payloads; emitted-Wasm checks | Differential outputs and exact trap/effect-order assertions | Six required families and three pre-existing non-benchmark consumers | `test/rfc0153-generalization-matrix` | pending |
| 6E integration and promotion closeout | blocked on 6A-6D | two warmups and twelve paired interleaved samples | Full stats, code-size, compiler-time, profile, and deoptimization matrix | Focused shards and serialized merge-queue gate | Every retained target <= 1.00x Go; protected rows within 5 percent | `integration/rfc0153-complete` | pending |

## Preparation checkpoint (2026-08-25)

The current local fast artifact is
`benchmarks/.build/results-fast.json`
(`sha256:044023a887764d65c6cbc261432dc61ca896f03a92538b7330337ffef8214ae9`).
It records clean source `d03861fd52d7ee07990dbe0b2afd903bf6a66af2`,
macOS arm64, Rust 1.97.1, Go 1.26.5, one warmup, and three paired,
counterbalanced samples. It is current work-selection evidence, not promotion
evidence:

| Target | Witchy median (ms) | Go median (ms) | Ratio | Preparation state |
|---|---:|---:|---:|---|
| `collatz` | 200.932 | 138.534 | 1.450x | attribution required |
| `nsieve` | 4.350 | 3.033 | 1.434x | attribution required |
| `fannkuch` | 157.960 | 129.273 | 1.222x | attribution required |
| `list_index` | 3.127 | 2.580 | 1.212x | attribution required |

Profiler discovery found `samply 0.13.1` available. `xctrace` is installed but
unusable with the active CommandLineTools developer directory; Valgrind,
Callgrind, Linux `perf`, and the listed allocation profilers are unavailable.
Track 6A therefore uses `samply` for wall-time attribution and existing Witchy
optimization counters. Allocation evidence is required only where the profile
attributes material allocator/runtime cost; a later environment may add a
native allocator probe without changing the frozen compiler contracts below.

### Dependency graph and critical path

```text
6A attribution -+-> 6B scalar closure ---+
                +-> 6C sequence closure -+-> 6E promotion and queue closeout
6D generalization matrix -----------------+
```

Tracks 6A and 6D start independently. Tracks 6B and 6C consume only attributed
costs and can run in parallel after 6A freezes their workload ownership. Track
6E is the integration owner and the only track allowed to mark an acceptance
criterion complete. The critical path is attribution, the slower of the scalar
or sequence closure, then the twelve-pair promotion run.

### Frozen shared contracts

1. `witchy-wir` owns `EvaluationProperties`, range facts, and arithmetic
   legality. Unknown effects, traps, allocation, memory roots, or wrapping lose
   the proof.
2. `witchy-lower` consumes facts only through canonical induction and the
   existing `SequenceAccessPlan`. No target or method-specific recognizer,
   runtime helper, or unchecked API is added.
3. The optimized and deoptimized paths preserve outputs, traps, effect order,
   root invalidation, and wrapping `Int` semantics. The interpreter remains the
   semantic oracle and does not implement the optimization.
4. Deterministic stats name every accepted transformation and decline reason;
   three payloads must agree byte-for-byte before timing promotion.
5. Target and protected benchmark sets, the six-family generalization matrix,
   the 6 percent Wasm-size limit, the 8 percent compiler-time limit, and the
   two-warmup/twelve-pair timing contract do not change during implementation.
6. A target with less than 10 percent attributable RFC-0153 cost transfers to a
   named later owner. That transfer may satisfy scope ownership, but it does not
   count as an RFC-0153 speedup.

### Track boundaries and handoff

| Track | Owned boundary | First verifying commands |
|---|---|---|
| 6A | Benchmark commands, raw profiles, counters, and attribution report; no production-code edits | Target output checks; three repeated counter payloads; `samply` profiles |
| 6B | `crates/witchy-wir/src/wir_opt/` and its focused tests | `cargo test -p witchy-wir`; matched target timing |
| 6C | `crates/witchy-lower/src/codegen/` and its focused tests | `cargo test -p witchy-lower`; matched target timing |
| 6D | Positive/near-miss fixtures, differential expectations, exact traps, and emitted-Wasm assertions | `./scripts/agent-check.sh parity`; path-routed focused checks |
| 6E | This ledger, integration conflict resolution, deoptimization matrix, code size, compiler time, promotion artifact, and queue evidence | `./scripts/test-for-paths.sh --run`; merge-queue gate |

## Historical Phase 0 comparison (exploratory only)

The table below was merged in the earlier closeout attempt. It is retained for
history, but it does not satisfy Track 6: no durable twelve-pair artifact is
indexed here, and `collatz` plus `nsieve` missed the required 1.00x threshold.

| Benchmark | Baseline Witchy (ms) | Candidate Witchy (ms) | Go Kernel (ms) | Speedup vs Baseline | Candidate Ratio vs Go | Output Status |
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
