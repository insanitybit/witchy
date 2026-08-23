---
rfc: 0146
title: "Post-Hybrid Performance Program: Async-State Fusion, Indexed Kernels, Call Optimization, and Range-Driven Scalar Lowering"
status: proposed
created: 2026-08-22
superseded-by:
tracking: "Defines the measured, agent-decomposable optimization program after RFC-0143 and RFC-0144. Raw benchmark and profiler artifacts remain outside commits; promotion evidence is summarized in an acceptance ledger when work begins."
---

# RFC-0146: Post-Hybrid Performance Program

## Summary

RFC-0143 and RFC-0144 removed the dominant dictionary, string-view, and
boolean-list representation costs. The remaining benchmark gaps are no longer
one collection catastrophe. They are several distinct costs: allocation-heavy
async state transitions around the existing fixed-arity selector, repeated
indexed sequence setup, direct and indirect call overhead, conservative scalar
arithmetic lowering, and a smaller residual in borrowed-string hashing.

This RFC defines seven independently ownable tracks:

0. reproducible benchmark and deterministic-counter evidence;
1. allocation-free fusion around the existing typed two-way selector;
2. indexed sequence kernel lowering with hoisted headers and pointer cursors;
3. conservative direct-call inlining and closure-call devirtualization;
4. range-driven simplification of the already-landed integer strength reductions;
5. bounded recursive inlining for small scalar functions;
6. packed-boolean strided-loop kernels; and
7. a conditional borrowed-string hash/equality residual track.

Tracks 1 through 5 can begin in parallel after Track 0 freezes the measurement
contract. Track 6 starts after Track 2 freezes `SequenceAccessPlan`. Track 7
starts only if profiling still attributes at least 10% of
`knucleotide` to hashing or borrowed-slice comparison after the other tracks.
The merge queue serializes landing, not implementation.

## Motivation

### Current measured state

The motivating snapshot is local `master` at `3403f4ce`, measured on an Apple
Silicon Darwin host with a freshly rebuilt release compiler, Wasmtime/Cranelift
at `OptLevel::Speed`, SIMD enabled, one warmup, and three kernel-clock samples.
The raw output is stored outside the repository at
`/tmp/rfc0144-bench-current.txt`.

| Benchmark | Witchy | Go | Witchy / Go | Classification |
|---|---:|---:|---:|---|
| `select_fanin` | 31.0 ms | 0.1 ms | 335.67x | catastrophic async overhead |
| `closure_calls` | 3.7 ms | 2.1 ms | 1.74x | indirect call overhead |
| `list_index` | 6.6 ms | 4.4 ms | 1.51x | indexed sequence overhead |
| `fannkuch` | 203.2 ms | 136.1 ms | 1.49x | repeated indexed mutation |
| `nsieve` | 4.5 ms | 3.1 ms | 1.47x | packed-boolean traversal |
| `collatz` | 180.9 ms | 126.0 ms | 1.44x | scalar arithmetic and calls |
| `fib` | 31.2 ms | 22.1 ms | 1.41x | non-tail recursive calls |
| `knucleotide` | 16.0 ms | 12.8 ms | 1.25x | small residual text/hash gap |
| `loop_sum` | 23.0 ms | 22.7 ms | 1.01x | parity control |
| `mandelbrot` | 33.0 ms | 31.3 ms | 1.06x | floating-point control |

The remaining non-async absolute gaps are concentrated in `fannkuch` and
`collatz`, not in dictionaries or interpolation. `word_count` is now 0.45x of
Go and `dict_count` is 0.65x. Reopening their representations without new
profile evidence would spend complexity where the measured gap has already
closed.

### Why RFC-0145 is not enough

RFC-0145 correctly identified five broad areas, but its motivating numbers
predate the completed Swiss dictionary and borrowed-string work. Its sequence,
text, and dictionary tracks have substantially shipped, while its acceptance
criterion still describes the old landscape. This RFC does not rewrite that
historical proposal. It replaces its unimplemented performance tail with
current measurements, narrow causal hypotheses, explicit shared interfaces,
and per-track promotion gates.

### What goes wrong without this RFC

Agents can independently produce plausible micro-optimizations that overlap in
`codegen/mod.rs`, change several optimization levers at once, or benchmark a
stale release binary. Those branches are difficult to integrate and their
claimed wins cannot be attributed. The resulting queue may be full of changes
that are individually green but collectively regress protected workloads.

