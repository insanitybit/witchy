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
**three** shapes, and — most importantly — **what each one costs in the
guarantees witchy is built on** (twin-backend parity, value semantics,
capability isolation, deterministic execution), so a future decision is informed
rather than ad hoc. The shapes: **A** shared-mutable threads (the Go model —
breaks witchy's invariants); **B** share-nothing instances + channels (the Python
subinterpreter / Erlang model — safe, coarse-grained); and **C** a
*capability-typed shared heap* (Rust `Send`/`Sync` / Pony — Go-like granularity
*without* Go's races, because witchy's value semantics + `frozen`/`unique` already
encode what is safe to share). The honest conclusion up front: A is off the table;
B and C both keep the invariants at an architectural (not foundational) cost, with
C the most promising but gated on making the ownership qualifiers a *sound*
race-freedom guarantee; and [0031](0031-simd-stdlib-hot-loops.md) (SIMD) is the
parallelism we can take *now* without paying any of these prices.

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

Verdict: a shape that keeps witchy's guarantees with a per-instance memory cost.
The cost is real (marshaling, the actor-model machinery, a deterministic
scheduler) but it is *architectural*, not *foundational*.

### Option C — capability-typed shared heap (Go-like granularity, race-free)

The realization that reframes Option A: **Go's races come from shared *mutable*
state, which witchy does not have.** Value semantics mean a callee cannot mutate
what the caller observes — so there is no shared mutable state to race on at the
language level. That opens a third shape: real OS-thread workers over a **shared**
wasm linear memory (wasm threads), made race-free not by a *detector* (Go) but by
the **type system** — i.e. Rust's `Send`/`Sync` or Pony's reference capabilities,
for which witchy already has the machinery.

- **Immutable data is shared by pointer across cores — zero races, zero copies.**
  A `frozen` value ([0025](0025-frozen-deep-immutability.md)), or any value the
  escape oracle ([0024](0024-unified-facts-lattice.md)) already proves is never
  mutated, is safe to read from many threads. That is most of a program's data.
- **Mutable in-place buffers stay task-local — for free.** The uniqueness/escape
  analysis already refuses to mutate-in-place anything aliased or escaping, so a
  value shared with another task is automatically treated as frozen (copied, not
  mutated). Extending "escapes" to include "escapes to another task" keeps the
  in-place / RC-floor / packed optimizations sound with no new proof obligation —
  they simply don't apply to cross-task values, which is correct.
- **Channels move ownership** (`unique`/`own`,
  [0026](0026-unique-qualifier.md)) to hand a mutable buffer between tasks.

The mapping is exact: witchy `frozen` ≈ Pony `val` / Rust `Sync`; `unique`/`own`
≈ Pony `iso` / Rust `Send`; `var`/`let` ≈ `ref`/`box`. The qualifiers, today used
as optimization hints + contracts, become **load-bearing for parallelism**. This
is the rare case where witchy's restrictive choices (immutability, capabilities,
the ownership oracle) become an advantage Go cannot replicate: data-race
*freedom* by construction, while keeping a shared heap.

Granularity beats Option B: with a shared heap a task is just a stack + locals
(not a whole instance with its own linear memory), so workers are **cheap and
fine-grained — the closest to Go's goroutines** that preserves the invariants.

Costs (real, but they EXTEND the model rather than contradict it):
- **Thread-safe runtime.** The bump allocator, RC-floor free-list, and watermark
  are single-threaded today; the clean answer is **per-thread arenas** (each
  thread bumps its own; cross-thread frees are the rare path). Plus the wasm
  threads substrate (shared memory + atomics; wasmtime supports it).
- **Sound qualifier enforcement.** `frozen`/`unique` must become a *complete*
  race-freedom guarantee — deep, transitive, no escape hatches, closures handled
  — not the current partial contracts. This is the genuine type-system lift (what
  Rust's borrow checker and Pony's type system exist for); witchy has the
  foundation, not yet the soundness.
- **Determinism / parity: identical to Option B's catch.** Race-free is not
  schedule-independent (channel-send races on order), so the interpreter-oracle
  still needs a deterministic scheduler to reproduce the result.

Verdict: the most promising *if* the qualifier soundness and thread-safe runtime
are built — it gets Go-like granularity and ergonomics without Go's races,
precisely because witchy already encodes what is shareable (`frozen`) vs
transferable (`unique`/`own`) vs local (`var`/`let`).

### Composing B and C — the recommended direction (two tiers)

