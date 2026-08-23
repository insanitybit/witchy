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

Done when:

- tie order, alternating readiness, close, cancellation, payload ownership,
  replayed/escaping tasks, and `PullAny` fallback have interpreter/Wasm parity;
- WAT and counters show no steady-state select continuation/result-envelope
  allocations or RC churn;
- `select_fanin` meets the RFC gate (first slice at least 10x faster and no
  worse than 2x Go) and `chan_throughput` regresses no more than 5%; and
- a fresh 12-pair schema-1 artifact, correctness hashes, three identical
  counter runs, and a merged queue event are recorded in the ledger.

### 2. Direct-call and closure devirtualization

Revisit only with a mechanism that reaches the whole workload, not a scalar
leaf microcase. Keep indirect, multi-result, escaping, recursive, and effectful
calls on the safe fallback path.

Done when `closure_calls`, `expr_eval`, and `binary_trees` pass the RFC
promotion thresholds together, protected rows stay within 5%, WIR/WAT and
negative tests cover every fallback, and a fresh post-merge matrix plus durable
evidence is landed. Otherwise update the ledger as rejected and stop.

### 3. Sound scalar strength/range lowering

Only re-enable additional reductions after proving evaluation order, wrapping
`Int` arithmetic, loop-carried invalidation, trapping operands, and multi-store
destinations. Never erase an effectful operand merely because its value is
algebraically unnecessary.

Done when focused WIR/effect/trap tests and parity pass, `collatz`, `loop_sum`,
`mandelbrot`, and `expr_eval` meet their measured gate without regressions, and
the same fresh evidence/queue/ledger protocol succeeds. The already-landed
fail-closed safety repair remains valid even if no performance candidate clears
the gate.

### 4. Bounded recursive inlining

Use a symbolized profile or equivalent attribution before implementation. Add
small, budgeted recursive clones only where recursion, closure capture,
multi-result envelopes, traps, and code-size limits are proven safe; preserve
the non-inlined fallback.

Done when `fib` and protected `binary_trees`/`expr_eval` meet the RFC threshold,
WIR tests pin clone budgets and rejection cases, and fresh post-merge evidence
is durable. If the host cannot provide attribution or the candidate is neutral,
record the rejection and defer rather than claiming completion.

### 5. Residual string/hash work

Reopen only after a current function-level profile attributes at least 10% of
`knucleotide` to eligible borrowed-string hashing/equality. Optimize the typed
hash/equality path without changing byte/string semantics or dictionary
representation.

Done when the profile, deterministic hash/equality counters, parity/trap tests,
and fresh 12-pair matrix show a threshold-clearing `knucleotide` gain with
`word_count` and `dict_count` protected. Otherwise retain the current deferred
status and do not invent a benchmark-specific fast path.

### 6. RFC-0143 Swiss dictionary

Keep RFC-0143 separately tracked. Resolve its ABI/hash-width, String/Bytes
equality wording, Swiss-root metadata, rebuild encoding, promotion set,
remove-order accounting, and Phase-0 vector-index control questions before
implementation. Land it independently only with its own correctness, counter,
and benchmark evidence; do not fold it into RFC-0146 claims.

## Per-track landing protocol

1. Rebase an isolated branch onto current `master`; capture a fresh baseline
   binary, source identity, profile, and correctness outputs.
2. Implement one mechanism with no benchmark-name recognizers and add focused
   positive, negative, trap/deopt, and WAT tests.
3. Run the relevant focused checks, then a fresh 12-pair interleaved matrix
   against that exact baseline. Record medians, MAD, paired bootstrap interval,
   raw samples, output hashes, toolchain/config identity, and three identical
   mechanism-counter runs.
4. Submit through `./scripts/merge-queue.sh`; wait for the terminal event.
5. After merge, rebuild and rerun the affected benchmarks from the new
   `master`, update the acceptance ledger, and only then begin the next track.

## Completion

This goal is complete only when every remaining track is either landed with its
promotion evidence or explicitly closed as rejected/deferred with durable
evidence, RFC-0146’s ledger is truthful, the full gate is green, and the final
post-merge benchmark sweep has been run from the resulting `master`.