The program therefore treats evidence, ownership boundaries, and integration
contracts as part of the optimization design rather than as closeout chores.

## Goals

1. Reduce every remaining non-async kernel benchmark to parity with Go on the
   reference host, without regressing workloads Witchy already wins.
2. Remove the residual allocation and dynamic-dispatch structure surrounding
   steady-state two-way channel selection while preserving deterministic
   scheduling semantics.
3. Make each optimization independently measurable, revertible, and landable.
4. Preserve normal-mode value semantics, `mode opt` ownership guarantees,
   traps, deterministic channel order, and interpreter/Wasm convergence.
5. Produce reusable compiler mechanisms, never benchmark-name recognizers or
   new method-specific ownership fast paths.

## Non-goals

- A new `Byte` scalar type. `Bytes` and `List(Bool)` already own the byte-dense
  contracts needed by these workloads.
- A tracing garbage collector, shared mutable memory, or nondeterministic task
  scheduling.
- Optimizing the interpreter for parity with compiled Wasm. Pure codegen
  optimizations belong only in the compiled path.
- Replacing Wasmtime or Cranelift before profiles show a backend ceiling.
- Restoring insertion-order bookkeeping to dictionaries.
- Changing benchmark algorithms, input sizes, result checksums, or measured
  regions to obtain a better ratio.
- Adding new `*_cap` helpers or new per-method `self_*` recognizers. Ownership
  and in-place eligibility remain general typed facts.

## Shared measurement contract

### Build identity

Every timing artifact records:

- source revision and dirty status;
- exact Witchy binary path and modification time;
- `rustc`, Cargo, Go, Wasmtime, OS, and architecture versions;
- optimization environment variables and Wasmtime optimization level;
- benchmark input checksum and result output;
- warmup count, sample count, and raw per-sample kernel times.

The harness must reject or rebuild a release binary older than compiler,
standard-library, manifest, or build-script inputs. A correctness match against
Go is necessary but does not prove the current compiler was measured.

### Timing evidence

Exploration may use the three-sample fast sweep. Promotion requires twelve
interleaved Witchy/Go samples after two warmups. The acceptance ledger records
median, minimum, maximum, median absolute deviation, and a paired bootstrap 95%
confidence interval for the median Witchy/Go ratio. Comparisons use the same
host, power state, toolchain, benchmark source, and compiler revision.

Kernel time is the decision metric for compiler optimizations. Wall time is
reported separately for startup-sensitive work and must not be mixed into the
kernel ratio.

### Deterministic evidence

Each track adds counters for the mechanism it claims to remove or introduce.
At least three repeated counter runs must agree exactly. Relevant counters
include:

- heap allocation calls and bytes;
- RC duplicate/drop/reuse operations;
- list header loads, checked indexed loads/stores, and cursorized accesses;
- direct, indirect, devirtualized, and inlined calls;
- integer divisions, remainders, shifts, and masked operations;
- channel select polls, waiter registrations, continuation allocations, and
  result-envelope allocations;
- borrowed-string hash bytes, hash invocations, and equality bytes.

Counter thresholds account explicitly for the fixed 8 KiB RFC-0144 format
arena. A VM high-water byte count that includes the arena is not evidence of
per-iteration growth.

### Profiling evidence

Before production code changes, every track obtains one function-level profile
using `samply` or `xctrace` on the optimized release workload and reconciles it
with deterministic counters and generated WAT. A track is rejected or revised
if its proposed target does not account for at least 10% of the workload or
cannot plausibly recover the requested whole-workload improvement.

Raw timing and profile artifacts stay outside source control. The repository
contains the harness, counter definitions, tests, and a concise acceptance
ledger, not machine-specific profile bundles.

## Design

## Track 0: Measurement integrity and acceptance ledger

### Objective

Make every subsequent performance claim reproducible and repair the acceptance
tests whose absolute heap thresholds were invalidated by the fixed format
arena.

### Work

1. Teach `bench.sh` to detect a missing or stale release compiler. With the
   default binary path it rebuilds; with an explicitly supplied `WITCHY` path
   it rejects staleness instead of building a different path and measuring the
   old executable.
