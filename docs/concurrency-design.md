# Concurrency redesign: async/await + channels

> Status: **SHIPPED — all 6 phases complete (committed `accb1b8`).** The
> `actor`/`on`/`Subject`/`ask`/`reply`/`spawn ActorType` system has been retired;
> concurrency is now `async`/`await` over `std/chan` (a multi-inbox cooperative
> executor) + `std/future`. An actor is an `async fn` looping on `recv`. Build
> green, full suite passes. Informed by withoutboats' *Why async Rust?* (Oct 2023)
> and the wasmtime-45 capability survey; this document records the design as built.

## Decision

witchy adopts a **stackless async/await** concurrency model with **channels**,
**`spawn`/`Task`**, and a **structured `scope`** — the same family as Rust/C#,
but shaped by witchy's own semantics so it avoids async Rust's worst costs.

The model **subsumes the actor system**: an actor becomes an `async fn` looping
on a channel; `Subject` becomes a channel `Sender`; `ask`/`reply` become
"send a reply channel and await it." The `actor`/`on`/`Subject`/`ask`/`reply`
keywords are retired.

## Why this shape (and what we learned from Rust)

withoutboats' retrospective makes the tradeoffs explicit. Several of Rust's
costs were *forced by Rust's constraints* (no runtime, no GC, borrowing, C FFI,
zero-cost), which witchy does not share:

| Rust cost | Why Rust paid it | witchy |
|---|---|---|
| **`Pin`** | a future can hold a *reference to its own state* across `await` → a self-referential struct, and Rust structs are always movable | **Gone.** witchy has value semantics: a future captures *owned values*, never internal references, so it is never self-referential. **No `Pin`, no `Unpin`, no "pin a trait object to await it."** This removes async Rust's single biggest wart by construction. |
| **Poll-based futures** | CPS/callback futures needed refcounting + allocation (`join` owns the continuation twice), unacceptable for zero-cost | witchy isn't allocation-phobic (arena/region memory), so it picks the *simplest* model rather than the zero-cost one — **CPS via closures** (see "Suspension mechanism"), which witchy's existing closures already support. |
| **Coloring** | the price of stackless | **Kept, and it's on-brand.** witchy functions are already colored by capabilities (pure vs effectful). `async` is a *refinement of the effect color*: among effectful functions, which ones suspend. An `await` marks where the program touches the slow outside world — the same thing we already make visible with capabilities. |
| **Runtime fragmentation** (tokio vs async-std vs …; std has no executor) | Rust ships no runtime | **Ship one executor in the language/std.** A greenfield language has no ecosystem to fragment. |
| **Green threads dropped pre-1.0** | imposed a runtime on all code + hurt C FFI | witchy *has* a runtime and no C-FFI constraint, so stackless is a *choice* (per-task memory = state size, and it reuses our transform), not a necessity. |

The hard problem Rust still hasn't solved cleanly — **cancellation / async drop**
— witchy addresses head-on with **structured concurrency** (see below) rather
than "drop a future and hope cleanup runs."

## Surface

```text
fn       # sync: pure compute + non-blocking effects (print)
async fn # can suspend
await e  # suspend until e (Future / Task / channel op) is ready
```

Blocking capability I/O is async (`read`, `connect`, `recv`, channel
`send`/`recv`, task `join`, `sleep`); non-blocking effects (`print`) stay sync.

```text
async fn first_line(dir: Dir[Read], name: String) -> String:
    let contents = await read(dir, name)
    list.at(string.lines(contents), 0)

async fn main(console: Console, dir: Dir[Read]):
    print(console, await first_line(dir, "notes.txt"))
```

**Concurrency = `spawn` → `Task`, `await` to join, `scope` to contain:**

```text
async fn fetch_all(net: Net[Connect, Tcp], urls: List(String)) -> List(String):
    scope:
        let tasks = [spawn fetch(net, u) for u in urls]   # in flight; parallel under --parallel
        [await t for t in tasks]                           # gather; on error the scope cancels siblings
```

**Channels — the `(tx, rx)` mpsc/mpmc split, close-on-drop via the ownership pass:**

```text
let (tx, rx) = chan.new()       # Sender(T), Receiver(T)
spawn produce(tx)
for await v in rx:              # ranges until the last Sender is dropped
    print(console, v)
```

**`select` for racing/timeouts; actors as a three-line idiom:**

```text
async fn counter(inbox: Chan(Msg)):
    var n = 0
    for await msg in inbox:
        match msg:
            Inc        -> n = n + 1
            Get(reply) -> await reply.send(n)
```