B and C are not rivals; they are the **process tier** and the **thread tier** of
one concurrency model, and witchy can have both — exactly as an OS has both
processes and threads, or BEAM has isolated processes on a multi-scheduler
runtime. They answer different questions:

- **Tier B — isolated VM, its own capabilities (the *isolation/security*
  boundary).** A witchy program can `spawn` a **separate VM** that receives an
  **attenuated subset of the parent's capabilities** and communicates only by
  message (a value `frozen`-shared or copied). This is the boundary witchy is
  uniquely positioned to offer: because capabilities are values, a child VM
  *cannot* perform I/O the parent didn't grant — a **sandboxed worker** for
  untrusted plugins, fault isolation, or privilege separation. Coarse-grained
  (each VM owns its linear memory), and the right tool when you want a *trust* or
  *fault* boundary, not just a core.
- **Tier C — lightweight tasks, shared heap (the *performance* boundary).**
  *Within* a VM (its single capability domain), `spawn` lightweight tasks over the
  capability-typed shared heap for fine-grained parallel compute — goroutine-like,
  cheap, race-free by `frozen`/`unique`.

They compose cleanly: a child VM (B) can itself run many C tasks; a C task pool
handles parallel compute inside one trust domain; a B boundary appears exactly
where a *capability* or *fault* boundary is wanted. The distinction is principled
— **B is a capability boundary, C is a core boundary** — so the surface can make
that explicit (e.g. `spawn` for a same-cap task vs a `spawn`-with-caps form for an
isolated VM) rather than conflating "I want a core" with "I want a sandbox."

Data crossing a tier-B boundary has a representation nuance: separate VMs have
separate linear memories, so a `frozen` value is **copied** across by default;
true cross-VM share-by-reference needs a **shared read-only segment** (wasm shared
memory used immutably) — a worthwhile later optimization, not a requirement. Within
tier C, `frozen` is shared by pointer with no copy. Determinism is the same
obligation for both tiers (a deterministic scheduler / message order so the scalar
interpreter reproduces results), and B's hard isolation makes per-VM determinism
the easy part.

This two-tier shape is the recommended target if witchy pursues multi-core: ship
**B first** (it reuses the existing instance + capability machinery and delivers
the distinctive sandboxed-worker value with the least new type-system risk), then
**C** (which needs the `frozen`/`unique` soundness lift) for fine-grained speed —
with [0031](0031-simd-stdlib-hot-loops.md) (SIMD) orthogonal and available now.

### Cross-cutting requirements (all options)

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

- All shapes touch the determinism/parity model: A breaks it (shared mutation +
  an unreproducible interleaving for the scalar oracle); B and C both keep it but
  require a deterministic scheduler so the interpreter reproduces results.
- Tier B revives per-VM-actor machinery that was removed for good reasons
  (marshaling cost, message-type constraints); this RFC must not paper over them —
  though the capability-attenuation payoff is new and changes the calculus.
- Tier C needs the big lift: `frozen`/`unique` must become a *sound* race-freedom
  guarantee (deep, transitive, no escape hatches), plus a thread-safe runtime
  (per-thread arenas). This is the type-system + allocator work that gates it.
- Large implementation surface overall (scheduler, marshaling/atomics, oracle
  changes, a spawn-with-caps capability) — justified only when embarrassingly-
  parallel compute or sandboxed-worker isolation is genuinely needed; SIMD
  ([0031](0031-simd-stdlib-hot-loops.md)) and the memory model serve the common
  case first.

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

- **Capability-typed shared heap (Option C family): Rust, Pony.** Shared memory
  made race-free by the *type system* rather than a detector. Rust's `Send`/`Sync`
  and Pony's reference capabilities (`iso`/`val`/`ref`/`box`/`tag`) statically
  partition values into transferable / shareable / local — exactly witchy's
  `unique`·`own` / `frozen` / `var`·`let`. This is how you get Go's shared-heap
  *granularity* without Go's races; witchy is unusually close to it already.

The granularity trade-off is the honest contrast: Option B's isolated instance
carries its own linear memory (thousands of coarse workers, not millions), while
Option C shares the heap (cheap, fine-grained, goroutine-like). The decisive
lesson across BEAM, Python, Rust, and Pony is that **safe parallelism comes from
controlling sharing** — by isolation (B) or by type (C) — never from Go's
shared-mutable default (A). So **A is out; B and C are both viable, and they are
complementary, not rival** (see "Composing B and C" above).