2. Add an artifact header containing the build identity fields above.
3. Add a machine-readable fast/full output alongside the human table.
4. Create `rfcs/0146-acceptance-ledger.md` when implementation begins. Each row
   names its baseline artifact, profile, counters, focused tests, protected
   workloads, branch, and terminal queue event.
5. Re-express fixed-memory tests as `fixed reservation + bounded workload`
   assertions. Do not merely raise all byte limits. Pin the fixed component and
   separately prove that increasing iteration count does not increase retained
   or high-water bytes beyond a small constant tolerance.
6. Add the current benchmark source digests and output checksums to the harness.

### Ownership

- Primary files: `bench.sh`, `benchmarks/run.sh`, `benchmarks/summarize.py`,
  `src/stats.rs`, and the new acceptance ledger.
- This track owns no compiler optimization pass.

### Acceptance

- Touching a compiler input newer than the release binary causes a rebuild or
  a loud rejection before timing.
- Two workload sizes for each fixed-memory fixture prove constant bounded
  growth while including the 8 KiB arena exactly once.
- Three repeated deterministic-counter runs are identical.
- The complete fast benchmark remains 17/17 result-correct.

## Track 1: Fusion around the existing typed two-way selector

### Objective

Eliminate the residual Task, Step, continuation, and selected-result allocation
around two-way `select` while preserving the source-level `chan.select`
contract and its deterministic first-arm tie rule.

### Landed baseline

RFC-0145 already changed `chan.select(a, b)` to the private typed
`task.__channel_select2` bridge. That bridge constructs `Pull2(ia, ib, decode)`;
the executor has dedicated `Pull2` and `Wait2` cases; and the continuation
receives `(tag, Option(message))` without constructing an intermediate channel
ID list or `(index, message)` pair. RFC-0146 must retain that path and measure
it as the control row.

The remaining source-level chain is still substantial:

```text
chan.select -> Task(thunk producing Pull2)
            -> executor Step match
            -> decode continuation -> Task(Done(Selected))
            -> await/and_then continuation
            -> immediate match of First/Second/Closed
```

The Track 0 counters must attribute allocations and indirect calls to each edge
before Track 1 chooses a rewrite. A branch that merely adds another `select2`
helper has not addressed the measured residual.

### Shared interface

The syntax and type checker continue to expose one typed select operation. The
existing intrinsic catalog identity for `__channel_select2`, not a function-name
string test, lets async lowering describe an immediately awaited two-way select:

```text
AwaitSelect2Plan {
    first_channel,
    second_channel,
    payload_layout,
    resume_target,
    result_use,
}
```

`result_use` distinguishes an immediate exhaustive match from an escaping
`Selected(T)` value. Selection is based on the typed intrinsic and async resume
edge. Calls through aliases, stored tasks, replayed tasks, and other shapes fall
back to the landed source implementation.

The fused internal resume result is a multi-value triple:

```text
(state: i32, ready_arm: i32, payload: exact payload kind)
```

`state` distinguishes ready from closed. No `Task`, `Step`, `First`, `Second`,
or `Closed` object crosses this internal boundary when the result is consumed by
the immediate match. Source-level objects are materialized exactly as today if
the task or selected result escapes.

### Algorithm

1. Preserve the landed `Pull2` probe order: first channel, then second channel.
2. If an arm is ready, dequeue exactly one value and resume with its arm index
   and exact-layout payload.
3. If neither is ready, preserve one `Wait2(ch0, ch1, continuation)` scheduling
   slot and suspend once. Do not register per-channel continuation objects.
4. Fuse the immediately awaited producer and consumer so the continuation
   target and exact result locals survive across the suspension boundary.
5. On resume, repeat the same first-arm tie rule. Closing and cancellation use
   the existing source semantics.

### Work decomposition

- Track 1A: allocation/call attribution and WAT census for every edge in the
  landed chain.
- Track 1B: `AwaitSelect2Plan` production in async lowering plus rejection tests
  for escaping/replayed tasks.
- Track 1C: suspension-safe scalar resume locals and exact-layout multi-value
  result in `witchy-wir`.
- Track 1D: immediate-match scalar replacement for `Selected` variants and the
  redundant `Done`/`Yield` transition.

These cuts share the `AwaitSelect2Plan` and WIR result contract. Track 1A lands
first; 1B and 1C may then proceed in parallel; 1D depends on 1B.

