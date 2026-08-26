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
| 6A current-master attribution refresh | merged | full artifact `/tmp/rfc0153-promotion-87ccb7e/results-full.json` at `87ccb7e9c` | three identical counter payloads; macOS `samply` emitted zero samples and is unusable for function attribution | all 21 Witchy/Go output hashes match | `collatz`, `fannkuch`, `nsieve`, `list_index` | `perf/rfc0153-attribution-refresh` | `merged` @ `b248c60528a20a4331e137b61d162e39e57a539f` |
| 6B scalar range/arithmetic closure | transferred | full 12-pair artifact shows `collatz` 1.666x Go | `all` and `none` emit identical collatz WAT; all sequence counters are zero | focused WIR effect-safety and arithmetic tests | `collatz` transfers to RFC-0146 Track 5 (small-leaf inlining/register SROA) | `perf/rfc0153-scalar-close` | no code change required; transfer evidence recorded below |
| 6C sequence-plan closure | merged, residual open | full artifact plus `/tmp/rfc0153-bounds-lever-87ccb7e` lever bisection | exact counters: bounds-elide reduces fannkuch 254.2 ms to 165.9 ms and list_index 5.24 ms to 2.69 ms | byte range/while, list cursor, and Brainfuck fallback codegen tests | `fannkuch`, `list_index` remain owned; nsieve residual transfers to RFC-0146 Track 6 | `perf/rfc0153-sequence-close` | `merged` @ `4f3e516f349bc600fc6dd07befd0eee8d8d71130`, `baa611383`, `d94431080` |
| 6D generalization and adversarial matrix | in progress | byte, paired binary, histogram, matrix, and fixed-output positive/near-miss fixtures | three identical byte/list stats payloads; WAT assertions cover byte, list, and real-consumer positive/fallback slices | fixture outputs/traps plus Glamour markdown and `std/cbor` WebAuthn backend parity; full `OptSet::none()` result/trap matrix | numeric boundary and real-consumer attribution extensions are pending queue evidence; the full deoptimization census is covered by `rfc0153_full_deoptimization_matrix_preserves_results_and_traps` | `test/rfc0153-generalization-matrix` | partial checks merged @ `a4562063e`, `87ccb7e9c`; matrix/serializer extension merged @ `cd02dfd6919272d999852d564bebd45bf1100541` (gate 60s); boundary and consumer extensions are pending queue evidence |
| 6E integration and promotion closeout | blocked on 6A-6D | two warmups and twelve paired interleaved samples | Full stats, code-size, compiler-time, profile, and deoptimization matrix | Focused shards and serialized merge-queue gate | Every retained target <= 1.00x Go; protected rows within 5 percent | `integration/rfc0153-complete` | pending |

## Preparation checkpoint (2026-08-25)

## Current promotion checkpoint (2026-08-25)

The latest complete promotion artifact is
`/tmp/rfc0153-promotion-87ccb7e/results-full.json`, produced by the release
binary `13aab8c029191e4016216c2ab6014b8a0c2b3bd5fb9f126a6547d2bdc41d1878`
from `87ccb7e9c6c387ce339b3cd7f6b76662ce52c9c7`. It contains all 21 workload
output checksums, two warmups, and 12 counterbalanced paired kernel samples.
Every output matches its Go counterpart, but criterion 5 is not yet met:

| Target | Kernel ratio to Go | RFC-0153 ownership decision |
|---|---:|---|
| `collatz` | 1.666x | transfer to RFC-0146 Track 5: the all/none WAT is identical and sequence counters are zero |
| `nsieve` | 1.384x | transfer to RFC-0146 Track 6: residual is packed-boolean layout, not a live bounds-elision path |
| `fannkuch` | 1.233x | retained: `bounds-elide` causally removes a 35 percent kernel cost |
| `list_index` | 1.203x | retained: `bounds-elide` causally removes a 49 percent kernel cost |

