---
rfc: 0055
title: Beyond one message type per program (design-first)
status: implemented
created: 2026-07-03
predecessors:
  - "concurrency-design.md (the executor this lifts a documented limit of)"
tracking: "option (b): erased messages, typed endpoints"
---

# RFC-0055: Beyond one message type per program (design-first)

> Design-first: this RFC decides the direction; implementation is a separate
> decision. No code ships from it.

## Summary

The deterministic pure-witchy executor is monomorphic over **one channel
message type per program** (`std/chan.witchy:8`;
`rfcs/concurrency-design.md:73-76` — "union into a sum type" is the documented
workaround). The consequence is an ecosystem cap: two libraries that each use
channels *internally* cannot coexist unless they agree on one program-wide
message union — no rune can use channels privately. This RFC examines the
actual constraint mechanism in the executor, works through three designs, and
recommends **(b) erased messages at the executor layer with typed channel
endpoints** — because the erasure is representationally free on both backends
(verified in the lowering), preserves byte-identical deterministic
interleavings, and is the only option that survives separately-compiled runes.

## Motivation: where the constraint actually lives

The executor (`std/task.witchy`, duplicated in `std/chan.witchy` — see the
cleanup note) is ordinary witchy: `run` threads
`slots: List(Slot(m))` and `channels: List((List(m), Int))` through a
deterministic round-robin (`step_round`/`step_one`/`try_push`/`try_pull`). The
message type `m` is a type parameter appearing in **every** layer: the channel
buffers (`List(m)`), the effect protocol (`Step(m, a)`'s
`Push(Int, m, cont)` / `Pull(Int, fn(Option(m)) -> Task(m, a))`), the parked
continuations (`Slot(m)`'s `WaitSend(Int, m, cont)`), and the task type itself
(`Task(m, a)`). One `run` instantiates the whole tower at one `m`; the async
lowering (`crates/witchy-syntax/src/async_lower.rs`) wraps `main`'s body in a
**single** `task.run`, so "per run" is "per program" in practice. (Probed: two
*separate* synchronous `chan.run` calls with different `m` do compile and run —
the monomorphization is per instantiation — but tasks in different runs cannot
interleave, so this is no escape hatch.)

The costs, concretely:

- **Libraries cannot use channels privately.** A rune whose internals pipeline
  work through a channel forces its `m` on the entire program; two such runes
  force a manual union neither controls. Under coven this caps composition the
  same way the flat type namespace does.
- **The workaround leaks plumbing.** The union type, injections at every send,
  and irrefutable-but-unchecked projections at every recv are user-visible
  boilerplate for a fact (this channel carries Jobs) the types already state.
- **The adjacent name collision compounds it** — `import iter` + `import chan`
  fails because both declare `Step` in the flat type namespace (probed: the
  error surfaces *inside* `iter.map_step` as a non-exhaustive match, because a
  duplicate type declaration silently last-wins and iter's match arms no longer
  match). RFC-0042 (module namespaces/imports) fixes that *naming* half; it
  does nothing about the executor monomorphism, which is a type-parameter
  fact, not a name fact. Both halves must land before two channel-using
  libraries coexist.

## Design space

### (a) Per-message-type executor instantiation — rejected

N executor instantiations, one per message type, interleaved by a meta-
scheduler. Examined against the executor: it fails structurally, not
incidentally. A *task* is `Task(m, a)` — monomorphic over one `m` — so any task
that touches channels of two types (the common shape: a worker pulling `Job`s
and pushing `Result`s) belongs to no single instantiation. Partitioning tasks
into typed islands breaks exactly the programs that motivate the feature, and
a meta-scheduler interleaving N `slots` lists deterministically would
re-implement the executor above itself. The typed `select`/mpmc surface
survives only within an island. Dead end.

### (b) Erased messages, typed endpoints — recommended

Make the executor's message slot **opaque**: `Step(a)`, `Slot`, and the
channel buffers hold an erased message value; the typed surface
(`Sender(m)`/`Receiver(m)`, `send`/`recv`/`select`) does the (un)wrapping at
the endpoint boundary via a compiler-blessed intrinsic pair
(`__erase(m) -> Msg` / `__unerase(Msg) -> m`), confined to `std/chan`.

**Why erasure is nearly free — verified in the lowering.** On the compiled
backend there is nothing to erase: *every* collection element, record field,
and closure argument is already an untyped 8-byte slot
(`spec/architecture.md:73` — `to_slot`/`from_slot`; the channel buffer is a
`List` of i64 slots today). `__erase`/`__unerase` lower to the identity. On the
interpreter, messages are `Value` enum instances — already uniform; the
intrinsics are likewise identity. concurrency-design.md's "type erasure, which
witchy doesn't have" is a statement about the *type system*, not the runtime —
the runtime has been erased all along. What this option adds is one
type-system-level opaque type whose only constructors live in `std/chan`.

**Soundness without runtime tags.** A message enters a buffer only through a
typed `Sender(m)` and leaves only through the paired `Receiver(m)` — the
channel id created by `channel(cap)` binds the two endpoints to the same `m`,
so every `__unerase` is guaranteed to see a value erased at the same type.
The obligation is exactly the pairing invariant, enforced today by
construction (ids are minted by `Open` and never cross channels). The
intrinsics are *not* exported; user code cannot forge an unerase.

**Determinism is untouched.** The scheduler consults message *presence*
(`list.length(buf)`) and never message *contents*; erasing the element type
changes no branch it takes. Both backends keep byte-identical interleavings —
the differential-testing asset is preserved, and the existing corpus plus the
examples sweep is the regression net.

**What it unlocks beyond the headline:** per-channel message types also
dissolve the second documented limit — `Wait`-based typed results
(`JoinHandle(T)`) become expressible as a private result channel per handle,
without the native-runtime trade concurrency-design.md rejected.

### (c) Compiler-synthesized program-wide union — the cheap automation, rejected as destination

Automate today's workaround: the compiler collects every channel element type,
generates the sum plus injections at `send` and projections at `recv`, and the
executor is *unchanged*. Genuinely attractive — zero runtime change, zero
type-system hole, determinism trivially preserved. Two costs decided against
it:

- **Phase order.** The element types of `channel(cap)` call sites are
  inference results; the linker (where whole-program synthesis would live,
  `crates/witchy-syntax/src/linker.rs`) runs *before* typeck. Synthesis needs
  a post-typeck desugar that invents a type and re-checks — a new compiler
  phase for a feature (b) gets by changing a type annotation in std.
- **Whole-program by construction.** The union's variant set is a global
  property; any future separately-compiled rune (coven's trajectory) breaks
  it — a rune cannot know the program's union at its own compile time. (b)
  has no global artifact: each endpoint pair is self-contained.

(c) remains the honest fallback if the confined erased type is judged an
unacceptable hole in the type system: it is strictly an automation of what
users already write by hand, and nothing in (b)'s surface precludes shipping
(c) first — the `Sender(m)`/`Receiver(m)` API is identical under both.

## Recommendation and acceptance

**Adopt (b).** Sequencing: after RFC-0042 (the name half; also the vehicle for
the cleanup note below), as its own increment. Acceptance tests:

1. Two independent modules, each with a *private* channel of a different
   message type (one `Int`, one record), compile and run together in one
   program — the test that is impossible today (probed:
   `expected `Msg`, found `String``).
2. `import chan` + `import iter` compile together (post-0042).
3. The determinism suite is unchanged: every existing channel example and the
   differential corpus produce byte-identical output on both backends, before
   and after — the erasure must be observationally invisible.

## Cleanup note (in scope only as a note; the decision rides RFC-0042)

`std/chan.witchy` does not import `std/task.witchy` — it carries a **full
duplicate** of the `Step`/`Task` types, the combinators, and the executor
(`chan.run` at `std/chan.witchy:451` beside `task.run`). The two coexist only
because duplicate type/constructor declarations silently last-win in the flat
namespace (probed: a user `type Step` redeclaration passes without error —
itself alarming, and why `import task` + `import future` fails: `future`'s
`Done` constructor overwrites `task`'s). Three name families
(`Step`×2-identical+1, `Task`, `Slot`×2, `Handle`) exist across
chan/task/future. Under 0042's namespaces the duplication should collapse to
one executor in `task`, re-exported by `chan` — but that consolidation is
0042's call, not this RFC's.

2026-07-07 update: that consolidation has landed. `std/chan` now imports the
task substrate, keeps typed channel endpoints and channel combinators, and
delegates task combinators plus `chan.run` to `std/task`; there is one scheduler
implementation body.

## Drawbacks

- **A hole in the type system, however confined.** `__erase`/`__unerase` is a
  trusted cast; its soundness argument (endpoint pairing) lives in `std/chan`'s
  code review, not in the checker. Mitigations: the intrinsics are
  non-exported, the pairing invariant is small and already load-bearing, and
  the differential suite would catch a violated round-trip as a divergence or
  trap. Still, this is the first deliberately non-inferable type boundary in
  the language.
- **Structural operations on erased values must never be reachable** — `==`,
  `show`, derive-generated code walking a buffer would see `Msg` and must
  reject at compile time (the executor treats messages opaquely today, so no
  current code path does this; a test pins it).
- **The executor rewrite touches the most parity-sensitive std module.** Every
  channel example, the chan_throughput benchmark, and RFC-0036's pending
  executor-ownership work all sit on this file; coordination cost is real.
- **(c)'s simplicity is genuinely forgone** — if separate compilation never
  arrives, (b) will have paid a type-system cost (c) avoided.

## Prior art

- **Go**: channels are per-type generic (`chan T`) over a runtime with erased
  scheduling — precisely the (b) split: typed endpoints, type-blind scheduler.
- **Rust (tokio/crossbeam)**: `Sender<T>`/`Receiver<T>` monomorphized over an
  untyped task queue; same shape, minus witchy's determinism constraint.
- **CLR/JVM erasure**: the general precedent that a uniform runtime
  representation makes generic channels a type-checking problem, not a
  codegen one.
- Internal: [`rfcs/concurrency-design.md`](concurrency-design.md) (the constraint's origin and the
  parity rationale this RFC keeps), RFC-0042 (namespaces — the sibling half),
  RFC-0036 (executor memory bounding — same file, independent concern),
  [`spec/architecture.md`](../spec/architecture.md) §"The WASM value model" (the i64-slot fact the
  recommendation rests on).

---

> 2026-07-04: implemented as option (b). The executor (`std/task` +
> `std/chan`, still duplicated pending RFC-0042) is now monomorphic over an
> opaque erased message type — `Task`/`Step`/`Slot` and the channel buffers
> dropped the `m` parameter and carry `__Msg` (a reserved double-underscore
> spelling, since `Msg` is already a user type in `examples/request_reply`).
> The typed endpoints `Sender(m)`/`Receiver(m)`/`Selected(m)` stay
> parameterized and bridge at the boundary via the `__erase`/`__unerase`
> intrinsic pair, added across all three backends as the identity (mirroring
> the `Bytes`↔`String` representation-neutral bridge): `send` erases into the
> buffer, `recv`/`select` unerase on the way out. The realization was
> largely std-witchy as anticipated; the only compiler support is the opaque
> `Ty::Msg` type + the two intrinsics (typeck signatures, identity lowering
> on both backends). Byte-identical determinism confirmed: every concurrency
> example passes `witchy parity`, and the two headline acceptance cases —
> two independent modules with private channels of different types, and one
> task pulling `Job`s while pushing `Answer`s — run and agree on both
> backends (`rfc0055_*` in `src/example_tests.rs`). Acceptance test #2
> (`import chan` + `import iter`) still awaits RFC-0042's namespaces, as this
> RFC noted; it is out of scope here. The type-system hole is confined but,
> consistent with the existing `__bytes_*`/`__render` intrinsics, is a naming
> convention rather than an enforced non-export — `==`/`show` on a forged
> `__Msg` agree on both backends (no divergence), and users never obtain one
> in normal use.

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below (e.g. "> 2026-07-01: clarified X").
  - The current behavior lives in spec/ and the code — NOT here.
-->