### Safety invariants

- No message is duplicated, dropped, or dequeued from more than one arm.
- The first source arm wins a tie exactly as today; this RFC does not introduce
  rotation.
- Cancellation removes or invalidates one select waiter without leaving stale
  channel-local continuation pointers.
- Payload ownership and capacity tokens follow the same typed call envelope as
  ordinary receive.
- The interpreter remains unchanged except for shared semantic fixtures.

### Acceptance

- Steady-state immediately awaited `select2` executes with zero heap allocations
  and zero RC operations attributable to selection scaffolding. Semantic
  payload ownership transfers are counted separately.
- Fixtures cover both-ready ties, alternating readiness, close, cancellation,
  payload records, payload references, escaping/replayed tasks, and the existing
  general `PullAny` path remaining unchanged.
- `select_fanin` improves by at least 10x in its first promoted slice and reaches
  at most 2.0x of Go before this track closes.
- `chan_throughput` does not regress by more than 5% in matched medians.

## Track 2: Indexed sequence kernel lowering

### Objective

Turn repeated checked indexing over one stable list into a loop-local base,
length, stride, and pointer cursor while preserving the exact out-of-bounds trap
at every unproven access.

### Landed baseline

RFC-0145 already emits inline bounds checks and direct machine loads/stores for
ordinary list indexing, and its narrow counted-loop proof can elide a check for
a registered `(index, list)` pair. The residual is repeated header loading,
address multiplication, duplicated checks across related accesses, and the
limited lifetime of that expression-local proof. Track 2 extends the existing
typed path; it does not reintroduce another `list.at` specialization.

### General mechanism

For a loop with a stable list root and monomorphic element layout, the lowerer
materializes a `SequenceAccessPlan`:

```text
SequenceAccessPlan {
    owner_root,
    payload_base,
    length,
    stride,
    element_kind,
    mutation_preserves_length,
    proven_index_domain,
}
```

The plan is valid only while no statement can replace the root, change its
length, or call opaque code that may write it back. `list.set_at` preserves
length and may remain inside the plan. `push`, `pop`, replacement assignment,
unknown `var` calls, and escaping aliases terminate it.

### Distinct optimizations

1. **Header-load hoisting:** load list length and payload base once per stable
   loop rather than once per access.
2. **Bounds-check coalescing:** one proof may cover multiple accesses with the
   same index or a statically bounded offset range. Unproven accesses retain
   their individual trap.
3. **Pointer cursorization:** induction-variable loops advance a typed pointer by
   stride instead of multiplying index by stride on every access.
4. **Paired load/store forwarding:** in a swap or read-modify-write sequence,
   retain loaded scalar values in locals and avoid reloading unchanged lanes.
5. **Small constant-trip unrolling:** unroll only when the trip count and body
   cost satisfy a WIR cost budget; preserve a scalar remainder.
6. **Length-call elimination:** typed `list.length(xs)` inside a valid plan reads
   the plan local, not the heap header.

### Ownership

- New module: `crates/witchy-lower/src/codegen/indexed_kernels.rs`.
- Narrow integration points: loop setup/teardown in `block_lower.rs`, typed
  element layouts from existing layout machinery, and WIR peephole cleanup.
- This track does not own list allocation, growth, RC, or stdlib APIs.

### Acceptance

- Generated WAT tests pin one header/base load per stable loop, direct typed
  loads/stores, and absence of `list_at`/`list_set` helper calls inside the
  optimized body.
- Negative tests cover root replacement, length-changing operations, opaque
  `var` calls, negative indices, and offset overflow.
- `fannkuch` improves by at least 20%, `list_index` by at least 15%, and neither
  `list_sum` nor `binary_trees` regresses by more than 5%.
- Bounds-elision disabled mode remains result- and trap-equivalent.

## Track 3: Direct-call inlining and closure devirtualization

### Objective

Remove call overhead where the exact callee and ownership envelope are known,
without growing a benchmark-specific AST rewrite or duplicating semantic
analysis.

### Inlining stage

Inlining occurs on typed WIR after monomorphization and before local pruning and
tail-call formation. The pass consumes exact callable-layout signatures, so it
does not reconstruct types from names.

An initial candidate must:

- have an exact direct target;
- contain at most 24 weighted WIR nodes;
- contain no loop, suspension, host call, dynamic dispatch, or unsupported
  reference/capability boundary;
