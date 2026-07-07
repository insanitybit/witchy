---
rfc: 0059
title: State-machine async — frames, an owning executor, and ring channels
status: planned
created: 2026-07-04
tracking: >
  Increment 1 SHIPPED: defunctionalized state-machine lowering — closure tower gone,
  await-in-while/var-across-await/folding-for-await work, chan_throughput folds + 4x
  past the OOM cliff, parity+determinism green. Increment-2 STEP 1 SHIPPED:
  fixed-capacity ring channels (send = in-place set_at, recv = advance head, no
  list.tail rebuild), determinism byte-identical, all concurrency parity+heap gates
  green. Increment-2 STEP 2 (scalar SoA frames = eliminate the CPS closure churn)
  REMAINS — its flat target is proven in-tree
  (chan_throughput_scalar_soa_reference_is_flat: 13 live cells flat to N=20k, ~11
  ns/msg flat to N=1M, both backends), re-scoped 2026-07-05 to a whole-program
  scalar-executor synthesis bounded to all-scalar programs (a multi-session
  transform); see the 2026-07-05 "Increment-2 STEP 2" note for the three blockers +
  staged plan.
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

## Implementation note (2026-07-05) — Stage-1 spike: the boxed-frame model does NOT reach the flat DoD; the scalar SoA + ring target is PROVEN

A measurement-first spike (RFC-0058 discipline) built four hand-written
representations of the `chan_throughput` producer/consumer and measured them on the
**compiled** backend with reclamation on (`witchy stats`, `__witchy_live_cells`),
plus the flat one kernel-timed at N=1,000,000. Both backends were checked to agree
(`witchy parity`). This retires the RFC's central risk ("Stage 1's win depends on
the in-place path applying to frame updates") — and it disproves a load-bearing
premise of the Design section.

### What was measured (compiled tier, reclamation on)

