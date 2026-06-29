---
rfc: 0032
title: Multi-core execution — true parallelism vs the deterministic executor
created: 2026-06-29
status: proposed
tracking:
---

# RFC-0032: Multi-core execution — true parallelism vs the deterministic executor

## Summary

witchy's concurrency — `spawn` plus first-class channels — runs as a
**single-threaded cooperative executor inside one wasm instance**. Tasks
interleave; they do not run on separate cores. A CPU-bound witchy program
therefore uses exactly one core, no matter how many tasks it spawns. This RFC
does NOT propose shipping multi-core execution; it records the design space, the
two viable shapes, and — most importantly — **what each one costs in the
guarantees witchy is built on** (twin-backend parity, value semantics,
capability isolation, deterministic execution), so a future decision is informed
rather than ad hoc. The honest conclusion up front: true multi-core conflicts
with the current determinism/parity model, so it is a deliberate architecture
choice, not an optimization knob — and [0031](0031-simd-stdlib-hot-loops.md)
(SIMD) is the parallelism we can take *without* paying that price.

## Motivation

The performance survey behind [0030](0030-perf-correctness-infra.md) and the
benchmark work found the compile path already parallel (wasmtime) and SIMD as a
free data-parallel lever, but the runtime as the genuinely untapped axis:

- `spawn` and channels are scheduled by a pure-witchy deterministic executor in
  one wasm instance (the per-VM actor model was retired in favor of this; see the
  concurrency-redesign history). The only host thread in the runtime is a
  preemption *watchdog*, not a worker.
- So `worker_pool`-style or fan-out compute does not scale across cores — it
  time-slices one. For I/O-bound or interleaving work that is fine (and the
  determinism is a feature); for embarrassingly-parallel compute it leaves cores
  on the table.

The question this RFC answers is not "can we add threads" but "what would we have
to give up, and is there a shape that keeps witchy's guarantees."

## Design — two shapes, and what each costs

### Option A — wasm threads / shared linear memory

Use the wasm threads proposal (shared `memory`, atomics) and real OS worker
threads in the host; the executor schedules tasks onto a thread pool.

- **Breaks value semantics at the representation level.** witchy's optimizations
  (in-place update, RC-floor reclamation, packed layouts) are all justified by
  "one owner, no other observer." Shared mutable linear memory across threads
  voids that proof globally unless every cross-thread value is provably immutable
  or uniquely transferred — i.e. it would require `frozen`/`unique`
  ([0025](0025-frozen-deep-immutability.md)/[0026](0026-unique-qualifier.md)) to
  become load-bearing *safety* qualifiers, not just contracts.
- **Breaks the scalar/deterministic oracle.** The interpreter (the parity oracle)
  is single-threaded; there is no obvious way for it to reproduce true-parallel
  interleavings bit-for-bit. Parity would have to weaken from "identical output"
  to "identical output for race-free programs," and we'd need a race detector to
  define "race-free."
- **Complicates the capability/sandbox story** (shared memory + threads is more
  attack surface; the per-instance isolation we rely on dissolves).

Verdict: maximal speedup, maximal cost to the invariants. Not recommended as the
first step.

### Option B — multiple wasm instances + channel message-passing

Each worker is its **own wasm instance** with its **own** linear memory; the
*only* sharing is channels, which already move/copy values (CSP). The executor
runs N instances on a host thread pool and routes messages between them.

- **Preserves value semantics and the per-instance optimizations** — nothing is
  shared mutably; a message is a copied/moved value, exactly today's channel
  semantics. In-place/RC-floor/packed reasoning stays valid *within* each
  instance.
- **Preserves capability isolation** — each instance gets its own (attenuated)
  capabilities, as today.
- **This is, deliberately, a return to a per-VM actor model** — the very thing
  retired for the single-VM deterministic executor. The retirement reasons must
  be confronted: cross-instance message marshaling cost, one-message-type-per-
  program constraints, and (the big one) **determinism** — N instances on a
  thread pool interleave message delivery non-deterministically.