- have one return envelope that the caller can represent exactly;
- not be directly or mutually recursive;
- not increase the caller's weighted size by more than 15% unless it is called
  once.

The cost model assigns larger weights to calls, allocations, branches, and
multi-value adapter work. It records accept/reject reasons in deterministic
counters.

### Distinct optimizations

1. **Tiny direct leaf inlining.** Substitute parameters with hygienically
   renamed locals and splice the result envelope into the caller.
2. **Known-closure devirtualization.** A non-escaping closure with a statically
   known code target becomes a direct call with scalar capture operands.
3. **Inline-after-devirtualization.** Apply the same cost model to the resulting
   direct closure body.
4. **Single-use wrapper elimination.** Remove compiler-generated adapters whose
   only work is exact-layout argument/result forwarding.

### Ownership

- New module: `crates/witchy-wir/src/wir_opt/inline.rs`.
- Callable-layout and ownership-envelope contracts remain owned by existing
  lowering code. This track consumes them and must decline optimization when
  they are incomplete.

### Acceptance

- WIR tests cover hygiene, multiple returns, traps, value/capacity write-back,
  reference roots, and explicit rejection of recursion and suspension.
- `closure_calls` improves by at least 20% and emits no indirect call in its hot
  loop.
- `record_build`, `expr_eval`, and compile time do not regress by more than 5%.
- Code-size growth across the benchmark corpus stays below 10%.

## Track 4: Range-driven residual integer simplification

### Objective

Simplify the signed-safe power-of-two sequences already emitted by RFC-0145
when dominating facts prove their cheaper nonnegative forms are exact. Add
other constant-divisor rewrites only when profiling attributes enough residual
time to them.

### Landed baseline

The expression lowerer already replaces division and remainder by positive
powers of two. For a syntactically proven nonnegative operand it emits a shift
or mask; otherwise it emits the signed-correct bias sequence. The missing
mechanism is control-flow-sensitive propagation: for example, the body of
`while n > 1` knows `n` is positive, but the current local syntactic predicate
cannot consume that dominating fact.

### Range facts

The pass consumes a small lattice attached to WIR locals:

```text
Unknown | NonNegative | Positive | Constant(i64) | ClosedRange(lo, hi)
```

Facts arise from integer literals, range loops, dominating comparisons, and
simple fact-preserving arithmetic. Joins lose precision conservatively. The
pass is not a general theorem prover.

### Distinct optimizations

1. **Dominating-fact simplification:** reduce the landed signed-safe remainder
   sequence to `x & (2^k - 1)` when a dominating fact proves `x >= 0`.
2. **Dominating-fact division:** reduce the landed bias-plus-shift sequence to a
   direct shift when a dominating fact proves `x >= 0`.
3. **Branch-consumer fusion:** lower `(x % 2^k) == 0` directly to a masked test
   when the same proof permits it, without materializing a remainder.
4. **Signed non-power-of-two constant division:** use a proven magic-number
   sequence only after exhaustive frozen-vector tests cover negative values,
   minimum integer, truncation toward zero, and the `MIN / -1` trap/overflow contract.
   This optimization is conditional on a profile showing such a divisor is hot.
5. **Paired quotient/remainder reuse:** when one dominating operand pair feeds
   both `/` and `%`, compute once or derive the remainder from the quotient if
   the backend does not already fuse them.

### Ownership

- New module: `crates/witchy-wir/src/wir_opt/strength_reduce.rs`.
- A narrow fact producer may live beside existing loop facts in
  `witchy-lower`; it exports facts through WIR metadata rather than sharing
  mutable compiler state.

### Acceptance

- Frozen exhaustive vectors cover constants, signs, zero divisors, `i64::MIN`,
  and wrapping neighbors on both optimized and deoptimized Wasm.
- Generated WAT proves the targeted divisions/remainders disappeared.
- `collatz` improves by at least 15%; `loop_sum`, `mandelbrot`, and `expr_eval`
  remain within 5%.
- No rewrite relies on benchmark-specific knowledge that Collatz values happen
  to remain positive; positivity must be represented by a dominating fact.

## Track 5: Bounded recursive inlining

### Objective

Reduce the unavoidable call count of tiny non-tail scalar recursion that Track
3 deliberately refuses to inline without bound.

