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
| 0 measurement integrity | merged | `/tmp/rfc0146-track0-quick-all.json` (`sha256:5a8e32f1bc12665170542503be0c23405b3b08137d23e67910172cb35bf4f85a`) | `stats::tests::deterministic_counters_repeat_exactly`; profile not applicable to harness correctness | 54-test `stats::tests` shard; five focused Python integrity tests; 17-result quick sweep | all 17 benchmark results | `fix/rfc0146-causal-integration` | merged as `dd7cb6f3c53db37d552dc6f05a4231b5cecf23b8` at `2026-08-23T06:20:49Z`; journal line 8567 and gate log `state/merge-queue/logs/20260823-021554-fix~rfc0146-causal-integration-74028-32.log` |
| 1 awaited select fusion | partial infrastructure: 1B + ready-path 1C + pure-lane proof prerequisite; allocation-free 1D open | `/tmp/rfc0146-track1-1b-20260823.tsv`; ready-path spot pair; no new timing claim for lane proof | `scalar_lane_update_sequence` proof and focused positive/negative tests in `a197e772`; no emitter/counter evidence yet | syntax fusion/deopt, WIR plan, carrier tests, semantic output/parity, and two lane-transfer proof tests passed | no performance claim for the pure-lane proof | `impl/rfc0146-track1-1d`, landed as `2ebcfe6c` (source `a197e772`) | merged through queue; Track 1 remains open pending typed payload projection/emitter |

#### Track 1D prerequisite detail