## Open sub-decisions (with leanings)

- **Eager vs lazy futures.** Rust is lazy (poll-on-demand) because of zero-cost;
  witchy is free here. Lazy enables clean cancel-by-drop and composition; eager
  avoids the "called-but-not-awaited did nothing" footgun. *Leaning: calling an
  `async fn` yields a lazy `Future`; `spawn` makes it an eagerly-scheduled `Task`.*
- **Cancellation.** Dropping a `Task`/`Future` cancels it; **a `scope` makes this
  safe** — on early exit or error it cancels its children and waits for their
  cleanup before returning. No leaks, deterministic teardown. Async-drop is
  designed in via scopes, not bolted on.
- **`await` placement.** Prefix `await e` (readable) vs postfix `e.await` (chains).
  *Leaning: prefix.*

## Suspension mechanism (the crux)

An `async fn` must suspend mid-body at an `await` and resume later with its locals
intact. Two candidate lowerings were considered:

- **Reuse the `gen fn` lowering — REJECTED.** `gen` reaches the k-th `yield` by
  *re-running the body from the top* and counting yields (`src/generators.rs`).
  That is only sound because generators are required to be **capability-free** —
  re-running repeats no side effects. Async functions are **effectful** (they do
  I/O), so re-running would repeat the I/O. The gen trick does not transfer.

- **CPS via closures — CHOSEN.** Transform an async body so each `await` splits it
  into a continuation: `…; let x = await e; rest` lowers to roughly
  `e.and_then(fn(x): rest)`, over a `Future(T)` type in std. The executor polls
  the resulting future. This is tractable because witchy **already has closures**,
  so there is no bespoke state machine to emit in *two* backends — the transform
  produces ordinary AST (closures + calls) that both tiers already run, preserving
  parity for free. It also reinforces the **no-Pin** property: a suspended
  computation's live locals are the *captured values* of its continuation closure,
  and captures are owned values, never internal references — nothing to pin.

  Control flow is handled structurally: `if`/`match` continue each branch then
  re-join; an `await` inside a loop lowers the loop body to a **recursive
  future-returning helper** (the loop's mutable state becomes the helper's
  parameters). This is the bulk of the Phase-2 work and the main correctness risk;
  it is built and tested incrementally (straight-line → branching → loops).

## Parity & the runtime

- **Deterministic single-thread executor is the default and the test mode** —
  poll runnable tasks in a fixed order; interpreter and compiled VM stay
  byte-identical and replayable (deterministic-simulation testing, à la
  FoundationDB). Concurrency bugs reproduce identically.
- **`--parallel`** swaps in a work-stealing executor across cores. On the
  compiled tier this is wasmtime's blessed shared-nothing pattern: one `Store`
  per worker, shared `Engine` (stable in wasmtime 45 today). No WASM
  stack-switching needed, because `async fn` lowers to a state machine.
- **No data races** — captured values are immutable; channels move values; the
  compiler already forbids the shared mutable state Go relies on convention to
  avoid.

## Implementation status / phases

1. **Surface + coloring — DONE.** `async`/`await` keywords, `is_async` on
   `Function`, `UnOp::Await` (a value-neutral unary modeled on `UnOp::Move`, so it
   survives to `fmt`); parser enforces "await only inside an `async fn`". No
   executor yet, so async runs sequentially and `await e == e` — byte-identical on
   both backends. (Sequential placeholder; replaced when Phase 2 lowering is wired.)
2. **`async fn` → CPS via closures — DONE (supported scope).** The link-time
   transform `src/async_lower.rs` rewrites async bodies into `and_then` chains over
   the unified `std/chan` `Task` substrate (see phase 4), run BEFORE typeck (like
   `gen`), so typeck/codegen/interp never see `async`/`await`. The whole body is
   wrapped in `chan.lazy` (no work until driven); an async `main` lowers to
   `chan.run([body])`; an async fn's return type is left to inference (so the
   channel message type is concrete when the body `send`s/`recv`s, phantom when it
   doesn't — both monomorphize). *Supported:* straight-line `let`/effects,
   `if`/`match` (tail), recursion (the actor loop, written recursively), and
   `await chan.recv()`/`await chan.send()`. *Rejected with a clear error (to be
   lifted):* `await` inside a `while`/`for`, in a condition/scrutinee, or nested in
   a larger expression; and carrying a mutable `var` across an `await` (caught for
   free by the existing "a closure may not assign to a captured variable" rule).