### Landed baseline and entry criterion

The current optimized `fib` WAT is already a minimal `(param i64) (result i64)`
function. Its hot body has a comparison, two direct recursive calls, and an
addition. There are no universal-slot conversions, capacity lanes, adapters,
or redundant parameter copies to compact. Track 5 therefore does not create a
new call ABI.

Track 5 begins only if a native profile and generated-code comparison attribute
at least 10% of `fib` to recursive call/return overhead. Otherwise the gap is
recorded as a backend ceiling and this track closes without production code.

### Design

A recursive function eligible for bounded inlining must be scalar-only, have
one small acyclic body around its recursive edges, have no allocation, host
call, suspension, indirect call, ownership envelope, reference, or capability,
and fit a weighted code-size budget. The pass clones one recursion level at a
selected hot call site, but leaves recursive calls in the clone. It never tries
to reach a fixpoint.

The initial maximum is one cloned level and 64 added weighted WIR nodes per
strongly connected component. A second level may be evaluated only as a
separate control row. Recursive base-case tests and traps are copied exactly;
evaluation order remains left-to-right. Mutual recursion is rejected in the
first slice rather than receiving a dispatcher rewrite.

### Work decomposition

- Track 5A: profile and count the current `fib` hot-edge calls, instructions,
  and call/return samples.
- Track 5B: bounded-recursion eligibility and deterministic reason counters.
- Track 5C: one-level clone with hygienic locals and preserved evaluation order.
- Track 5D: depth-zero, depth-one, and optional depth-two code-size/performance
  control rows.

Track 5B begins only if 5A attributes at least 10% of `fib` to call/return
overhead that bounded inlining can remove.

### Acceptance

- Self-recursive fixtures preserve result, trap, evaluation order, and stack
  behavior on interpreter and Wasm; mutual recursion is a pinned rejection.
- `fib` improves by at least 10% without source rewriting or memoization.
- `collatz` results are evaluated independently from Track 4 so gains are not
  double-counted.
- No ABI changes and no cloning of host, capability, reference, closure,
  ownership-envelope, or indirect-call bodies.

## Track 6: Packed-boolean strided-loop kernels

### Objective

Exploit the already-landed one-byte `List(Bool)` representation and
`memory.fill` constructor in strided loops without changing source semantics or
switching to bit packing.

### Landed baseline

`list.repeat(value, n)` for concrete `List(Bool)` already routes to
`list_repeat_bool`, allocates an eight-byte header plus one byte per element,
and initializes the payload with `memory.fill`. Reads and writes already have
byte-layout-specific lowering. This track owns only repeated loop access that
the current per-expression paths cannot hoist or prove.

### Distinct optimizations

1. **Stable header hoisting:** reuse Track 2's sequence plan for repeated
   `length`, read, and write operations.
2. **Proven byte load/store:** emit direct byte operations for a proven valid
   packed-boolean index, with no slot conversion.
3. **Stride-step marking:** in sieve-like loops, retain a scalar pointer cursor
   advanced by the proven positive step rather than recomputing base plus index.
4. **Read-only count reduction:** only for a separately proven pure traversal,
   use 16-byte loads plus lane reduction and a scalar tail. The interleaved
   outer loop in `nsieve` is not eligible merely because it increments a count.
5. **Constructor control row:** pin the landed one-byte `memory.fill` path so a
   traversal win cannot conceal a constructor regression.

### Ownership

- Track 6 consumes Track 2's `SequenceAccessPlan` and owns a new focused
  packed-boolean WIR kernel module; the existing constructor helper remains the
  control implementation.
- It does not change `List(Bool)` layout or public APIs.

### Acceptance

- Allocation bytes remain one byte per boolean payload plus the existing fixed
  header and allocator metadata.
- WAT contains byte loads/stores or `v128` operations and no universal-slot
  conversions in the promoted loop.
- `nsieve` improves by at least 15%, with scalar, tail, empty, and small-list
  fixtures result-equivalent.
- `List(Bool)` equality, iteration order, traps, and mutation semantics remain
  unchanged.

## Track 7: Conditional borrowed-string hash residual

### Objective

Close the remaining `knucleotide` gap only if fresh attribution shows hashing,
slice equality, or materialization remains dominant after Tracks 2 through 6.

