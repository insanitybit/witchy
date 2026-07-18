---
rfc: 0032
title: Multi-core execution — true parallelism vs the deterministic executor
created: 2026-06-29
status: implemented
tracking: "IMPLEMENTED (2026-06-29). The std `vm` module delivers true multi-core +
  isolation, parity-safe: vm.par_map (multi-core map across OS-thread worker VMs, for
  Int/String/Bytes), vm.with_dir (capability-passing: run f in a worker granted exactly
  one Dir), vm.serve (cross-VM channel: a stateful service on a persistent isolated
  worker). Plus the Tier-C cooperative ladder (chan.par_map/scope/gather/race/cancel) and
  the Bytes type (std/bytes). Free-racing async + in-VM shared-heap threads are deliberate
  non-goals (parity-incompatible / research-grade). Original tracking below.
  Tier-C STRUCTURED concurrency shipped as pure-witchy stdlib over the
  existing cooperative executor (no runtime change, parity by construction) — the
  full constraint ladder: level 1 chan.par_map/par_reduce (input-ordered parallel
  map+fold, deterministic by construction), level 2 chan.scope (spawn-all/join-all
  nursery) + chan.gather (typed fan-out-and-collect) + spawn_all/join_all, level 3
  the pre-existing escaping spawn/Handle — leak-free, no escaping handle (1-2),
  deterministic. PLUS cancellation: a Cancel Step extension to the executor (in BOTH
  task+chan, kept structurally in sync) + chan.cancel + chan.race (first-result-wins,
  loser cancelled, deterministic tie). Provides real COOPERATIVE concurrency now;
  see examples/scope. REMAINING (native runtime — not a pure-witchy change):
  (1) Tier B vm.spawn — isolated VM + attenuated caps + cross-VM channels (the
  sandboxed-worker value); native runtime both backends. (2) True multi-core — a
  native parallel scheduler (in-VM = wasm-threads + thread-safe allocator + SOUND
  frozen/unique race-freedom = research-grade; OR OS-thread child VMs = the Tier-B
  backend); parity-neutral (must match the cooperative executor's deterministic
  results)."
---

# RFC-0032: Multi-core execution — true parallelism vs the deterministic executor

The shipped isolated-worker surface is implemented in
[`std/vm.witchy`](../std/vm.witchy) and the native worker runtime in
[`crates/witchy-runtime/src/runtime.rs`](../crates/witchy-runtime/src/runtime.rs),
with parity coverage in [`src/example_tests.rs`](../src/example_tests.rs); the
free-racing/shared-heap designs remain explicit non-goals.

## Summary

> **Status: implemented (2026-06-29).** This began as a design-space RFC. It is now
> shipped — the `vm` std module gives witchy real multi-core execution and isolated
> worker VMs while preserving twin-backend parity. The body below records the design
> reasoning; the **Implementation status** section at the end describes what was built
> and why the parity invariant *shaped* the surface (chose Option B share-nothing
> instances; made cross-VM channels lock-step rather than free-racing). The original
> "does not propose shipping" framing is preserved for the historical record.