- **Determinism is recoverable here, unlike Option A**: because instances share
  nothing but messages, observable output is deterministic *if message ordering
  is deterministic*. A deterministic scheduler (fixed task order, or
  content-addressed message queues drained in a defined order) keeps the parity
  oracle viable — the interpreter runs the same instances on its single thread in
  that same defined order. Parallelism becomes a host *scheduling* detail that
  does not change observable results. This is the crux that could make multi-core
  parity-safe.

Verdict: the only shape that plausibly keeps witchy's guarantees. The cost is
real (marshaling, the actor-model machinery, a deterministic scheduler) but it is
*architectural*, not *foundational*.

### Cross-cutting requirements (either option)

- A parity story for the differential sweep (the oracle must reproduce results).
- A `witchy stats` / bench proof that adding cores actually scales the target
  workload (e.g. a parallel map over a large list: near-linear speedup to core
  count, identical output).
- A capability model for spawning a worker (a new authority? a refinement of the
  channel/spawn caps?).

## Alternatives

- **Status quo + SIMD only** ([0031](0031-simd-stdlib-hot-loops.md)). The
  pragmatic near-term answer: data parallelism within a core, zero invariant
  cost. Recommended first.
- **Offload to native helpers** (an FFI-style "run this pure function across
  cores in Rust"). Sidesteps the wasm-tier question but reintroduces a native
  trust-boundary and an FFI capability; narrow applicability.
- **Never** — accept single-core compute as the price of determinism + parity,
  and lean on SIMD + the memory model. A legitimate choice for a
  capability-secure, twin-backend language.

## Drawbacks

- Both options weaken or complicate the determinism/parity model that is
  witchy's defining property; Option A breaks it, Option B taxes it with a
  deterministic scheduler.
- Option B revives machinery (per-VM actors) that was removed for good reasons;
  this RFC must not paper over them.
- Large implementation surface (scheduler, marshaling, oracle changes, capability
  design) for a win that only helps embarrassingly-parallel compute — a narrower
  audience than SIMD or the memory-model work.

## Prior art

The prior art splits cleanly along the same Option A / Option B line, which is the
useful way to read it:

- **Shared-memory (Option A family): Go.** Goroutines on an M:N scheduler (G
  goroutines over P logical procs over M OS threads; `GOMAXPROCS`, work-stealing,
  async preemption since 1.14). All goroutines share one heap; channels are
  idiomatic but a *guideline*, not enforcement — direct shared mutation is
  allowed, so **data races are possible** and Go ships a race *detector*, not
  prevention. This buys **fine granularity** (millions of ~2 KB goroutines,
  *because* they share the runtime/heap) at the cost of giving up enforced value
  semantics. That cost is exactly what witchy cannot pay: the in-place / RC-floor
  / packed optimizations and the scalar parity oracle all rest on "one owner, no
  other observer," which a shared mutable heap voids. So Go is the model witchy
  **cannot** adopt without abandoning its foundations — it is Option A.

- **Share-nothing isolates (Option B family): Python subinterpreters, Erlang,
  Web Workers, Ruby Ractors.** This is the shape that fits witchy. **Python**'s
  PEP 684 (per-interpreter GIL, 3.12) gives each subinterpreter its own state and
  GIL so several run bytecode on separate OS threads in true parallel, and PEP 734
  adds the `interpreters` stdlib module to spawn them and pass data over channels —
  i.e. *one isolated interpreter per worker, sharing only via channels*, which is
  Option B almost verbatim (witchy "wasm instance" ≈ Python "subinterpreter").
  **Erlang/BEAM** is the mature version: isolated processes, per-pair message
  ordering, multiple schedulers, no shared mutable state — parallelism coexisting
  with strong per-process guarantees *because* nothing is shared. **JS Web
  Workers** and **Ruby Ractors** are the same bargain.

The granularity trade-off is the honest cost of choosing Option B over Go's
model: an isolated instance carries its own linear memory, so witchy could spawn
**thousands of coarse workers, not millions of fine ones**. Option B therefore
targets coarse, embarrassingly-parallel compute (a worker pool over chunks), not
a goroutine-per-request style. The lesson across BEAM and Python's recent
direction is consistent and decisive for us: **share-nothing + messages is what
lets parallelism coexist with strong guarantees** — which is why Option B
(the Python-subinterpreter / actor shape), not Option A (the Go shape), is the
only candidate if witchy ever takes this on.
