---
rfc: 0059
title: State-machine async — frames, an owning executor, and ring channels
status: proposed
created: 2026-07-04
tracking:
---

# RFC-0059: State-machine async — frames, an owning executor, and ring channels

## Summary

Replace the CPS-over-closures async lowering with a defunctionalized
state-machine lowering, rewrite the executor to own its state in place, and
give channels a fixed-capacity ring buffer. Targets, in witchy's own terms:
steady-state message passing allocates nothing, per-message cost drops from
microseconds to hundreds of nanoseconds, the memory ceiling disappears, and
`await` becomes fully expressive (`await` in `while`, `var` across `await`,
folding in `for await`). This is the throughput program that sits on top of
RFC-0036 (which bounds memory but explicitly does not close the constant
factor).

## Motivation

The concurrency substrate (`chan.spawn`, channels, `select`, `for await`) is a
flagship feature. Today it has a hard functional ceiling and a per-message
cost three orders of magnitude above what the operation intrinsically
requires.

**Measured 2026-07-04** (kernel clock via `now_monotonic`, best of runs,
`witchy sandbox` compiled tier, producer/consumer over a 64-slot buffered
channel — the `benchmarks/chan_throughput` pair):

| N (messages) | best | per message |
|---|---|---|
| 500 | 1.37 ms | 2.7 µs |
| 1,000 | 2.85 ms | 2.9 µs |
| 2,000 | 10.2 ms | 5.1 µs |
| 4,000 | 33.6 ms | 8.4 µs |
| 8,000 | 116 ms | 14.5 µs |

Two distinct defects, and neither is a "tier contract" item:

- **A large constant factor.** Moving one `Int` through a buffered channel is
  intrinsically a handful of stores plus ring-index arithmetic — an operation
  in the tens-of-nanoseconds class, which the benchmark pair's twin
  (`chan_throughput.go`, ~24 ns/message on the same machine) confirms is
  achievable for this workload. witchy spends 2.7 µs even at small N: the cost
  is going somewhere other than the work.
- **Superlinear degradation**: per-message cost doubles as N doubles, ending
  in an OOM trap at N ≈ 9,000. The benchmark file itself documents the cap. A
  channel abstraction with a hard ceiling of a few thousand messages is a
  functional defect, not a slow path.

### Diagnosis — three compounding causes

**D1 — closure per await.** `await E` lowers to `task.and_then(E, fn(x): rest)`
(`crates/witchy-syntax/src/async_lower.rs`): every await point allocates a
closure capturing the live locals, per execution.

**D2 — tower rebuild per poll.** `and_then_step` re-wraps every non-`Done`
step in a *fresh* wrapper closure (`std/task.witchy:78-89` — every match arm
allocates `fn(h): and_then(cont(h), k)`). A continuation chain k awaits deep
allocates O(k) new closures on *every scheduler step*, and the previous tower
becomes garbage. Cost per message is proportional to continuation depth, and
garbage production is quadratic in progress.

**D3 — the executor owns nothing.** `step_one` threads `slots`/`channels` as
borrowed params rebuilt per step (`std/task.witchy:208-263`, duplicated in
`std/chan.witchy:416-471`); the D2 garbage is never reclaimed (shell-only
drop — recursive `$rdrop` is not implemented; see RFC-0036), so the arena
grows monotonically and per-message cost grows with the accumulated heap until
the trap.

### The lowering is also expressively incomplete

`async_lower.rs` documents its own restrictions: `await` inside `while` is
unsupported, a mutable `var` cannot cross an `await`, and
`benchmarks/chan_throughput.witchy` cannot even fold its stream into a sum
("for-await body captures by value") — it drains instead. These are not
incidental: they are direct consequences of representing continuations as
capture-by-value closures. The state-machine representation removes them as a
side effect rather than as extra work.

## Design

### Stage 0 — land RFC-0036 Design B + recursive `$rdrop` (prerequisite)

Already specified there; this RFC elevates its priority and adds one
requirement: it must cover **both** executor copies (`std/task.witchy` and
`std/chan.witchy`) or be sequenced after their dedup. Exit criterion: flat
per-message cost and bounded heap high-water independent of N (expected ~2–3 µs
per message, flat — removing D3 but not D1/D2).

### Stage 1 — state-machine async lowering (the core)

Compile each `async fn f(args) -> T` to:

- a **frame record** `__F_f` whose fields are the arguments, every local that
  is live across an await point, and a `state: Int`;
- a **step function** `fn __f_step(var frame: __F_f) -> Step` that dispatches
  on `frame.state` via `match`, runs straight-line code to the next await,
  stores the live locals back into the frame, sets the next state, and returns
  the suspension `Step`.

Key properties:

- **`Task(a)` becomes frame + step-fn** instead of a closure tree. The
  continuation *is* the frame; `and_then` towers (D1, D2) cease to exist.
  Nested async calls hold an explicit `parent: Option(Frame)` link — a
  defunctionalized call stack, in ordinary witchy values.
- **Frontend-only transform.** Like today's lowering, typeck/codegen/the
  interpreter never see `async`/`await` — both backends receive ordinary
  records and `match`. Parity by construction is preserved; no new WIR nodes,
  no new runtime helpers, and **no per-method special cases** (CLAUDE.md rule):
  frames are ordinary records, so the *existing general* uniqueness/in-place
  machinery applies to them with no executor-specific paths.
- **Frame reuse in place.** A resumed task mutates its own frame — unique by
  construction, since only the scheduler holds it. This is exactly the
  `var`-receiver shape RFC-0043 declares; steady-state message traffic
  allocates nothing.
- **Expressiveness falls out**: loops become states, so `await` in `while`
  works; `var` locals become frame fields, so they cross awaits; `for await`
  can fold.