The matching bisection artifact
`/tmp/rfc0153-bounds-lever-87ccb7e` compares `release,-bounds-elide` to
`release` with two warmups and 12 counterbalanced samples. Both workloads have
identical output hashes. It measures fannkuch medians of 254.2 ms and 165.9 ms,
and list_index medians of 5.24 ms and 2.69 ms, respectively. This proves the
two retained rows are materially RFC-0153-owned; it does not establish their
remaining Go-parity requirement.

### Post-forward mechanism checkpoint (2026-08-25)

`/tmp/rfc0153-forward-refresh-fd542b3e6` is a two-warmup, twelve-pair
counterbalanced matrix between the pre-forward control binary
(`sha256:92fc3e7481b3c23f2f62b3bcef812bf7645143afe796c0ba01edf052f45b3e41`)
and current master `fd542b3e6` (`sha256:d45a956c2953aa87fa7ac1e0734c699ce0e6905df7ca18eb54ee510c0fb66947`).
The artifact records identical output hashes and three exact counter payloads
for both retained workloads.  The current binary reports 6,235,300
`sequence_forwarded_loads` in `fannkuch`; the control reports zero.

| Workload | Control median | Forwarding median | Ratio | Interpretation |
|---|---:|---:|---:|---|
| `fannkuch` | 160.199 ms | 158.053 ms | 0.9866x | 1.34% improvement; retained mechanism evidence only |
| `list_index` | 2.664 ms | 2.642 ms | 0.9916x | 0.84% improvement; no promotion credit |

This confirms that immediate checked-store forwarding is live in the current
binary and preserves results. It does **not** meet the 10% real-workload row
threshold or the target Go-parity requirement, so it cannot close Track 6C or
6E.

### Guarded range completion checkpoint (2026-08-26)

`/tmp/rfc0153-guarded-range/matrix-fann-20260826-154214` is a two-warmup,
twelve-pair counterbalanced matrix between control
`0feee2551e7a4eea58b52cb24849b29fce1cc17473bc41e636a6546013c7200b` and
candidate `a178e4c902f0a5fa7cbde4f89761524456506d10f37c535643e638b1c98b38d4`.
It records identical fannkuch output hashes and three exact counter payloads.
The guarded normal-completion proof raises cursorized accesses from
319,638,933 to 338,344,833 and removes three `list_at` fallbacks.

| Workload | Control median | Guarded-range median | Ratio | Interpretation |
|---|---:|---:|---:|---|
| `fannkuch` | 156.965 ms | 150.266 ms | 0.9573x | 4.27% improvement; retained mechanism evidence only |

`d2b5d6c6fb1cc45fca765bc9670d340d5296e993` merged this proof and its
positive, deoptimized, zero-trap, and reordered-update fallback coverage via
`mq-3ed9884b2118a96b7e231fe20f2ca34fcbdd0d6b` (gate 262s). The gain is
attributable and correctness-preserving, but remains below the 10% promotion
threshold and therefore cannot close Track 6C or 6E.

### Deoptimization repair checkpoint (2026-08-26)

Two regressions found while exercising the required forced-copy matrix were
repaired and merged through the serialized queue:

- `3dd3638597aea4195d8797ed88534d28b2423f57` gates immediate checked
  store-to-load forwarding on `inplace`. Before this repair, `fannkuch`
  produced `0` under `WITCHY_OPT=release,-inplace` and `WITCHY_OPT=none`;
  the repaired release binary returns the expected `73196038` in `all`,
  `release,-inplace`, and `none`.
- `d59e274c7e3fb601eb75ec3f1491e9813cf25ad7` declares a zero-capacity
  ownership slot for any surviving forced-copy structural fallback. Before it,
  `knucleotide` failed to encode with `unknown local $counts__cap` under
  `-inplace`. The focused lowering test compiles the linked benchmark in that
  mode, and the in-place gate now classifies its known copying-path failure as
  the allowed resource cliff rather than an invalid WIR module.