The landed proof is deliberately conservative: it accepts only local lane
writes, integer constants/locals, arithmetic, and structured control flow. It
rejects direct and indirect calls, stores, allocation-producing expressions,
host effects, and unsupported nodes, preserving the ordinary Task/Step
scheduler as fallback. It is an emitter prerequisite, not an allocation-free
select optimization and carries no performance claim.
| 2 sequence plans | merged | schema 1 artifact `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track2-f77e84f-21c6a9e1/acceptance-f77-schema1.json` (`sha256:ab3ca448d25476cbac1123d5d5a483aec77fb34d19fddfed601b8407ea9fad83`) | pre-change Samply capture plus integrated raw-WAT reconciliation; three exact counter payloads (`sha256:5152cd4a5c07e10ff204828e2b44b492b097c2bd6d1b22ba504acc8f73813744`) | sequence-plan correctness/trap/deopt/WAT tests, exact stats fixture, runtime policy tests, and differential golden output | shipping: `fannkuch` +21.677796%, `list_index` +37.426176%, `list_sum` +5.651316%, `binary_trees` +0.464728% | `impl/rfc0146-track2`; implementation `21c6a9e1`, measured source `9a7b607d` on baseline `f77e84f` | merged as `c372d53956035188622e52ff7f8d6343594c6f8b` at `2026-08-23T09:20:15Z`; journal line 8580 and gate log `state/merge-queue/logs/20260823-051537-impl~rfc0146-track2-74028-37.log` |
| 3 WIR inlining | rejected: scalar-leaf candidate removes a direct closure call but misses the whole-workload gate | `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track3-next-5e120cec/track3exact-closure_calls.tsv` (`sha256:5b6ef21d3a1e3f78d19b7e9a9c52ab16ff4103ce329d460baa3288cff228b3a3`), plus exact `expr_eval` and `binary_trees` pairs; baseline binary `sha256:5182e3aa46a4a9672aa721f43f56812cbc60efdfce6422af58cb6a46a39a9349`; candidate `sha256:f615181d838740b63d57576f7166c13ebd7334c503905187064c96ae9d8602df` | `witchy-wir` native scalar-leaf positive/negative WAT and multi-result envelope tests: 2 passed; candidate WAT has no hot `call $__lamt0`, negative envelope retains the call | Fresh 12-pair medians: `closure_calls` 3.725 to 3.713 ms (+0.33%), `expr_eval` 11.151 to 11.241 ms (-0.81%), `binary_trees` 53.022 to 52.660 ms (+0.68%). RFC requires closure_calls >=20% and protected rows <=5%; no promotion claim | detached current-master experiment; implementation not queued | no terminal queue event; do not queue without a reproducible whole-workload gain |
| 4 strength reduction | performance rejected; safety repair merged separately | `/tmp/rfc0146-track4-{master,candidate}-*.tsv` (12 paired samples; raw hashes in the safety note below) | prior pass removed from default pipeline; no promotable profile attribution | `witchy-wir` 42 tests; workspace fast gate 3051 passed; Wasm shard green | Collatz gain 0.4%; controls: `loop_sum` +5.9%, `mandelbrot` +1.0%, `expr_eval` +0.4% (candidate/master) | `perf/rfc0146-strength-reduce` at `992a77af` | merged as `c09dd6d5a704cdd40858cf0b0a6589e7cc7a51da` at `2026-08-23T14:16:47Z`; journal line recorded under `mq-557e507b21e2fdf26b02a0fe0ac3c664b1e6a980`; gate log `state/merge-queue/logs/20260823-101617-perf~rfc0146-strength-reduce-74028-39.log` |
| 5 recursive inlining | deferred: entry criterion remains unproven | durable current-master profile `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track5-fib-profile-75737cb6/fib-profile-96.samply.json.gz` (`sha256:9e5107108a4d65d6a1769459a35490def4e1404479b9224965ad9ba5844f35a9`); generated WAT `fib-master.wat` (`sha256:07cf61b7089f1aca5d906a1e070146b537b2acafa3bd0818790203eb1f9c2a52`) | 6,070 main-thread samples over 7.15 s for `fib(35)` x96 at 1 kHz; profile remains unsymbolicated (`native_symbols=0`); WAT remains a minimal scalar body with two recursive calls and no adapters/allocations; no attributable call/return percentage | RFC requires native attribution of >=10% call/return overhead before implementation; source-level unrolled control is noisy/sub-threshold as described below; no promotion evidence | `perf/rfc0146-recursive-inline`; no implementation changes | no terminal queue event; keep deferred until a symbolized/attributable profile is available |
| 6 packed Bool kernels | merged | `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track6-2b065c24/track6-schema1.json` (`sha256:854d6528fc0a7e05622ab7cb6118b9eac1700f48d8341b5b0f69cfceafbf2361`) | candidate WAT `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track6-2b065c24/candidate.wat` (`sha256:05fb7d37b0735d53494a41f7a8c9dd48431407d6209b237907a9d4c7fffa77c2`); exact packed Bool cursor read/store shape and no scalar-set helper call in promoted loop | `witchy-lower` host-layout shard (11 passed), `cargo check -p witchy-lower`, parity/result checks, trap/deopt fallback retained | `nsieve` 4.814 to 3.845 ms (20.08% faster); `list_sum` 8.798 to 8.882 ms (0.95% regression); `list_index` 3.210 to 2.639 ms (17.86% faster) | `perf/rfc0146-packed-bool` @ `2b065c24` | merged as `0d3e0589ec6b16f0351de7fa0b773683bfa20573` at `2026-08-23T14:22:07Z`; journal line 8592 and gate log `state/merge-queue/logs/20260823-102106-perf~rfc0146-packed-bool-74028-41.log` |

### Track 1 rejection detail

The only implementation on this branch, `5f9d6f09`, is a helper-only routing
slice and is deliberately not accepted as RFC-0146 Track 1. It rewrites an
immediately awaited `chan.select(a, b)` through `chan.__select2_map`, but the
generated WAT still constructs the outer continuation, a helper closure/task,
and the `Pull2` task. The decode path still calls
`task.and_then__chan_Selected...` (10 calls in `select_fanin`) and selection
scaffolding still reaches `rc_alloc`. The generated carrier plan records an
`AwaitSelect2Plan`, but `scalar_executor::synthesize` has no `ChannelSelect2`
emitter.

The required 1C/1D prerequisite is a coordinated internal ABI, not another
stdlib wrapper:

```text
Select2Resume { state: Ready | Closed, ready_arm: i32,
                payload: exact erased-message lane,
                continuation: owned compiler state/frame }
```

