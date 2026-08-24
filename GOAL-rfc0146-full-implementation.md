# Goal: complete RFC-0146

Complete the remaining promotable work in RFC-0146. Each track is an
independent change: implement it in an isolated worktree, validate it against
the immediately preceding `master`, land it through the merge queue, and run a
new benchmark matrix after it lands. A rejected experiment is not complete
work; record its evidence and leave the next viable seam explicit.

## Tracks

### 1. Allocation-free awaited `select2`

Implement the compiler-owned frame/scheduler ABI and WIR emitter for the
existing typed two-way selector. Do not add another stdlib wrapper or retain a
closure/task allocation on the hot path.

Done when tie order, alternating readiness, close, cancellation, payload
ownership, replayed/escaping tasks, and `PullAny` fallback have interpreter/Wasm
parity; WAT and counters show no steady-state select continuation/result
allocations or RC churn; `select_fanin` meets the RFC gate; `chan_throughput`
regresses no more than 5%; and a fresh 12-pair schema-1 artifact, correctness
hashes, three identical counter runs, and a merged queue event are recorded.

### 2. Direct-call and closure devirtualization

Revisit only with a mechanism that reaches the whole workload, not a scalar
leaf microcase. Keep indirect, multi-result, escaping, recursive, and effectful
calls on the safe fallback path. Done when the three target workloads pass the
RFC thresholds together, protected rows stay within 5%, and fresh durable
evidence is landed; otherwise record rejection and stop.

### 3. Sound scalar strength/range lowering

Only re-enable reductions after proving evaluation order, wrapping `Int`
arithmetic, loop invalidation, trapping operands, and multi-store destinations.
Never erase an effectful operand merely because its value is algebraically
unnecessary. Require frozen parity/trap vectors, WAT proof, target gains, and
protected-workload limits.

### 4. Bounded recursive inlining

Require a symbolized profile or equivalent attribution before implementation.
Use small budgeted recursive clones only where captures, multi-result
envelopes, traps, and code-size limits are proven safe; preserve fallback.
Require the `fib` gain and protected-workload gates, or durably defer.

### 5. Residual string/hash work

Reopen only after a current profile attributes at least 10% of `knucleotide` to
eligible borrowed hashing/equality. Preserve byte/string semantics and the
dictionary representation. Require counters, parity/trap tests, and a fresh
threshold-clearing matrix; otherwise retain deferred status.

### 6. RFC-0143 Swiss dictionary

Keep RFC-0143 separate. Resolve its hash-width ABI, equality wording, Swiss-root
metadata, rebuild encoding, promotion set, remove-order accounting, and
Phase-0 vector-index controls before implementation. Land independently only
with its own correctness, counters, and benchmark evidence.

## Per-track landing protocol

1. Rebase an isolated branch onto current `master`; capture a fresh baseline
   binary, source identity, profile, and correctness outputs.
2. Implement one mechanism with no benchmark-name recognizers. Add focused
   positive, negative, trap/deopt, parity, and WAT tests.
3. Run focused checks, then a fresh 12-pair interleaved matrix against that
   exact baseline. Preserve medians, MAD, paired bootstrap intervals, raw
   samples, output hashes, toolchain/config identity, and three identical
   mechanism-counter runs.
4. Submit through `./scripts/merge-queue.sh`, wait for terminal merge, and
   update the acceptance ledger.
5. Rebuild and benchmark from the resulting `master` before beginning the next
   track.

## Completion

The goal is complete only when every remaining track is either landed with
promotion evidence or explicitly closed as rejected/deferred with durable
evidence, the RFC ledger is truthful, the full gate is green, and a final
post-merge benchmark sweep has run from the resulting `master`.