### Entry criterion

This track is deferred unless a matched profile attributes at least 10% of
`knucleotide` kernel time to one of:

- duplicate hash computation for the same `(ptr,len)` view;
- scalar tail handling in borrowed-string equality;
- repeated length/header extraction that can be hoisted;
- accidental owned-string materialization.

### Candidate optimizations

1. Thread a computed 64-bit hash through one typed dictionary operation so the
   probe and insertion/update path do not recompute it.
2. Retain `(ptr,len)` in locals across one loop iteration rather than rebuilding
   the pair at each call boundary.
3. Improve SIMD equality tail handling only if instruction/profile evidence
   shows it dominates real key shapes.
4. Reject any path that restores insertion-order bookkeeping or changes String
   and Bytes equality semantics.

### Acceptance

- Exact counters prove one hash per semantic dictionary operation.
- Owned-string materialization remains zero for the borrowed hot path.
- `knucleotide` improves by at least 10%, and `dict_count` plus `word_count`
  remain within 5% of their protected baselines.

## Dependency graph and agent ownership

```text
Track 0: evidence contract
    ├── Track 1: awaited select fusion ── 1A → (1B || 1C) → 1D
    ├── Track 2: sequence plans ───────────────┬── Track 6: packed Bool
    ├── Track 3: WIR inlining                  │
    ├── Track 4: strength reduction            │
    └── Track 5A: recursion profile → 5B → 5C → 5D │
                                                └── Track 7, only if triggered
Integration track: shared pass ordering, full matrix, acceptance ledger, queue
```

Each agent owns its new module, focused tests, counters, benchmark artifact,
and ledger row. The integration owner alone edits pass ordering and shared
module registration after narrow interfaces are frozen. If two tracks need the
same existing hunk, they agree on a small interface commit first rather than
rebasing conflicting implementations repeatedly.

Suggested branch and file ownership:

| Track | Suggested branch | Primary ownership |
|---|---|---|
| 0 | `perf/rfc0146-evidence` | benchmark harness, stats bounds, ledger |
| 1A | `perf/rfc0146-select-profile` | landed-path counters, profile, WAT census |
| 1B/1C | `perf/rfc0146-select-plan`, `perf/rfc0146-select-wir` | typed awaited-select plan, suspension-safe WIR result |
| 2 | `perf/rfc0146-indexed-kernels` | new lowerer module and focused WAT tests |
| 3 | `perf/rfc0146-inline` | new WIR inliner module |
| 4 | `perf/rfc0146-strength-reduce` | new WIR arithmetic pass |
| 5 | `perf/rfc0146-recursive-inline` | bounded scalar recursive inlining |
| 6 | `perf/rfc0146-packed-bool` | packed-bool kernels consuming Track 2 interface |
| 7 | `perf/rfc0146-string-hash` | borrowed-string/dictionary residual only |
| integration | `perf/rfc0146-integration` | pass order, full evidence matrix, ledger closeout |

## Pass ordering

The intended initial WIR order is:

1. monomorphized lowering and exact layout construction;
2. known-closure devirtualization;
3. conservative inlining;
4. bounded recursive inlining for explicitly eligible components;
5. range-fact propagation and residual strength reduction;
6. indexed-kernel and packed-boolean lowering where the plan is still valid;
7. proper-tail-call formation;
8. local pruning, peephole cleanup, and encoding.

This order is provisional. The integration track must pin it with interaction
tests. In particular, inlining must not hide a tail edge, and cursorization must
not run before an opaque inlined call is known to be harmless.

## Promotion and rollback rules

An optimization is promoted only when all of the following are true:

1. independent expected output and backend parity are green;
2. deterministic counters prove the intended mechanism fired;
3. generated WAT or machine profile confirms the expected code shape;
4. the owning benchmark meets its track threshold on twelve matched samples;
5. every named protected workload stays within 5% of its baseline median;
6. memory, code size, and compile time stay inside the track's limits;
7. the focused shard and merge-queue gate pass;
8. the acceptance ledger names the terminal merged commit and artifact paths.

If a branch improves only a microbenchmark, moves cost into another phase, or
cannot reproduce its improvement on the same workload, it is rejected rather
than retained behind a default-on lever. A useful experimental pass may remain
default-off only when the RFC names the additional evidence required to revive
it.