witchy's `chan`/`task` concurrency runs as a **single-threaded cooperative executor
inside one wasm instance**: tasks interleave, they do not run on separate cores. The
question this RFC set out to answer — and now answers in code — is how to get genuine
multi-core and isolated workers **without** giving up the guarantees witchy is built on
(twin-backend parity, value semantics, capability isolation, deterministic execution).
It records the design space, the **three** shapes, and **what each costs**. The shapes: **A** shared-mutable threads (the Go model —
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

### Structured concurrency — a constraint ladder *within* Tier C

Tier C should not be a single primitive. **Structured concurrency** (Trio
nurseries, Swift task groups, Kotlin `coroutineScope`, Java/Loom
`StructuredTaskScope`, and especially Rust's *scoped threads*) shows that
*constraining* a task's lifetime to a lexical scope is not a tax — it removes
Go's leaked-goroutine / swallowed-error footgun AND unlocks sharing the looser
forms cannot have. The decisive witchy-specific payoff: **a scope-bounded task
may `let`-BORROW the parent's data by reference (zero copy, no `frozen`)**, because
the scope guarantees the task is joined before the borrow's owner returns — exactly
why Rust scoped threads borrow non-`'static` data while spawned threads need
`'static`. It maps onto the existing conventions: `let` = borrow (scope-bounded
tasks), `frozen`/`own` = sendable (escaping tasks). So there is a ladder where
*more constraint buys more sharing*, the inverse of a cost:

1. **Parallel combinators** (`chan.par_map` / `par_reduce`) — tasks not even
   visible; spawn+join internal; results returned as a value. Cannot leak, cannot
   deadlock on a forgotten join, and **deterministic by construction** (pure fn
   over a list → trivially parity-safe). The ergonomic default for data
   parallelism, and the easiest piece to ship first.
2. **Task scope / nursery** (`scope sc:`) — explicit, heterogeneous tasks, still
   lexically joined, still `let`-borrow-friendly; failures surface at the scope.
3. **Free tasks** (`chan.spawn` → `Handle`) — escaping lifetime for long-lived /
   background / actor loops, but `frozen`/`move` data only (no borrows). The
   *advanced/looser* form, not the primitive — inverting the usual "spawn first,
   structure bolted on."

The constraint is enforced as a type rule, not new grammar: a closure passed to a
scope-bounded spawn may capture `let`-borrows that outlive the scope; one passed to
the escaping `chan.spawn` may not (it needs `frozen`/`move`). Recommendation: make
1–2 the default surface (possibly the *only* Tier-C forms in v1), and treat 3 as
opt-in — learning from Go's mistake rather than reproducing it.

### Cross-cutting requirements (all options)

- A parity story for the differential sweep (the oracle must reproduce results).
- A `witchy stats` / bench proof that adding cores actually scales the target
  workload (e.g. a parallel map over a large list: near-linear speedup to core
  count, identical output).
- A capability model for spawning a worker (a new authority? a refinement of the
  channel/spawn caps?).

## Surface sketch

The two-tier model needs **almost no new grammar** — it reuses the existing
capability-narrowing (`cap as Narrower`), `spawn`, and channel syntax.

**Tier C — lightweight tasks.** The *recommended default* is the structured
forms, which need only a `scope` block on top of the existing `chan` types and let
tasks `let`-borrow parent data (no copy):

```witchy
async fn summarize(let docs: List(Doc)) -> List(Summary):
    # level 1 — structured parallel map: spawn+join internal, borrows `docs`, no handles
    chan.par_map(docs, fn(d): heavy_summary(d)).await

async fn pipeline(console: Console, let input: List(Job)) -> Nil:
    scope sc:                              # level 2 — nursery: all tasks joined at block end
        sc.spawn(fn(): stage_a(input))     # borrows `input` by ref — sound: joined before return
        sc.spawn(fn(): stage_b(input))
    # <-- both joined HERE; a failure in either surfaces here; no escaping handle
    chan.done(nil).await
```

The looser **escaping** form is the *existing* `chan` API unchanged — it just
*becomes parallel*; the cost of the unbounded lifetime is `frozen`/`move` data
only (no borrows):

```witchy
async fn worker(rx: Receiver(Job)) -> Nil:
    chan.serve(rx, 0, fn(acc, job): chan.done(acc + heavy(job))).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(16).await
    let h = chan.spawn(worker(rx)).await   # level 3 — escaping handle; frozen/move data only
    chan.send(tx, job).await
    chan.join(h).await
```

**Tier B — isolated VM with attenuated caps: a `vm` module, used like
`chan.spawn`.** The only new surface is `vm.spawn`/`vm.join`; attenuation is
ordinary capability narrowing. The child runs in a separate VM and gets EXACTLY
the caps + channel ends passed — nothing else is reachable:

```witchy
import vm
import chan

async fn render(rx: Receiver(m), out: Sender(m), net: Net[Connect]) -> Nil:
    # this VM can ONLY connect over net, recv jobs, send results — no Dir, no
    # Exec, no parent heap: physically unreachable, not merely unused.
    chan.serve(rx, "", fn(_, tmpl): chan.done(fetch_and_render(net, tmpl))).await

async fn main(console: Console, net: Net):
    let (jobs, jobs_rx) = chan.channel(8).await
    let (out, out_rx)   = chan.channel(8).await
    # isolated VM, delegating ONLY net narrowed to Connect (+ the channel ends):
    let child = vm.spawn(render(jobs_rx, out, net as Net[Connect])).await
    chan.send(jobs, frozen template).await    # frozen ⇒ copied across the boundary
    let html = chan.recv(out_rx).await
    vm.join(child).await
```

The rules (type-checker, not grammar):

- `vm.spawn(entry(args))` mirrors `chan.spawn(entry(args))` — same call shape; the
  difference is semantic (isolate, re-grant the cap args inside the child, route
  channel endpoints across).
- **Marshalable args** — the one new type rule. Across a VM boundary you may pass
  capabilities (re-granted, narrowed by `as`), channel endpoints (made cross-VM),
  and `frozen`/`move`d data (copied — separate memories). A plain mutable
  parent-heap value is a compile error — the `frozen`/`unique` machinery doing its
  job.
- The distinction is legible at the call site: `chan.spawn` = "a core" (same VM,
  shared heap, tier C); `vm.spawn` = "a sandbox + core" (isolated VM, attenuated
  caps, tier B) — one form each, no overloading.
- Capability honesty: delegating a *subset* of your own caps needs no new
  authority (it's attenuation). A resource limit on spawning VMs would be a
  separate `Spawn` capability threaded from `main` like any other
  (`async fn main(console: Console, net: Net, spawn: Spawn):` →
  `vm.spawn(spawn, render(...))`).

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

## Implementation status

**Shipped (2026-06-29) — Tier-C structured concurrency, pure-witchy stdlib.** Over
the existing cooperative `Step`/`Task` executor (no runtime change; parity by
construction), the **complete constraint ladder**:

- **Level 1 — parallel combinators:** `chan.par_map` (map a task over a list
  concurrently, results in INPUT order — each item gets a private channel so the
  order is independent of completion order, making the result a *pure function of
  the inputs*) and `chan.par_reduce` (par_map + associative fold). Tasks never
  visible; cannot leak or deadlock on a forgotten join; deterministic by
  construction. The ergonomic default for data parallelism.
- **Level 2 — task scope / nursery:** `chan.scope` (spawn-all/join-all nursery),
  `chan.gather` (typed fan-out-and-collect), and `chan.spawn_all`/`join_all`.
  Leak-free, no escaping handle, failures surface at the scope.
- **Level 3 — escaping tasks:** the pre-existing `chan.spawn` → `Handle` for
  long-lived/background loops.
- **Cancellation:** a `Cancel` `Step` extension to the cooperative executor
  (`std/task` owns the scheduler; `std/chan` delegates its public surface there),
  plus `chan.cancel(handle)` (shallow, idempotent) and
  `chan.race(a, b)` — run two tasks, return the first result, cancel the loser; the
  winner of a tie is fixed by the round-robin schedule, so the outcome is
  deterministic and byte-identical on both backends. Build `timeout` by racing
  against a sentinel-yielding task.

All deliver the constraint-ladder's forms — deterministic, byte-identical on both
backends — i.e. **real cooperative concurrency** (overlapped I/O, structured
lifetimes, data-parallel map/reduce) today. Demo: `examples/scope`. The level-1/2
forms' determinism is precisely the property a future parallel backend must
preserve: it runs the items on separate cores *without changing the observable
result*, so multi-core becomes a backend swap rather than a semantics change.

**Shipped (2026-06-29) — TRUE MULTI-CORE for data-parallel compute (Option B).**
`vm.par_map(xs, f)` (std/vm) over scalar elements with a top-level (capture-free) `f`
runs on the compiled backend across **N worker VMs** — one wasmtime instance + linear
memory per core, capped by element count — each processing a contiguous chunk in
parallel (`std::thread::scope`), results reassembled in input order. This is RFC-0032's
**Option B (share-nothing instances)** realized: workers share nothing, the mapped
function is re-entered by table index (`__call_idx`, NULL env) in each fresh instance,
and scalars marshal as flat i64s. Measured **4.2× at 620% CPU** (10-core, 8×200M-iter
elements), byte-identical output to the sequential run.

Parity holds *without* spending the invariant: a pure `f` collected by index makes the
parallel result equal the sequential one, so the interpreter oracle still matches
(`vm_par_map_backends_agree`). Soundness is enforced at lowering — only scalar elements
(no pointer marshaling across memories) and a top-level `f` (no captured parent-heap
state) take the native path; every other shape falls through to the sequential
`list.map` body, always correct (`vm_par_map_capturing_closure_agrees`). The two-step
build: `vm.par_map` scaffold (sequential, both backends agree) → host re-entry trampoline
→ parallel worker VMs.

**Shipped (2026-06-29) — Tier-B isolation MECHANISM (zero-authority workers).** The
multi-core `vm.par_map` workers run with **zero ambient capabilities**: a worker's
linker grants only the authority-free staging imports and defines every other host
import as a TRAP (`Linker::define_unknown_imports_as_traps` — deny-by-omission). A
worker is thus share-nothing (own linear memory) AND authority-nothing — it physically
cannot reach the filesystem, network, or any host resource, despite sharing the
parent's compiled module. This is the capability-attenuation/sandbox mechanism Tier B
is built on, now proven on the multi-core path.

**Remaining for the FULL `vm.spawn` (native subsystem — a dedicated effort).** The
isolation mechanism above plus the worker-VM instantiation machinery are the
foundation; the full surface needs two more substantial pieces, deliberately not
half-built:

1. **Capability *passing* to a child — SHIPPED (2026-06-29).** `vm.with_dir(dir, f,
   input)` runs a top-level `f(Dir, Bytes) -> Bytes` in an isolated worker VM granted
   EXACTLY the passed `Dir` (the parent's read/write rights re-granted into the worker's
   `VmState`) and nothing else — every ungranted host import traps. A worker reads a file
   through the passed `Dir` and the interpreter (runs `f` directly) and compiled backend
   (isolated worker) agree, since the isolation is invisible to the result. The same
   pattern (read the parent grant → build the worker `VmState` + link only those caps)
   generalizes to `Net`/`File`. The checker requires `f` to be a bare top-level
   function, and codegen repeats that check as a backstop: an alias or closure is an
   error, never a silent parent-VM fallback. Tests: `vm_with_dir_capability_passing_agrees`
   and `isolated_worker_apis_reject_indirect_callbacks`.
2. **Cross-VM channels (the last remaining piece)** — the parent and child have separate linear memories, so a
   channel between them cannot be the current in-VM pure-witchy data structure.
   **SHIPPED (2026-06-29) as `vm.serve`** — and the parity invariant *forced the right
   shape*. A truly-racing async channel (`vm.spawn(...).await` with the two VMs
   interleaving freely) is **fundamentally parity-incompatible**: nondeterministic
   message interleaving cannot be reproduced by the single-threaded interpreter oracle,
   so it could never agree bit-for-bit. The deterministic, parity-safe realization is a
   **lock-step stateful service**: `vm.serve(init, requests, handler)` runs a service on
   one long-lived isolated worker VM, processing the request stream IN ORDER and
   threading `state` through `handler(state, request) -> new_state`. The interpreter runs
   it as a sequential scan; the compiled backend runs it as a persistent worker VM; both
   produce identical responses. So lock-step serving is not a compromise — it is the
   correct cross-VM-channel shape for a parity-preserving language. (A free-racing
   `vm.spawn` is therefore a deliberate NON-goal, not unfinished work.) Test:
   `vm_serve_stateful_service_agrees`. As with `with_dir`, `handler` must be a bare
   top-level function; the shared checker/codegen contract rejects closures and local
   aliases rather than weakening isolation.
2. **True multi-core — SHIPPED** as the `vm.par_map` backend (OS-thread child VMs;
   instances are `Send`). Parity-neutral, because results are collected by input index:
   a pure function over a list gives the same answer sequentially or in parallel, so the
   parallel run agrees with the interpreter's sequential one. (In-VM wasm-threads over a
   *shared* heap — the finer-grained Option C — remains research-grade and unbuilt; it is
   not needed for the data-parallel and service workloads `par_map`/`serve` cover.)

All of RFC-0032's shippable surface is now implemented and parity-safe: the Tier-C
cooperative ladder + cancellation; true multi-core `vm.par_map` (scalars/String/Bytes);
Tier-B zero-authority isolation; capability-passing (`vm.with_dir`); and cross-VM
channels (`vm.serve`). The only deliberately-excluded pieces are free-racing async
(parity-incompatible by construction) and in-VM shared-heap threads (research-grade,
unneeded for the covered workloads).