Today `Step::Pull2` and `Slot::Wait2` own a closure returning `Task(a)`, and
`task.run` resumes by allocating `Active(cont(tag, payload))`. A scalar result
without a compiler-owned frame ID cannot resume across `Wait2`, close, or
cancellation; retaining the closure leaves the target allocations in place.
The next attempt must change the scheduler/frame representation and WIR
`ChannelSelect2` emitter together, then add tie, close, cancellation, payload
ownership, and escaping/replayed-task parity tests.

The matched evidence is sub-threshold: `select_fanin` improved from 22.801 to
20.110 ms (`0.882x`, 11.8% faster than the immutable master control), while
`chan_throughput` was 125.271 vs 124.203 ms wall-only (`1.009x`). This is far
from the RFC's 10x first-slice and <=2x-Go acceptance criteria, so the branch
has no terminal queue event and must not be promoted or queued as accepted.

### Track 6 acceptance detail

| Requirement | Evidence | State |
|---|---|---|
| One-byte packed representation retained | candidate WAT contains `list_repeat_bool`, `i32.load8_u`, and `i32.store8`; no layout/API change | accepted |
| Exact sequence cursor consumption | candidate WAT's `nsieve.nsieve` uses `__seq_cursor_*` for the Bool read and write and has no `call $__witchy_packed_scalar_set_*` in the promoted loop | accepted |
| Ownership and deoptimization safety | clean exact-proof sites emit direct `store8`; dirty, non-Bool, opaque, and unproven sites retain the existing packed scalar helper and copy path | accepted |
| Correctness/parity/traps | `host_layout_tests` 11 passed; candidate outputs match Go for `nsieve`, `list_sum`, and `list_index`; the exact path is gated by `sequence_element_address`, so non-proven accesses retain checked lowering | locally verified |
| Matched promotion matrix | durable schema-1 artifact and paired raw TSVs at `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track6-2b065c24`; candidate source `2b065c24`, predecessor source `99c0045f`, binary hashes recorded | accepted; merged as `0d3e0589ec6b16f0351de7fa0b773683bfa20573` |
| 7 borrowed string/hash residual | deferred: entry criterion not met; schema 1 audit `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track7-audit-d86b5ff5/track7-schema1.json` (`sha256:58b60fdaa0d23f33c8dd4f0d4ce88c96aff6773d0ce32d560a47bdd624e7f35d`) | matched 12-sample baseline and deterministic hash/probe counters; no symbolized function-level profile attribution >=10% of `knucleotide` to an eligible residual | phase0 harness + temporary stats counter activation; Samply Wasmtime JIT profile retained but unsymbolicated; no implementation proposed | baseline: `dict_count` 25.288 ms, `word_count` 22.701 ms, `knucleotide` 17.543 ms median | audit/rfc0146-track7 | not applicable: deferred before implementation; RFC-0143 remains proposed |

## Track 4 safety repair (not performance acceptance)

The original `22e1a33c` range reducer is not accepted as a performance track.
Its default-pipeline algebraic folds could erase effectful calls, trapping
loads, and other observable evaluation; its dataflow also did not model
wrapping arithmetic or loop-carried assignments. The current-master repair is
`992a77af` on `perf/rfc0146-strength-reduce`: it removes the experimental pass
from the default optimizer and adds a regression fixture proving that
effectful and trapping operands remain present. This is a correctness repair,
not an optimization claim, and the Track 4 performance row remains rejected.
The repair landed as `c09dd6d5a704cdd40858cf0b0a6589e7cc7a51da` through merge
queue change `mq-557e507b21e2fdf26b02a0fe0ac3c664b1e6a980`; its terminal merged
event occurred at `2026-08-23T14:16:47Z` with gate log
`state/merge-queue/logs/20260823-101617-perf~rfc0146-strength-reduce-74028-39.log`.