3. **Deterministic executor.** *DONE (structured form):* `future.join_all(tasks)`
   — a round-robin scheduler written in pure witchy (one poll step per task per
   round), proven to interleave cooperative tasks byte-identically on both backends
   (`examples/async_executor.witchy`). Because it is ordinary witchy, parity is
   free and no runtime scheduler state / WASM feature is needed. *Remaining:*
   free-floating `spawn` (dynamic task registration from any depth) needs shared
   mutable scheduler state, which value-semantics witchy can't express purely — so
   `spawn` will be a runtime primitive mirrored across both backends, OR the model
   stays structured (`join_all`/`scope` over an explicit task list, which already
   covers the C10k fan-out: `[fetch(u) for u in urls]` → `join_all`).
4. **Channels — DONE (generic messages).** `std/chan`: a cooperative
   message-passing executor in pure witchy via an effect protocol — a task yields
   `Emit`/`Recv` requests and the executor owns the one FIFO buffer and threads it
   through the schedule (no shared mutable state, no runtime primitive).
   `send`/`recv`/`done`/`and_then`/`yield_now` + `run(tasks)`; a consumer that
   loops on `recv` IS the actor idiom. Messages are **generic** (`Task(m, a)`),
   proven parity-clean for `Int` and `String` (`examples/channels.witchy`). This
   needed a real monomorphizer fix: explicit ADT type params (`type Step(m, a):`)
   are now HONORED to fix the parameter order — previously the order was inferred
   from variant fields, so a constructor that omits a param (`Done(a)`) mis-placed
   the message type and collapsed it to `Nil`. Channels are **unified with `async`/`await`**:
   the CPS transform targets this substrate, so you write `await chan.send(x)` /
   `await chan.recv()` inside an `async fn` and run a list of them with `chan.run`
   — an `async fn` looping on `recv` IS an actor, ergonomically
   (`examples/async_tasks.witchy`, `async_with_channels_backends_agree`). The manual
   recursion is now optional: `chan.serve(state, handle)` is the actor loop as a
   combinator — it receives, runs `handle` to get the next state, and repeats,
   threading state through every message (`examples/counter_serve.witchy`).

   *Decoupled addressing — `chan.address()`.* `send`/`recv` address tasks by their
   index in the `run` list, so a responder would otherwise hardcode the asker's
   position. `chan.address()` returns the current task's own address; a request
   carries it so the responder replies to whatever address it was handed, not a
   literal (`examples/request_reply.witchy`). It is a fifth `Step` effect, `Whoami`,
   answered by the executor with the task's index — the same shape as `Recv`.

   *Deferred:* first-class `(tx, rx)` channel handles and `for await v in rx` loop
   sugar, both of which can build on `address`.
5. **`scope` + `select` + cancellation — DONE (primitive form).** `future.select`
   races tasks and returns the first to finish, dropping the losers — and because
   futures are pure and lazy, dropping IS cancellation (no cleanup hook needed),
   proven parity-clean. Structured join is already `join_all`. *Deferred:* the
   `scope:`/`select` SURFACE syntax and cancel-siblings-on-error (`try_join`), which
   are sugar over these primitives.
6. **Retire actors — replacement COMPLETE, only the migration remains.** The
   async/channel model now covers the full actor model:
   - single actor + `ask`/`reply` (`examples/actor_as_async.witchy`),
   - **multiple actors with separate mailboxes** — `chan` gives every task its own
     inbox; `send(target, msg)` routes by actor index, `recv()` reads the current
     task's inbox; actor state lives in a recursive parameter
     (`examples/actors_async.witchy`: Logger + Forwarder + driver, parity-clean).

   What's left is purely the **migration + removal**, which is genuinely large and
   atomic:
   ~16 `src/` files reference actors (including the 1722-line `src/actor_system.rs`,
   plus actor support in lexer/parser/ast/typeck/interpreter/codegen/format), ~192
   lines of actor tests in `src/main.rs`, six `examples/actor_*.witchy`, `std/server`,
   and `book/src/tour-actors.md`. Removing `actor`/`on`/`Subject`/`ask`/`reply` while
   keeping the suite green means migrating every one of those in the same change —
   a dedicated effort, not a tail-of-session edit. `fmt` should rewrite the old
   forms where mechanical.

## Sources

- withoutboats, *Why async Rust?* — <https://without.boats/blog/why-async-rust/>
- withoutboats, *Pin* — <https://without.boats/blog/pin/>
- WebAssembly stack-switching (phase 3) — <https://webassembly.org/features/>
- wasmtime multithreaded embedding (store-per-thread) —
  <https://docs.wasmtime.dev/examples-multithreaded-embedding.html>
