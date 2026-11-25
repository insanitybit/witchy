# Bounded proof scaling observations

Qualified optimized same-host results and limitations: [performance evidence](wasm-performance.md). The three-sample final-candidate matrix and separate twelve-pair edit comparisons have different evidence strength.

The new `scripts/measure-wasm-proof-scaling.py` is measurement preparation, not
qualification or a speed improvement. Obtain the coordinator's explicit host
reservation before invoking it. It performs no builds, installation, SDK change,
cache admission or optimization. Use already-built, immutable optimized binaries
that support `wasm-measure`, and an explicit trusted absolute Lean root.

A small first pilot can use one instrumented compiler:

```sh
python3 scripts/measure-wasm-proof-scaling.py \
  --compiler /absolute/pinned/witchyc --compiler-revision EXACT_COMMIT \
  --lean-root /absolute/trusted/lean-root \
  --out /absolute/evidence/new-pilot --cases ghost-1 ghost-8 --samples 3
```

Add `--baseline /absolute/pinned/baseline --baseline-revision EXACT_COMMIT` for
matched paired comparisons. Revision labels are operator declarations; binary
SHA256 pins are independently checked before work, before each compiler call,
and at termination. The harness revision, tracked source snapshot, Python/Node
versions, platform, fixture hash, and observed proof-tool receipt identities are
recorded. Do not treat a declared revision as independently recovered from the
binary. The evidence directory must be new and outside the checkout; it is
private, with private compiler scratch space. Importing the isolation helper
disables bytecode writes into the checkout. Python optimization flags are refused
because assertion guards must remain enabled.

Runtime code-edit cases are `ghost-1`, `ghost-8`, `ghost-32`, `chain-16`, `chain-64`, `chain-120`,
and `vec`. Ghost cases have true authored unused lemmas and an ordinary main
whose printed value changes from 1 to 2. Chains reuse the existing generic Box
baseline: an actual consumed f0 addition changes from 1 to 2. No false proof
or rejection latency is included.

For `vec`, supply `--vec-source /absolute/pinned/proof-bearing-caller.wy`.
Select the actual qualification fixture with a generic exercise<T>, native
clauses, and four U8/U64/Bool/Pair callers, not the proof-erased comparison file.
The runner pins the fixture, checks its two-goal receipt and four concrete
instances per generic definition, and expects actual spare/Full output. It edits
only the first main byte call's requested capacity from 2 to 3, requiring the
first printed capacity to change accordingly. Other capacities remain 2 and 0.
The edit must also change emitted bytes. Every row records both the original
fixture hash and exact compiled source hash. Without `--cases`, the pilot selects
ghost-1/ghost-8 and adds vec only when `--vec-source` is provided.

Each case has separate coverage and timed warmup, excluded from sample
summaries. A deterministic seed randomizes case/sample/state order and adjacent
version order. Each fresh row has absent output. Each unchanged/edit row gets its
own recorded base compilation before the timed operation; unchanged recompiles
the same source, while edit recompiles the consumed change. All are complete
checks. These names imply neither cold filesystems nor cache hits. Setup time
remains visible separately, and creates no compiler session or admission cache.

The runner reuses `scripts/harness.py` process-group execution and bounded
cancellation/reaping. It enforces 16 GiB free disk before every command and a
4 GiB live floor, signals cancellation on SIGINT/SIGTERM, and checks a whole
campaign deadline. Individual compiler commands are bounded at 90 seconds,
host commands at 10, metadata at 5; all also respect the remaining campaign
limit. The default campaign limit is 1800 seconds, configurable to 60..3600.
Samples default to 3 and are bounded to 3..24. No command starts after cancellation.
Failures retain raw logs and a terminal results file; unreaped groups remain
visible and cause failure rather than a successful resource-release claim.