| Representation | live_cells scaling | notes |
|---|---|---|
| **Current CPS** (RFC-0036 Design B, the real `async` lowering) | ~94 / message (18608 @ 200; 37490 @ 400; 75652 @ 800) | OOM ~10k; the closure towers |
| State-machine **boxed sum-type frame**, reconstructed per step | ~9 / message (1819 @ 200; 180019 @ 20000) | 10× better + folds; **still leaks** |
| **Boxed record** frame, "reused in place" (`var f = at(xs,0); f.x = …; set_at`) | ~1 / message (204 @ 200; 20004 @ 20000) | read-out/write-back forces a copy; the displaced record leaks |
| Scalar `set_at` on a fixed-size list | **FLAT** (3, constant) | RFC-0035's reclaimed shape — scalar element only |
| `list.push` + `list.tail` (growable buffer = today's channel) | ~1 / message (203 @ 200; 2003 @ 2000) | why channels must become rings |
| **Scalar SoA frames + fixed-cap ring channel** (the reference target) | **FLAT** (13, constant N=200 … 1,000,000) | **10 ns/message** kernel-timed @ N=1M; both backends agree |

The producer/consumer reference also **folds** the received stream into a sum
(`sum += v`) and returns the correct `499999500000` at N=1M — i.e. the
folding-`for await` expressiveness the current lowering cannot express falls out of
the state-machine shape, as the RFC predicted.

### The corrected finding

- **Boxed frame records do NOT become flat.** The RFC's Design says Stage 1 alone
  makes "per-message cost a small constant number of in-place record writes" and
  "steady-state message traffic allocates nothing." Measurement contradicts this for
  the *boxed* frame: whether a frame is a reconstructed sum value (~9/msg) or a
  record "reused in place" (~1/msg), the displaced continuation is a **heap child of
  the `Slot`**, and **shell-only drop cannot reclaim it** (recursive `$rdrop` is not
  implemented — RFC-0036's open item, blocked on the per-capture move/borrow oracle).
  This is the **same wall** RFC-0036 hit; changing closures → boxed frames slides the
  leak from ~94 to ~1–9 cells/message (a real 10–90× win, and it moves the OOM
  ceiling out ~10×) but does **not** remove it.
- **Flat requires an all-scalar representation.** The only reclaimed-to-flat
  primitives measured are (a) scalar `set_at` on a fixed-size list and (b) scalar
  ring indices. So the flat + ns DoD is reachable only when **every** per-task datum
  — frame fields AND channel payloads — is a scalar column mutated by `set_at`, i.e.
  **frames become Struct-of-Arrays scalar columns** (the defunctionalized state is an
  `Int` per column, not a boxed record) **and channels become fixed-capacity ring
  buffers** (Stage 2 is not optional for the DoD — the `push`/`tail` buffer leaks
  ~1/msg on its own). The reference executor that does exactly this is flat at 13
  cells to N=1M and costs 10 ns/message — beating the DoD's ≤300 ns target and its
  ≤100 ns stretch, in the same class as the RFC's Go twin (~24 ns).
- **Scalar-only.** The flat result holds for **scalar** message/local types (the
  `Int` benchmark, the DoD test). A `String`/`List` carried across an `await` or sent
  as a message is a boxed frame field / boxed `__Msg`, which reintroduces the shell-
  drop leak and still needs recursive `$rdrop`. The DoD programs are all scalar, so
  the scalar path satisfies them; the general path degrades to "10× but not flat"
  until recursive drop lands.

### Consequence for the plan (what Stage 1 should actually be)

The RFC's Stage 1 / Stage 2 split should be read as: **the frame transform and the
ring channel are BOTH prerequisites of the flat DoD, and the frame representation
must be scalar-columnar, not a boxed record.** Two landable increments, in order:

1. **`async_lower.rs` → single-step state machine, executor UNCHANGED (low risk,
   ~10× + expressiveness, no flat).** Keep the existing `Step`/`Task` shape and the
   `std/task`/`std/chan` executor exactly as they are; only change what `async_lower`
   emits: instead of nested `task.and_then(inner, fn(x): rest)` (the tower), compile
   each `async fn` to a frame record + a step function, and emit each suspension's
   continuation as a **single** closure `fn(x): __f_step(inject(frame, x))` (captures
   the frame — one pointer — never a tower). `and_then` leaves the hot path;
   `and_then_step`'s per-poll re-wrap (D2) is gone. This needs NO executor/Step/std
   change (parity by construction — the executor still sees closures), so it is
   incrementally committable behind the full oracle/fuzz/determinism gate, and it is
   the substrate for step 2. Expressiveness (`await` in `while`, `var` across
   `await`, folding `for await`) is authored here because the state machine, not the
   closure, now carries the live locals. Measured ceiling for this step: ~9/msg
   (proto), i.e. the 10× win and the OOM ceiling out to ~90k, **not** flat.
2. **Scalar SoA frames + ring channels (the flat DoD).** Lower the frame record to
   scalar columns (an "unbox the frame into `Int` columns indexed by task id" codegen
   shape — a generalization of the RFC-0027 `unbox` layout to the executor's slot
   table) and replace the channel buffer with a fixed-capacity ring (`List` + `head`/
   `tail`/`count` scalars, `set_at`-mutated; a full ring grows amortized for the
   `cap == 0` unbounded case). This is where the flat + 10 ns result lands, and it
   changes the executor + `Step`/`Slot` shape (break-don't-deprecate: migrate
   `std/task`, `std/chan`, book, examples in one cut). For heap payloads it falls back
   to boxed frames (leaky-but-safe) until recursive `$rdrop` (RFC-0036) exists — so
   the DoD's flat gate is met for the scalar benchmark/test, and the general
   flat guarantee is explicitly sequenced after the move/borrow oracle.

**DoD reachability, honestly stated.** DoD items 1–3 (flat @ 1M, ≤300 ns, un-ignore
`chan_throughput_bounded_by_rc_floor` < 500) require step 2 (scalar SoA + ring) — a
hand-written reference proves they are reachable (13 cells flat, 10 ns/msg). DoD
item 5 (the three expressiveness examples) requires step 1. DoD item 6 (delete the
CPS lowering + closure-tower `Step` arms) is completed by step 2 (step 1 keeps the
closure `Step` arms as the executor interface; step 2 replaces them with scalar
`(channel-id, task-id)` arms). The prior deferral note in RFC-0036 stands: the boxed
general path still needs recursive `$rdrop`, which still needs the per-capture
move/borrow oracle — so the *general* (heap-payload) flat guarantee is not in this
Stage's scope, only the scalar DoD.

**Spike status.** No transform code landed this session — the spike's job was to
qualify the representation *before* the (multi-day) `async_lower` rewrite, and it
found the RFC's stated boxed-frame Stage 1 cannot meet its own flat DoD. The four
reference programs (proto = boxed sum frames; proto3 = boxed record reuse; proto4/5
= scalar/`push`-`tail` isolation; proto6 = flat scalar SoA + ring) are the evidence;
their numbers are inlined above so the finding is self-contained. Next session
starts at step 1 (the low-risk `async_lower` state machine), with step 2 as the flat
follow-on whose target is already proven.

## Implementation note (2026-07-05) — Stage-1 step 1 LANDED: defunctionalized segment-function lowering

Step 1 (the low-risk `async_lower` state machine, executor UNCHANGED) is
implemented. The chosen representation is a **defunctionalized set of segment
functions** rather than a single boxed *record* + `match frame.state` dispatcher.
The two are isomorphic — a segment function's parameter list *is* the frame's live
columns, and the function identity *is* the `state` tag — but the segment-function
form is the one the pre-typeck transform can actually emit soundly. Four
measurement probes (RFC-0058 discipline) established why:

- **Pre-typeck types are unknown, and a boxed record forces them into the open.**
  A `type Frame__f(...)` needs a field type for every carried local. Args carry
  their declared type, but a resume-bound local (`let x = E.await`) and a
  `for`/`for await` loop variable have inference-only types this pass never sees.
  A single generic dispatcher `run__f(frame: Frame__f(a))` then type-checks its
  body *generically*, so `frame.i < frame.n` / `v * v` fail the `Ord`/`Mul` bounds
  the concrete program would satisfy — the "trait op on an un-annotated generic"
  wall. (Measured: the record producer needs `Frame__f(Int)` spelled out to
  compile; the transform cannot spell it.)
- **The segment-function form keeps every value at an inference site.** A loop
  variable / recv result stays a **lambda parameter** of the `and_then`
  continuation (`fn(o): match o { Some(v) -> ... }`), so `v` is typed from the
  channel exactly as today's `consume` lowering types it — `v * v` and `sum + v`
  keep working. A resume-bound local that must cross a *further* await is passed
  **forward as a bound parameter** of the next segment (never a `None`
  placeholder), so no `Option`-wrapping and no bottom-value reader are needed.
  Carried counters/accumulators are annotated where derivable (a `for i in lo..hi`
  counter is `Int`; a `var acc = <literal>` takes the literal's type) and left to
  inference otherwise.
- **The tower is gone by construction.** Each suspension emits exactly one shallow
  closure `fn(x): __async_f_N(carried…, x)` capturing only the live locals and
  tail-calling a *named* segment — never a nested `and_then` tree. The active
  `and_then` depth is therefore bounded by the async-call-nesting depth, not by
  awaits-per-body or loop iterations, so `and_then_step`'s per-poll re-wrap (D2) is
  O(1) per async frame instead of O(depth). `for`/`for await`/`while` lower to a
  **recursive segment function** that threads its counter/accumulator through
  parameters and iterates a list by index (no `list.tail` O(n²) rebuild), which is
  what makes `await` in `while`, `var` across `await`, and folding `for await`
  fall out.
- **Interleaving is preserved for the linear/`let`/tail/`if`/`match` shapes.** The
  emitted structure is `and_then(E, fn(x): seg(...))` — identical to today's
  `and_then(E, fn(x): K)` except `K` is a named segment call instead of an inlined
  nested lambda — and the segment runs its straight-line code *eagerly* at
  `Yield(k(v))` just as the inlined `K` did, so the `Step` sequence (and thus the
  round-robin interleaving) is byte-identical there. Loop shapes move off
  `for_each`/`consume` onto the indexed/threaded recursion, so their interleaving
  changes deterministically; the book output manifest (`book/examples.json`) is
  re-blessed and both backends still agree (the parity gate is the correctness
  proof, re-bless the sanctioned snapshot update).

`Step`/`Task`/`Slot` and the `std/task` + `std/chan` executors are **unchanged**;
the segment closures plug into the existing `and_then`/`Step` machinery. Increment
2 (scalar SoA columns + ring channel) reifies each segment's parameter list as the
per-task scalar columns and is where the flat DoD lands.

## Implementation note (2026-07-05) — Increment-2 STEP 1 LANDED: fixed-capacity ring channels; and the CORRECTED finding that the ring is NOT where the leak is

Step 1 of increment 2 — **fixed-capacity ring channels** — is implemented in the
executor. Originally that meant both executor copies (`std/task.witchy`,
`std/chan.witchy`); after the 2026-07-07 dedup, the implementation lives only in
`std/task` and `std/chan` delegates `run` there. A channel's state changed from
the growable `(buf, cap)` (mutated by `list.push` on send / `list.tail` on recv) to a
**fixed-capacity ring** `(buf, head, count, cap)`: `buf` is a physical list of `physcap`
slots, the `count` live messages are `buf[(head + i) % physcap]` for `i in 0..count` in
FIFO order, `cap` is the logical capacity (`0` = unbounded). **Send** writes the tail slot
with `list.set_at` **in place** — no allocation, no growth; **recv** just advances `head`
— so the `O(occupancy)` `list.tail` rebuild the growable buffer did on *every* recv is
gone. Unbounded channels (`cap == 0`) start as a 1-slot ring and grow amortized (`ring_grow`
relayouts the live elements FIFO into a doubled buffer). FIFO order and the ready
(`count > 0`) / room (`count < cap`) predicates are byte-identical to the old length-based
ones, so the **deterministic round-robin schedule and every interleaving are unchanged** —
verified green: `future_executor_interleaves_backends_agree`, all `chan_*`/`async_*`/
`for_await_*`/`vm_par_map`/`vm_serve` parity tests, `every_compilable_example_agrees_on_both_backends`,
`every_example_agrees_under_rc_floor`/`unbox`, the full `rc_corpus` + `rc_floor` heap-safety
corpus, `clippy -D warnings`, `witchy fmt`, `stdlib_docs_are_current`.

### The corrected finding (measurement-first, RFC-0058) — the ring gives ZERO leak reduction here

The spike (note above) modelled the growable buffer in isolation as leaking `~1/msg` and
listed the ring as a prerequisite of flat. Measured against the **real** executor with
rc-floor on, that `~1/msg` was already being reclaimed, so the ring does **not** move the
per-message leak at all. `chan_throughput` (producer while-loop + folding `for await`,
`witchy stats` / `__witchy_live_cells`, compiled tier, rc-floor):

| N | live_cells (cap 8) before ring | live_cells (cap 8) after ring | heap high-water after (cap 64) |
|---|---|---|---|
| 200 | 9112 | 9113 | 9716 cells / 455 KB |
| 400 | 18112 | 18113 | 19316 / 906 KB |
| 800 | 36112 | 36113 | 38516 / 1.81 MB |
| 1600 | 72112 | 72113 | 76916 / 3.61 MB |
| 64000 | — | — | 3,072,116 / 144 MB |
| 1,000,000 | — | — | **OOM trap** (out-of-bounds memory access) |

Slope ≈ **45 cells/message (cap 8), ~48 (cap 64)**, FLAT per message but **LINEAR in N** —
identical before and after the ring (the cap-64 pre-allocated buffer adds a one-time
`O(cap)` constant, correctly, matching Go's `make(chan int, cap)`). Kernel-timed (compiled
`witchy sandbox`, best of 5, full executor run incl. setup/teardown): **N=200 ≈ 559 ns/msg,
N=1000 ≈ 496, N=64000 ≈ 519** — i.e. per-message cost stays ~500 ns and does not fall with
N, above the `≤ 300 ns` DoD, because the linear leak keeps allocation pressure up. N=1M
OOM-traps (~48M cells ≈ 2.3 GB).

**Conclusion, falsified against measurement:** the entire per-message leak is
closure/`Task`/`Step` churn from the **CPS-over-closures executor INTERFACE** — the segment
continuation `fn(x): __seg(carried, x)`, its `Task`/`Step`/`and_then` wrappers, and the
erased `__Msg` — none of which shell-only drop can reclaim (their heap children survive the
shell free). The round-robin schedule keeps buffer occupancy at ~1, so the channel
representation is simply not on the leak path for this workload. **The ring is correct,
mandated substrate and a real throughput win at occupancy > 1, but it is neither necessary
nor sufficient for the flat DoD on `chan_throughput`.** DoD items 1–3 (flat @ 1M, ≤300 ns,
un-ignore `chan_throughput_bounded_by_rc_floor` < 500) all reduce to a single remaining
task: **eliminate the per-resume closure allocation.**

### Handoff — Increment-2 STEP 2 (scalar SoA frames), the sole flat-DoD blocker

The reference spike (13 cells FLAT to N=1M, 10 ns/msg, both backends) is program-specific;
generalising it is the remaining work, and it is genuinely the largest transform in this
RFC — it changes the executor protocol, so it must land as ONE cut across `async_lower.rs`,
both executors, every `std/chan` combinator, the book chapter, and the concurrency examples,
under the full parity + interleaving-determinism + heap-check gate. Concretely:

1. **Defunctionalize the continuation to data, not a closure.** Today `async_lower` emits
   `task.and_then(inner, fn(x): __seg(carried…, x))` — the closure IS the boxed frame.
   Replace it with a `(seg-id, task-id)` pair: each lifted segment function gets a global
   integer `seg-id`, and the executor resumes a task by dispatching on `seg-id` through a
   **whole-program generated `step` dispatcher** (`match seg_id: 0 -> __seg0(cols…); …`)
   rather than calling a stored closure. This is Reynolds defunctionalization carried one
   step past increment 1 (which already named the segments; step 2 removes the closure that
   *calls* them).
2. **Reify `carried` as scalar columns indexed by task id.** The executor's slot table holds
   a fixed-width table of `Int` columns (`col0…colK`, `K` = max frame width across all
   segments) plus a `seg-id` column, instead of `Active(Task(closure))`. A resumed segment
   reads its columns from the task's row, runs straight-line code, writes columns back with
   scalar `list.set_at` (the RFC-0035 reclaimed-to-flat shape — measured flat in the spike),
   and returns the next effect + next `seg-id`. No `Task`, no `Step` closure, no `and_then`
   — nothing allocates per resume. `Step` collapses to scalar arms `(channel-id, task-id)`
   (DoD item 6: delete the closure-tower `Step` arms) carried in the same columns.
3. **Scalar-only, documented.** Every column is `Int` (the `Sender(Int)`/`Receiver(Int)`
   endpoints are 1-field Int wrappers, `unbox`-able to their inner id). A `String`/`List`
   frame field or message reintroduces a boxed child and still needs recursive `$rdrop`
   (RFC-0036) — out of scope; the DoD programs are all scalar, so the scalar path satisfies
   them, and the general path stays "10× but not flat" until the move/borrow oracle lands.
4. **Migrate the surface in the same cut** (break-don't-deprecate): `std/task` + `std/chan`
   executors, every combinator (`spawn`/`join`/`race`/`gather`/`par_map`/`select`/`serve`/
   `consume`), `book/src/tour-async.md`, and the eight concurrency examples. The parity +
   `future_executor_interleaves_backends_agree` gates are the correctness proof that the
   scalar schedule stays byte-identical.

The ring representation from step 1 is the channel half of step 2's flat target; step 2's
channel state becomes scalar SoA columns (`chan_head`/`chan_count`/`chan_cap` as parallel
`Int` lists + a `chan_bufs` list) mutated by scalar `set_at`, removing even the per-op
`(buf, head, count, cap)` tuple (the last ~4/msg of channel-side churn). The buffer-element
write into the nested `chan_bufs[ch]` is the one subtle uniqueness site (nested-container
in-place mutation) to get right there.

## Implementation note (2026-07-05) — Increment-2 STEP 2: the flat TARGET is now proven in-tree, and the transform is re-scoped as a whole-program scalar-executor SYNTHESIS (three architectural blockers the prior handoff under-specified)

A step-2 attempt began with the prior handoff's framing ("one focused executor-representation
change"). Building out the surface established that the framing is wrong in one specific way:
the flat DoD is reachable, but **not** by editing the existing closure executor in place. The
change is a *whole-program scalar-executor synthesis*, bounded to all-scalar programs, and it
is a multi-session transform, not a single focused edit. Three concrete blockers, each
evidenced against the current tree, and the corrected staged plan follow. Nothing that changes
the closure executor / `async_lower` protocol landed this session (an atomic half-cut would red
every concurrency example); what landed is the durable proof + this re-scope.

### What landed this session (additive, verified, green)

- **The flat TARGET is now a re-runnable in-tree fact, not ephemeral scratch prose.**
  `src/stats.rs::chan_throughput_scalar_soa_reference_is_flat` compiles+runs the all-scalar
  producer/consumer kernel (scalar `Int` columns `f0=[np,sum]`/`f1=[i,seen]`/`status` +
  fixed-cap ring, no closures/`Task`/`Step`) under rc-floor and asserts **live_cells = 13,
  IDENTICAL at N=200 and N=20000** (FLAT, independent of N) — the executable form of the spike's
  "13 cells flat". Kernel-timed on the compiled tier (`now_monotonic`, min of 8, both backends
  agree via `witchy parity`): **11.8 ns/msg @ N=1k, 11.0 @ 64k, 11.1 @ 1M** — flat, under the
  ≤300 ns DoD and the ≤100 ns stretch. So the target representation is confirmed reachable and
  guarded against codegen regression; the remaining work is purely *making the transform emit
  it for the real `async` source*.
- **Baseline is green** (retained closure path): the 9 concurrency parity + interleaving-
  determinism gates (`future_executor_interleaves_backends_agree`, `async_*`/`for_await_*`/
  `chan_producer_consumer`/`rc_corpus_channel_executor_is_stable`/`async_method_in_impl`/
  `rfc0055_two_modules_*`) pass. The `#[ignore]`d DoD test `chan_throughput_bounded_by_rc_floor`
  still measures **18413 live cells at N=200** (drain form, cap 8, via `--run-ignored`) — i.e.
  the transform is genuinely required; there is no shortcut to <500.

### Blocker 1 — a std executor cannot reach a program-generated dispatcher (no global mutable ⇒ the columns can only travel by capture or by upcall)

The handoff says "the executor resumes a task by dispatching on seg-id through a whole-program
generated `step` dispatcher rather than calling a stored closure." But that dispatcher
(`match seg_id: 0 -> __seg0(cols…); …`) references the program's lifted segment functions, so
it lives in the **user module**, while `run` lives in **`std/task`**. witchy has **no global
mutable state**, so a resumed segment can receive its carried columns by exactly two routes:
(a) capture them in a closure — the leak we are removing — or (b) have the executor pass them
in. Route (b) means `std/task::run` must **call the user-module dispatcher**, an upward
`std → user` call that the linker does not provide. Therefore step 2 must FIRST pick an
architecture, and the honest options are narrow:

- **Recommended: `async_lower` SYNTHESIZES the specialized scalar scheduler into the program**
  (replacing the `task.run(lazy_body)` it already emits for an async `main`) when the program
  qualifies (Blocker 3), so the scheduler and the `match seg_id` dispatcher are co-generated in
  one module and no upcall is needed. This keeps `std/task`/`std/chan`'s human-readable closure
  executor as the fallback for non-qualifying programs — which is not "two lowerings" in the
  forbidden sense but the `unbox`-style representation choice (one mechanism, specialized on a
  proven fact; see Blocker 3).
- Alternative: add a linker `std → user` upcall (a well-known generated symbol the std executor
  imports). Rejected as first choice: it puts a program-shaped hole in a std module and widens
  the parity surface (the executor's control flow now depends on generated code) for no gain
  over synthesis.

### Blocker 2 — the closure combinators are generic + higher-order, so they cannot become scalar segments

`consume`/`serve`/`select`/`and_then`/`gather`/`par_map`/`race`/`race_n`/`scope`/`spawn_all`/
`recv_n`/`recv_each`/`par_build` are written with `and_then` over `Task(m)`/`fn` values and are
polymorphic in the message type. A `(seg-id, Int-column)` row cannot carry a `Task(m)` argument
or a user closure, so "migrate every `std/chan` combinator in the same cut" **as scalar
segments is not possible** — `request_reply` even calls `chan.and_then` in *user* source and
sends a `Sender(Msg)` *inside* a message. These combinators must keep boxed frames (closures),
which are "10× but not flat" (the RFC-0036 recursive-`$rdrop` path). **Flat is reachable only
for the all-scalar `async fn` shape**, never for the generic combinator surface. The DoD's item
6 ("delete the closure-tower `Step` arms") is therefore only achievable for the synthesized
scalar path; the closure `Step` stays as the combinator substrate until recursive `$rdrop`.

### Blocker 3 — the 8 concurrency examples carry NON-scalar state across `await`, so a scalar-only executor cannot run them

Every shipped concurrency example threads non-`Int` state through carried locals: `Console`
capabilities (`async_tasks`, `channels`, `for_await`, `select`, `worker_pool`), `String` names
(`async_tasks`), `Receiver`/`Sender` values (`select`, `worker_pool`, `request_reply`),
`Selected(m)` (`select`), `Msg` records with an embedded `Sender(Msg)` (`request_reply`), and
user closures passed to `consume`/`serve` (`channels`, `worker_pool`, `request_reply`). A scalar
`Int` slot-table **cannot represent any of these**, so the scalar executor is not a drop-in
replacement — replacing the closure executor with a scalar one wholesale breaks all 8 examples
(the exact atomic-half-cut failure to avoid). The resolution is the `unbox`-style choice:
`async_lower` emits the synthesized scalar scheduler **only when the whole reachable async
surface is provably all-`Int`** (frame columns and channel payloads), and emits today's closure
lowering otherwise. This is ONE mechanism specialized on a proven fact — like `unbox` picking a
flat buffer only for confined fixed-scalar records — not two hand-maintained async lowerings.

### Corrected staged plan (multi-session; each stage independently green)

1. **Qualification analysis** (frontend): decide, per async program, whether its entire
   reachable async surface is all-`Int` (frame live-across-`await` locals AND every channel's
   message type). Only the DoD benchmark/test qualify today; the 8 examples do not. This is a
   pure predicate over the already-lifted segments + endpoint types — testable in isolation.
2. **Synthesized scalar scheduler + `match seg_id` dispatcher** for qualifying programs
   (Blocker 1's recommended architecture): reify each segment's carried columns as scalar `Int`
   columns indexed by task id, the `Step` effect as scalar `(effect-tag, channel-id, next-seg-id)`
   arms, and the channel as the step-1 ring lowered to SoA columns (`chan_head`/`chan_count`/
   `chan_cap` + `chan_bufs`). This is where DoD items 1–3 land; the reference in
   `chan_throughput_scalar_soa_reference_is_flat` is the exact shape to emit, so it is a codegen
   target with a green oracle already in tree.
3. **Retain the closure executor** for non-qualifying programs (all 8 examples + the generic
   combinators, Blockers 2–3), unchanged, under the same parity + `future_executor_interleaves_
   backends_agree` gate. The general (non-scalar) flat guarantee stays deferred behind recursive
   `$rdrop` (RFC-0036) + the per-capture move/borrow oracle, exactly as previously scoped.

**DoD status after this session.** Item 2 (kernel-timed ≤300/≤100 ns) and the flat-memory half
of items 1/3 are PROVEN reachable in-tree (`chan_throughput_scalar_soa_reference_is_flat`,
11 ns/msg flat to N=1M). Items 1/3 for the *real async source*, and item 6 (delete CPS `Step`
arms) for the scalar path, require stage 2 above and remain open. Items 4–5 (examples,
determinism, oracle sweep) are green on the retained closure path and must stay so through stage
2 (they exercise the non-qualifying branch). The `#[ignore]` on `chan_throughput_bounded_by_rc_
floor` stays until stage 2 emits the scalar scheduler for its async source.