The fresh deterministic current-master artifact is
`/tmp/rfc0153-attribution-d59e274c`. It records three identical counter
payloads and matching optimized output checksums. It confirms the retained
target loops have no checked indexed loads: `fannkuch` has 319,638,933
cursorized accesses and 9,864,090 coalesced bounds checks, while `list_index`
has 10,000,000 cursorized accesses. This repair/attribution checkpoint does
not close the complete deoptimization census, the promotion matrix, or the
Go-parity criteria.

### Range-value regression recovery checkpoint (2026-08-26)

`c2f993655` correctly removed an unsound proof that treated an arbitrary
`list.at` result as the source list's length; it consequently removed the
second inward cursor in fannkuch. The replacement records a strictly separate
value-domain fact only for a list built by exact `0..<n` pushes, preserves it
only through same-domain writes and a whole exact-range copy, and carries a
loaded scalar domain through a plain alias. Unknown writes, arithmetic
rebinding, and root/bound invalidation all drop the fact.

The local mechanism artifact
`benchmarks/.build/rfc0153-fannkuch-range-domain-20260826-221911` compares
master binary `bd8c6b55d383ac0d30281119cea7a7a30c6dab77d3162fcbe5859c5218f0faa4`
to candidate `03bc2f251ba7e5cac931f080127cf4141e86892420374a39afdfff13baef9f17`.
It has two warmups, twelve counterbalanced pairs, three byte-identical counter
payloads, and matching output hashes for both workloads. This is retained
mechanism evidence, not the required all-workload promotion artifact.

| Workload | Control median | Candidate median | Ratio | Deterministic counter result |
|---|---:|---:|---:|---|
| `fannkuch` | 1428.300 ms | 1036.105 ms | 0.7254x | checked loads and stores each fall by 57,996,463; cursorized accesses rise by 115,992,926 |
| `list_index` | 7.036 ms | 6.998 ms | 0.9946x | all seven recorded sequence counters are unchanged |

The focused codegen fixture executes optimized and fully deoptimized forms of
the copied-domain paired cursor and asserts four fewer `list_at` calls. The
existing `[99]` arbitrary-element fixture continues to take the observable
`list_at` trap in both modes. Queue and broad-gate evidence remain pending.

The first queue attempt also exposed a deoptimization-performance regression:
the candidate's async differential fuzz timed out at 83.48 seconds while an
exact parent control passed in 48.41 seconds. The cause was value-domain
bookkeeping running even with `OptSet::none()`. The repaired implementation
makes those fact producers a no-op unless `bounds-elide` is enabled; the exact
candidate rerun then passed in 52.34 seconds. This establishes the repair's
fully-deoptimized behavior, but does not replace required queue evidence.

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

### Real-consumer mechanism checkpoint (2026-08-26)

Focused emitted-WAT attribution now exercises three unchanged repository
consumers without treating semantic parity as a promotion claim:

- `std/regex.witchy` `find_all` has a locally monotonic `i < len` byte cursor.
  Its generated function contains a direct `i32.load8_u` and no
  `__witchy_abort` call for that access.
- `std/cbor.witchy` `read_uint` keeps `__witchy_abort`: callers establish a
  range with `need`, but that result is not an interprocedural length proof for
  the private reader's `start + k` accesses.
- `examples/brainfuck/src/brainfuck.witchy` keeps the byte bounds abort for its
  dynamic tape pointer. Its program counter is deliberately not classified as
  canonical induction because bracket matching can rebind it through a call.

`regex_span_decoder_uses_a_stable_byte_sequence_plan`,
`cbor_fixed_width_reader_retains_caller_dependent_byte_guard`, and
`brainfuck_dynamic_tape_pointer_retains_byte_bounds_guard` each pass. This is
mechanism and safety evidence only; it records no timing promotion row.

## Workload Generalization Matrix (Required Families)