The repair was checked with all 42 `witchy-wir` library tests, the workspace
fast gate (3051 tests passed), and the Wasm shard. The matched 12-sample raw
comparison used master binary
`8a1f303f8e65641ea645dc2c96a8107e7644795155d82e81b09bcf94b39aac17` and
candidate binary
`8d4a1588e67c7775d542da1aaf423b0e1915e67f8d9850b711523b473536659d`.
Raw samples remain at
`/tmp/rfc0146-track4-{master,candidate}-{collatz,loop_sum,mandelbrot,expr_eval,fib,binary_trees}.tsv`.
The candidate/master medians were Collatz 194.832/195.573 ms, loop_sum
26.594/25.117 ms, mandelbrot 35.119/34.785 ms, and expr_eval 11.318/11.274
ms, so the RFC's 15% Collatz threshold was not met.

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
| Merge queue | terminal journal event at line 8567; gate log `state/merge-queue/logs/20260823-021554-fix~rfc0146-causal-integration-74028-32.log` records the complete seven-stage gate | merged as `dd7cb6f3c53db37d552dc6f05a4231b5cecf23b8` at `2026-08-23T06:20:49Z` |

## Track 2 acceptance detail

| Requirement | Evidence | State |
|---|---|---|
| Stable sequence plan contract | plans carry root, payload base, length, data offset, stride, element kind, proven domains, cursors, and conservative invalidation; nested plans reuse only matching initialized ancestor metadata | locally verified |
| Header and length elimination | parent stable-root metadata reuse and in-plan `list.length` consumption reduce the final `fannkuch` dynamic `list_header_loads` count from 45,822,974 to 3 | locally verified |
| Bounds-check coalescing | exact affine domains and structural counted-builder provenance coalesce 9,864,090 checks in `fannkuch`; reassigned/opaque bounds, disabled bounds elision, shadowing, overflow, and root mutation remain guarded | locally verified |
| Pointer cursorization | final `fannkuch` evidence records 338,344,833 cursorized indexed accesses; WAT retains exact traps for negative, out-of-range, and affine-overflow cases | locally verified |
| Safe load/store forwarding | same-root and same-affine-address forwarding records 6,235,300 dynamic loads; calls, distinct roots/offsets, conditional stores, merges, and invalidated plans decline | locally verified |
| Small constant-trip unrolling | cost-budgeted existing unroll is consumed only by an active sequence plan; the protected `fannkuch` fixture correctly records zero dynamic unrolls | locally verified |
| Backend policy | versioned `witchy.optimizer-policy` is set transactionally only when a plan is consumed; runtime parsing is fail-closed and cache identity includes the policy | locally verified |
| Production instrumentation isolation | production code emits neither counter globals nor placeholder `const/drop` WIR; stats mode batches deterministic loop-local counts | locally verified |
| Counter repeatability | three byte-identical deterministic `fannkuch` payloads each have `sha256:5152cd4a5c07e10ff204828e2b44b492b097c2bd6d1b22ba504acc8f73813744` | accepted |
| Promotion sampling | two warmups and twelve paired, interleaved samples for shipping and raw modes; schema 1 identity includes both source revisions, binary hashes, configurations, samples, median, range, MAD, deterministic 10,000-resample bootstrap interval, and correctness hashes | accepted |
| Shipping performance | integrated medians: `fannkuch` 180.700 to 141.529 ms (+21.677796%); `list_index` 4.540 to 2.841 ms (+37.426176%); `list_sum` 8.309 to 7.839 ms (+5.651316%); `binary_trees` 52.006 to 51.765 ms (+0.464728%) | accepted |
| External retention | `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track2-f77e84f-21c6a9e1`; bundle `SHA256SUMS` file hash `fdc6301c74b1224323fb9cac4f3d3f43ee730e891e78440abe718e2b7edd2033` | accepted |
| Merge queue | terminal journal event at line 8580; gate log `state/merge-queue/logs/20260823-051537-impl~rfc0146-track2-74028-37.log` records the complete seven-stage gate | merged as `c372d53956035188622e52ff7f8d6343594c6f8b` at `2026-08-23T09:20:15Z` |

## External artifact retention

Before closing a performance track, copy its raw local artifact bundle to a
durable path outside the repository and replace `pending` in its row with that
path. The ledger records summaries and hashes, not machine-specific raw data.

### Track 7 deferred audit