- **`Step` slims down**: suspension arms carry (channel id, frame) instead of
  closures. The `Step`/`Task` public shape changes; break-don't-deprecate
  applies (std/task, std/chan, book concurrency chapter, examples migrate in
  the same cut).
- **Closures captured across await**: a closure value stored in a live local is
  boxed into the frame like any other value (closures are values). No `Pin` is
  needed anywhere — frames are plain values and moves are copies, which is a
  structural simplification the value-semantics model buys outright.
- **`gen`/`yield`** (planned, not yet RFC'd) must share this machinery — one
  state-machine transform for both suspension features, not two.

Expected effect: removes D1 and D2; per-message cost becomes a small constant
number of in-place record writes + one ring operation.

### Stage 2 — owning executor + ring channels

- Scheduler state as `var` locals mutated in place (the Design B shape carried
  to its conclusion): a slot table indexed by task id, a run queue as a ring.
- A channel becomes a **fixed-capacity ring buffer**: a `List` used as a ring
  with `head`/`tail` ints, mutated through the general in-place `set_at` path —
  no new `*_cap` helpers, per the one rule.
- **The deterministic schedule policy does not change.** Round-robin order and
  interleavings stay byte-identical to today (`std/chan.witchy`'s determinism
  contract), differential-tested. Determinism is a feature we keep; we are
  removing allocation, not reordering execution.

### Stage 3 — parallelism (explicitly out of scope; future RFC)

The executor is deterministic and single-threaded by contract; running tasks
across cores is a separate design. witchy's value semantics + capability model
mean a parallel executor has no data races *by construction* — messages are
owned values, and there is no shared mutable state to race on. A worker-VM
channel fabric (host-mediated ring buffers between VMs, building on the
RFC-0032 pool machinery) is the likely shape, but it is not needed to fix the
per-message cost and it interacts with the RFC-0005 externref plan
(capabilities must not cross Stores). Listed so the roadmap is visible; not
part of this RFC's definition of done.

## Definition of done (falsifiable, kernel-clock, RFC-0058 discipline)

1. `benchmarks/chan_throughput` un-capped: **N = 1,000,000** completes on both
   backends with heap high-water independent of N.
2. Per-message cost **≤ 300 ns** at N = 1M on the compiled tier (stretch goal
   ≤ 100 ns). Kernel-timed, adopted into `kernel_only.sh` with the benchmark
   pair kept current via the `bench_ns` pattern used by every other pair.
3. `src/stats.rs::chan_throughput_bounded_by_rc_floor` un-`#[ignore]`d and
   green.
4. New benchmark pairs: `select` fan-in, and spawn-heavy (10,000 tasks).
5. Await-expressiveness examples (`await` in `while`, `var` across `await`,
   folding `for await`) become executed `book/` tests; the differential fuzz
   grammar grows an async arm; the full oracle sweep and interleaving
   determinism tests stay green.
6. The old CPS lowering and the closure-tower `Step` arms are **deleted**, not
   kept as a fallback (break-don't-deprecate; two lowerings would be a
   permanent parity liability).

## Alternatives

- **Keep CPS, optimize it** (closure reuse, tail calls): wasm tail calls are
  disabled by the RFC-0005 step-7 engine lockdown, and no amount of reuse fixes
  D2's tower rebuild — the representation is the defect. Rejected.
- **Host-native executor in Rust**: fastest possible, but it moves scheduler
  semantics out of witchy into two independent implementations (interpreter
  and runtime), creating a permanent divergence surface in the subsystem where
  interleaving subtleties are hardest to test. Rejected while the in-language
  design can reach the target; revisit only if Stage 2 misses it by an order
  of magnitude.
- **wasm stack-switching proposal**: not stable in wasmtime 45, and would also
  reopen the RFC-0005 feature-lockdown decision. Revisit at Stage 3.
- **Do nothing (tier contract)**: the OOM ceiling alone makes the current
  state indefensible for a flagship feature, and the cost model makes
  channel-heavy program shapes impractical in witchy today.

## Drawbacks

- The `async_lower.rs` rewrite is the largest frontend transform to date;
  liveness across await points must be computed correctly or frames carry too
  much (perf) or too little (miscompile). Mitigation: the transform is pure
  witchy-AST → witchy-AST, so every existing differential/fuzz gate applies to
  its output; add an async arm to the fuzz grammar in the same change.
- Public `Task`/`Step` API breaks; all channel-using code migrates in one cut.
  The surface is small (std/task, std/chan, one book chapter, a handful of
  examples and benchmarks).
- Frames make suspension state visible to the optimizer as ordinary records —
  good for the general machinery, but it means Stage 1's win depends on the
  in-place path applying to frame updates. If the uniqueness analysis fails to
  prove frame uniqueness anywhere, that arm silently falls back to copying;
  the DoD's flat-memory + ns targets are the guard against shipping that
  regression unnoticed.
- Sequencing: RFC-0043's `var`-receiver machinery (in flight) and RFC-0046's
  table-backed dispatch are the substrate the frame records lean on; Stage 1
  should land after both.

## Prior art

- **Rust async**: the state-machine transform this adopts — minus `Pin`,
  which witchy does not need (frames are values; moving is copying).
- **Kotlin coroutines**: CPS *specified*, state-machine *implemented* —
  evidence that the observable semantics of today's CPS lowering can be kept
  while the representation changes underneath.
- **Go runtime channels**: ring buffer + waiter queue (`sudog`) — the closest
  production design to Stage 2's channel shape.
- **Defunctionalization** (Reynolds): the general technique for turning
  closure towers into data + dispatch.
- RFC-0036 (memory floor this builds on), RFC-0016/0035 (reclamation),
  RFC-0032 (the pool machinery Stage 3 would extend).