## Program-level acceptance criteria

The RFC closes only when:

1. all 17 benchmark programs remain result-correct;
2. every non-async benchmark has a Witchy/Go kernel median at or below 1.00x on
   the reference host, with no confidence interval indicating a regression
   larger than 5%;
3. `select_fanin` is at most 2.0x of Go and has zero steady-state select
   allocations; exact Go parity is a follow-on target, not purchased by
   weakening deterministic scheduling;
4. the full workspace gate passes with fixed-arena-aware memory assertions;
5. the deoptimization matrix preserves results and traps;
6. compile time and emitted Wasm size across the benchmark corpus each grow by
   less than 10%;
7. no new per-method ownership fast paths, ambient authority, unsafe host
   shortcuts, or interpreter-only semantic patches are introduced; and
8. `rfcs/0146-acceptance-ledger.md` proves every row with merged commits and
   external raw artifact paths.

## Delivery strategy

Phase A freezes Track 0 and the three shared data contracts:
`AwaitSelect2Plan`, `SequenceAccessPlan`, and the WIR range-fact metadata. Phase B
starts Tracks 1, 2, 3, 4, and 5A concurrently. Phase C starts Track 6 after the
sequence-plan interface lands and decides whether Track 7 meets its entry
criterion. The integration branch continuously rebases completed tracks onto
current `master`, runs interaction tests, and submits compatible disjoint
branches for queue batching.

Progress reports use acceptance rows and unresolved dependencies, not commit
counts. A live full gate never blocks independent implementation work; agents
continue on fresh branches unless their next task depends on the queued commit.

## Alternatives

### Continue RFC-0145 unchanged

RFC-0145's categories were directionally useful, but its benchmark landscape
and dictionary/text priorities are stale after RFC-0143 and RFC-0144. Continuing
it unchanged would direct agents toward already-closed gaps and leave current
mechanisms insufficiently separated.

### One broad optimizer branch

A single branch simplifies pass ordering initially, but destroys causal
measurement, creates conflicts in compiler hotspots, and serializes work that
can proceed independently. This RFC instead freezes small shared contracts and
assigns one mechanism per branch.

### Rely on Binaryen or Cranelift

Backend optimizers cannot infer Witchy's ownership tokens, deterministic select
semantics, stable list roots, or borrowed-view lifetime facts after those facts
have been erased. They remain valuable controls: each track should compare its
output with the backend-only result. They are not substitutes for typed
front-end and WIR mechanisms.

### Benchmark-specific intrinsics

Recognizing `fannkuch`, `collatz`, or a particular stdlib function could produce
large headline wins quickly. It would not generalize, would be difficult to
deopt, and violates the project's typed-mechanism rule. The proposed plans are
derived from layouts, range facts, call graphs, and async semantics instead.

### Parallelize the compute benchmarks

Parallelism would change the resource model and hide single-core compiler
costs. These kernels are controls for generated-code quality. Parallel workers
remain a separate throughput feature and are not used to satisfy this RFC.

## Drawbacks

- Seven tracks and an integration owner create process overhead. The explicit
  interfaces are justified because the alternative is repeated conflict in
  large compiler modules.
- Exact range facts and callable layouts add metadata that can increase compile
  time even when no optimization fires.
- Inlining can increase Wasm size and instruction-cache pressure.
- Awaited-select fusion adds a compiler path that must remain behaviorally
  identical to the landed `Pull2`/`Wait2` source implementation.
- Twelve-sample matched evidence is slower than the fast sweep and may expose
  host noise that delays promotion.
- The program target is host- and workload-specific. Passing it demonstrates
  parity for the protected matrix, not universal superiority over Go.

## Prior art

- SSA loop-invariant code motion, induction-variable simplification, scalar
  replacement of aggregates, and sparse conditional constant propagation.
- Go's bounds-check elimination and escape analysis provide useful controls for
  the paired benchmark implementations.
- Producer-consumer fusion and scalar replacement of non-escaping async state
  follow established deforestation and coroutine-lowering techniques.
- Magic-number integer division is established compiler practice, but this RFC
  requires Witchy-specific frozen semantics before enabling it.
- WebAssembly multi-value results and exact local kinds provide the low-level
  substrate for fused select results and hygienic inlining.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
  - The current behavior lives in spec/ and the code, not here.
-->