The Track 7 entry audit ran on `d86b5ff5` with the release binary identified in
the row above. Twelve phase-0 samples (two warmups) had medians of 25.288 ms
for `dict_count`, 22.701 ms for `word_count`, and 17.543 ms for `knucleotide`.
With the deterministic Swiss counters enabled for the audit-only stats build,
the workloads performed respectively 6,003,254, 1,003,254, and 870,487 hash
operations. Probe-group counts were 3,786,429, 1,314, and 231,797; H2
candidate counts were 2,999,001, 1, and 483. These counts establish workload
shape but do not establish kernel-time attribution. The available Samply
capture was unsymbolicated for Wasmtime JIT frames, so it cannot prove that an
eligible residual consumes at least 10% of the `knucleotide` kernel; the
attribution requirement therefore remains unmet rather than being inferred
from counter volume. The existing borrowed-view path already avoids owned-key
materialization on hits, and the audit found no sound cache or length-hoisting
seam. Track 7 is therefore deferred without an implementation or queue event.
The raw samples and counter outputs are retained under the external artifact
directory recorded in the Track 7 row.

This audit does not accept RFC-0143. The current master contains Swiss control
metadata, SIMD H2 probing, ordered projection routing, and borrowed String
lookups from earlier implementation work, but RFC-0143 still lacks its
requirement-level promotion evidence: the complete size/hit/miss/churn/key-mode
matrix, scalar/SIMD and interpreter differential rows, ownership and retained
heap measurements, ordered-projection limits, bytes-per-entry comparison,
portable SIMD/scalar release artifacts, and the matched whole-workload
geomean/no-regression gate. Existing code and counters are implementation
evidence only; the RFC header remains `proposed` until that separate acceptance
ledger is completed.

## Track 5A profile detail

The stronger Track 5A run used the current master source `75737cb66871b1205511670b7787ea7b779678d4` and release binary `target-track5/release/witchy` (`sha256:f301816d26cb0729e9f88767b142430de7b1ae7dac94ec77d7c5bfbb03f41250`). A temporary fixture executed `fib(35)` 96 times inside one monotonic-clock region, producing the correct checksum `885836640` and a measured kernel of `6,949,464,542 ns`. Samply ran at 1 kHz and retained 6,070 main-thread samples over 7,145 ms. The capture is durable at `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track5-fib-profile-75737cb6/fib-profile-96.samply.json.gz`; it reports `symbolicated=false` and zero native symbols because Wasmtime JIT frames remain raw addresses. The longer run proves that the workload is large enough for sampling, but it still cannot attribute a percentage specifically to recursive call/return overhead. The RFC entry criterion therefore remains unproven, not accepted by inference.

As a generated-code control, a temporary source-level one-level-unrolled fixture was paired six times with the same repeated workload. Raw pairs are retained at `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track5-fib-profile-75737cb6/fib-unrolled-control.tsv` (`sha256:30ff7281df6de91cc112923c700a1172413b3aa9270ea97e04e72d00e2a2c0b9`). The samples were highly unstable: baseline values ranged from `4,823,019,333` to `9,216,173,334 ns`, while the unrolled control ranged from `3,119,567,667` to `35,353,235,417 ns`; the paired medians did not establish a reproducible >=10% gain. This is rejected experimental evidence only: it is source rewriting rather than a compiler implementation and does not satisfy Track 5 acceptance. The profile, WAT, and control artifacts are retained for a future symbolized re-profile; no Track 5 code or queue event exists.

## Track 3/5 fresh scalar-recursion investigation

On current master `5e120cec`, the existing `recursive_inline` module was found to be a no-op for `fib`: its single-expression extractor did not admit the value-producing `If`, and its `target.locals.is_empty()` gate rejected the lowerer's predeclared scratch-local catalog. A detached scalar-leaf experiment retained strict no-loop/no-host/no-reference filters, admitted only bodies with no non-parameter local reads, and added traversal for the actual `CallStoreMulti` enclosing shape. Focused native `witchy-wir` checks passed (129 existing tests plus 2 new positive/negative WAT tests), and the candidate WAT proves the hot direct closure call was removed while the multi-result negative envelope retains its call.