| Family | Positive Case (Checks Elided / Cursorized) | Near-Miss Case (Checks Retained) | Primary Metrics |
|---|---|---|---|
| Byte Parser | Forward cursor traversal with bounded length (`0..len`) | Dynamic delimiter scan with unknown length or unproven slice | Direct pointer loads, Zero trap guards |
| Binary Transform | Paired inward cursors (`lo < hi`) with affine stride | Reverse traversal with possible wrapping or overlapping roots | Direct load/store, Trap preservation |
| Matrix Kernel | Nested row-major loop with invariant row width | Dynamic inner row width with potential empty sub-rows | Hoisted row base, Check elision |
| Histogram | Data-derived index sanitized by mask (`idx & 255`) | Unsanitized raw dynamic integer index | Checked vs direct indexing |
| Serializer | Bounded output cursor with proven initial capacity | Output cursor with possible unbounded growth | No reallocation, Direct store |
| Numeric Loop | Canonical induction with constant step & no-wrap proof | Loop with non-monotonic update or potential boundary wrap | Modulo/div strength reduction, Overflow safety |

### Executable 6D fixture index (2026-08-25)

- Byte parser: `byte_counted_range_elides_only_the_shared_bounds_proof` and
  `byte_while_length_snapshot_elides_only_stable_cursor_bounds` prove direct
  loads, bounds-off guards, output equality, and optimized/deoptimized traps.
- Binary transform: `paired_cursors_elide_only_a_stable_length_minus_constant_bound`
  proves inward reads and writes, with a too-large upper cursor retaining its
  exact `list_at` trap.
- Matrix and serializer: `matrix_and_fixed_output_cursors_elide_only_proven_bounds`
  proves nested row traversal and a literal preallocated output buffer, each
  with bounds-on/off WAT shape plus a dynamic-width or undersized-buffer trap.
- Histogram: `masked_static_histogram_index_elides_only_the_range_proven_guard`
  proves a masked signed index and rejects raw and cross-function lookalikes.
- Numeric loop: `power_of_two_modulo_strength_reduction` proves the signed
  modulo negative case alongside the optimized positive remainder shape.
  `rfc0153_full_deoptimization_matrix_preserves_results_and_traps` adds a
  positive modulo loop, empty range, maximum-Int no-wrap, loop-local wrapping
  with break, and zero-step false-entry parity under the default and fully
  deoptimized pipelines. The non-terminating zero-step shape separately proves
  that cursorization is declined in WAT.
- Full deoptimization: `rfc0153_full_deoptimization_matrix_preserves_results_and_traps`
  executes the byte-parser, paired-binary, matrix, histogram, serializer,
  numeric-loop, and same-address-forwarding positive fixtures under both the
  default set and `OptSet::none()`. It also checks the byte and each list-family
  negative fixture traps in both modes.
- Pre-existing consumers: `brainfuck_dynamic_tape_pointer_retains_byte_bounds_guard`,
  Glamour's `markdown_code_fence_carries_its_language_class_on_both_backends`,
  and `webauthn_verify_registration_parses_the_create_ceremony` keep the
  Brainfuck, Glamour, and `std/cbor` paths under executable interpreter/Wasm
  semantic coverage. These are protected-source evidence, not performance rows
  or a claim that their individual loops are promoted.

### Boundary-census checkpoint (2026-08-26)

`rfc0153_full_deoptimization_matrix_preserves_results_and_traps` now executes
the remaining numeric boundary shapes under both `OptSet::default_set()` and
`OptSet::none()`: an empty range, a one-trip maximum-Int no-wrap loop, a
maximum-Int wrapping loop stopped by an explicit break, and a false-entry
zero-step loop. The existing `zero_step_while_declines_sequence_cursorization`
WAT fixture retains the complementary non-terminating case and verifies that
no sequence cursor is installed. The focused test passed locally; queue
evidence remains pending.