Compiler and disposable production-host elapsed times are separate. These
external times include the existing isolation wrapper's launch, wait, log and
reap overhead; they are not compiler or host CPU time. Every
module executes through the existing host with explicit output/allocator grants,
checking successful status and exact consumed output. Wasm/source hashes must
match across versions for each paired sample/state. Runtime code-edit rows
assert optimized mode and zero/one Lean invocation from `wasm-measure`. Where new phase
fields exist, goal/hash counts and inner-versus-outer reconciliation are checked;
missing older instrumentation is absent, not inferred zero. Whole-child time
must not be called kernel time.

`results.json` retains raw rows, setup records, command logs, phases, pins,
terminal source/compiler/fixture checks and child cleanup. Summaries give median,
minimum and maximum compiler/host/phase times. With at least twelve samples,
a deterministic median bootstrap interval is added. Optional paired differences
and ratios describe these matched runs; they do not establish causality or
optimality. Keep raw evidence outside tracked source. Measure instrumentation
overhead against a matched uninstrumented build before optimization attribution.
Hardware counters, hash read volume, allocation counts, cold-cache behavior,
concurrency, rejection latency and physical Lean kernel-only time remain outside
this harness. No execution or baseline was performed while preparing it.

Use `--states edit` for a bounded matched edit campaign after the default pilot
has covered fresh, unchanged and edited states. State selection changes only the
measurement matrix; every selected invocation still performs its full check and
consumes the result. Setup and baseline preparation remain recorded separately.


Authored proof edits are separate explicit cases: `ghost-1-proof` changes one
claim from `x == x` to `x <= 18446744073709551615`; `vec-proof` appends the
redundant `output.length > old(input.length)` to the Vec Ok post, preserving every
old clause and the unchanged source algorithm. Main and emitted runtime bytes
must stay identical. Defaults and runtime code-edit cases remain unchanged.
Both proof edits must change the exact source hash and the claim fingerprint;
unchanged/fresh states must preserve both. A source-hash change alone is not
accepted as proof consumption.

Proof-edit timed rows use `wasm-checked` rather than `wasm-measure` to consume
the actual fresh compiler receipt. Every statement SHA256 is recomputed from
its reported expected statement; the fingerprint hashes the ordered theorem/
statement-SHA pairs. The runner checks selected coverage, expected goal count,
no proof admission/reuse, fresh kernel-checked outcomes and dependency count,
and source/compiler identity. Full receipts retain source/model/audit/generated
module/tool bindings per row. The qualified unedited coverage and each subsequent
base setup must agree with unchanged rows; edited rows must differ in claim
fingerprint while retaining byte-identical Wasm and exact runtime output. Across
matched binaries, source/emitted-Wasm/claim fingerprints must agree for each state.
Generated Lean module identities may differ with compiler import policy.

Receipt mode is explicit in `compiler_mode` and `case_mode`, with campaign
`case_modes` metadata. `compiler_phases` is null and `phase_ns` is empty for these
rows: there is no phase breakdown, kernel-only timing, or independently reported
invocation count in this API. Whole elapsed includes receipt construction/output
and isolation overhead; compare matching commands and cases, not code-mode
phase timings. A separate setup-only zero-goal `wasm-measure` check per binary
asserts optimized mode and Lean0 whenever any proof-edit case is selected.
Coverage, warmup and base setups remain excluded from timed sample summaries.

After independent review and a separately granted small pilot, a matched
proof-edit campaign can use:

```sh
python3 scripts/measure-wasm-proof-scaling.py \
  --compiler /absolute/pinned/candidate --compiler-revision CANDIDATE_COMMIT \
  --baseline /absolute/pinned/baseline --baseline-revision BASELINE_COMMIT \
  --lean-root /absolute/trusted/lean-root \
  --vec-source /absolute/pinned/proof-bearing-caller.wy \
  --out /absolute/evidence/new-proof-edit-pairs \
  --cases ghost-1-proof vec-proof --states edit --samples 12
```

Valid authored-proof edits remain full fresh checks, not admission-cache hits.
False-proof rejection latency is still outside this campaign. This harness
extension was prepared without syntax checks, compiler execution or measurement;
the new proof cases require focused validation before timing claims.