The exact current-master twelve-pair comparison is retained under `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track3-next-5e120cec/`. Baseline is a release build from `5e120cec` (`sha256:5182e3aa46a4a9672aa721f43f56812cbc60efdfce6422af58cb6a46a39a9349`); candidate is the same source plus the detached patch (`sha256:f615181d838740b63d57576f7166c13ebd7334c503905187064c96ae9d8602df`). Medians were `closure_calls` 3.725 to 3.713 ms (+0.33%), `expr_eval` 11.151 to 11.241 ms (-0.81%), and `binary_trees` 53.022 to 52.660 ms (+0.68%). This is far below Track 3's required >=20% closure gain, so the implementation is rejected and not queued; the WAT proof is mechanism evidence only.

The eight-pair current-master comparison is retained at `/Users/cobrien/.local/share/witchy/evidence/rfc0146/track35-fresh-7864f03e/fib-one-level-paired.tsv` (`sha256:0e9df891f8fc5375a6277e1d231a3b5470c0212e29da0003bcf165f7b329dc03`). Baseline medians were approximately 31.0 ms and one-level candidate medians approximately 29.2 ms, a stable ~5.9% gain, below Track 5's >=10% acceptance threshold. A separate depth-two control reached approximately 20.7 ms in four exploratory samples but expanded `fib` WAT from 674 to 818 lines (~21.4%), exceeding the RFC's bounded code-size budget and was not paired promotion evidence; its raw samples are retained at `fib-depth2-control.tsv` (`sha256:1a3a57c19573342619d408f7350c3d4365d1ed0453273a59da5d01e9305238b`). The source patch is preserved outside the repository at `/tmp/rfc0146-track35-one-level.patch` (`sha256:d58571155b42737e8b4ba3331f03d29080f511fb9cb52fa858cdbcd8df16c6e2`). The one-level seam therefore remains a useful future candidate but is not queueable as accepted Track 5; depth two is rejected on code-size/evidence grounds.

## Track 0 local verification

- `python3 -m unittest benchmarks/test_summarize.py -v`: 5 passed.
- `CARGO_TARGET_DIR=target cargo nextest run -p witchy 'stats::tests' --no-fail-fast`:
  54 passed, including the exact arena control and three-run counter equality.
- `taskpolicy -c utility ./bench.sh --quick --json
  /tmp/rfc0146-track0-quick-all.json`: 17/17 result matches.
- `python3` artifact invariant check: complete suite, seventeen records, matching
  output hashes, one positive sample per kernel benchmark, and the declared
  wall-only exception.

## Track 2 local verification

- `state/agents/rfc0146-track2/acceptance-f77-schema1.json` is the source
  identity artifact for baseline `f77e84f87fb6af4f615453b7ce88ae7269ffb9d3`
  and measured candidate `9a7b607d308acf47878a0be7310f140153062b45`;
  its retained external copy has
  `sha256:ab3ca448d25476cbac1123d5d5a483aec77fb34d19fddfed601b8407ea9fad83`.
- `state/agents/rfc0146-track2/acceptance-f77-shipping.tsv` has
  `sha256:3c4d64c2eb406683d5a4d61dab59f6e2103029b4d4ccfe80df4ac45039230957`;
  the raw control has
  `sha256:0d42b83076654c48da4bfa40fa45865902ddf09ac5b9a3e288e0f134ab957679`.
- Integrated raw WAT has
  `sha256:c244c0895b4eca3a739f867244426ff3e09dae80bddcb61ffa22dd45a713709f`.
- The retained pre-change Samply capture has unsymbolized Wasmtime JIT PCs;
  generated-WAT reconciliation and exact dynamic counters provide the
  mechanism-level attribution.
- Three exact counter repetitions agree on `list_header_loads=3`,
  `checked_indexed_loads=6235300`, `checked_indexed_stores=15077101`,
  `cursorized_indexed_accesses=338344833`,
  `sequence_bounds_checks_coalesced=9864090`,
  `sequence_forwarded_loads=6235300`, and
  `sequence_small_loops_unrolled=0`.
