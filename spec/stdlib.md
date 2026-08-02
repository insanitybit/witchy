# API reference

## `ascii`

ASCII character predicates over single-character strings (such as those `string.char_at` returns). Pure and capability-free. Classification is by code point in the ASCII range; the comparisons use the standard string ordering, so every function here is correct on both the interpreter and the compiled backend. The rough equivalent of Go's `unicode` helpers for the ASCII subset.

#### `fn is_digit(c: String) -> Bool`

#### `fn is_upper(c: String) -> Bool`

#### `fn is_lower(c: String) -> Bool`

#### `fn is_alpha(c: String) -> Bool`

#### `fn is_alnum(c: String) -> Bool`

#### `fn is_space(c: String) -> Bool`

#### `fn to_digit(c: String) -> Option(Int)`

The numeric value of a single decimal digit as `Some`, or `None` when `c` is not a digit (RFC-0044 rule 1: absence is `Option`, never a -1 sentinel).

#### `fn all_digits(s: String) -> Bool`

True when `s` is non-empty and every character is an ASCII digit — a safe guard before `string_to_int`, which traps on non-numeric input.

## `borrow`

Materialization for RFC-0083 borrowed views. `value.owned()` copies a borrowed `View(T, 'a)` into an owned `T`, ending the view's borrow of its owner so the owner may be mutated or moved again.

A view has no value representation of its own, so `owned` is the logical identity on both backends. The checked last use ends the loan; compiled ownership facts have already made the owner shared, so a later mutation copy-on-writes and the returned value remains an independent snapshot. It is a blanket impl over any type and dispatches through the ordinary typed method path (RFC-0046), with the same shape as `std/convert`'s `Into`. On a non-view value it is a plain identity. (This module is named `borrow`, not `own`, because `own` is the parameter-convention keyword and so cannot be a module name.)

#### `trait Owned`

- `fn owned(self) -> Self`

### Trait implementations

#### `impl Owned for a`

- `fn owned(self) -> Self`

## `bytes`

std/bytes — immutable byte buffers.

A `Bytes` is a flat, UTF-8-free sequence of bytes — the type for binary data (file contents, network frames, hashes, serialized payloads) that `String` (which is always valid UTF-8) cannot faithfully hold. It shares `String`'s in-memory layout (`[length][bytes…]`), so the bridge operations (`from_string`/`to_string`) are free, and a `Bytes` is FLAT: it byte-copies directly across a worker VM boundary (RFC-0032), making it the canonical cross-VM and serialization payload.

#### `type BytesError`

Matchable byte-buffer conversion failures.

- `ByteOutOfRange(Int)`
- `InvalidUtf8`

#### `fn bytes_error_message(e: BytesError) -> String`

#### `fn from_string(s: String) -> Bytes`

The UTF-8 bytes of a string.

#### `fn from_list(xs: List(Int)) -> Result(Bytes, BytesError)`

Build raw bytes from integers in `0..=255`, or Err on the first invalid byte.

#### `fn from_list_string(xs: List(Int)) -> Result(Bytes, String)`

#### `fn to_string(b: Bytes) -> String`

Decode bytes as UTF-8 text. Invalid sequences are replaced with U+FFFD (lossy), so this never fails; round-tripping a string is exact because witchy strings are always valid UTF-8. Prefer `to_string_lossy` when the lossy boundary matters.

#### `fn length(b: Bytes) -> Int`

The number of bytes.

#### `fn at(b: Bytes, index: Int) -> Int`

The byte at `index`, as an Int in `0..=255`.

#### `fn concat(first: Bytes, second: Bytes) -> Bytes`

The two byte buffers joined.

#### `fn slice(b: Bytes, start: Int, end: Int) -> Bytes`

The bytes in `start..end` (clamped to the buffer; `start >= end` yields empty).

#### `Bytes.to_string_lossy() -> String`

Decode bytes as UTF-8 text, replacing invalid sequences with U+FFFD. This is explicit about being lossy; use `decode_utf8` when invalid bytes must be an Err.

#### `Bytes.to_string() -> String`

Decode bytes as UTF-8 text. Invalid sequences are replaced with U+FFFD (lossy), so this never fails; round-tripping a string is exact because witchy strings are always valid UTF-8. Prefer `to_string_lossy` when the lossy boundary matters.

#### `Bytes.decode_utf8() -> Result(String, BytesError)`

Strict UTF-8 decode. Returns Err instead of replacing invalid byte sequences.

#### `Bytes.decode_utf8_string() -> Result(String, String)`

#### `Bytes.length() -> Int`

The number of bytes.

#### `Bytes.is_empty() -> Bool`

Whether the buffer has no bytes.

#### `Bytes.at(index: Int) -> Int`

The byte at `index`, as an Int in `0..=255`.

#### `Bytes.get(index: Int) -> Option(Int)`

The byte at `index`, or None when out of range.

#### `Bytes.concat(second: Bytes) -> Bytes`

The two byte buffers joined.

#### `Bytes.slice(start: Int, end: Int) -> Bytes`

The bytes in `start..end` (clamped to the buffer; `start >= end` yields empty).

#### `Bytes.to_list() -> List(Int)`

The bytes as a list of Ints in `0..=255`.

#### `Bytes.contains(needle: Bytes) -> Bool`

Whether `needle` appears in the buffer. The empty needle is always present.

#### `Bytes.index_of(needle: Bytes) -> Option(Int)`

The first byte index where `needle` appears, or None when absent. The empty needle is found at 0, matching string/list search conventions.

#### `Bytes.starts_with(prefix: Bytes) -> Bool`

Whether the buffer starts with `prefix`.

#### `Bytes.ends_with(suffix: Bytes) -> Bool`

Whether the buffer ends with `suffix`.

### Trait implementations

#### `impl Show for BytesError`

- `fn show(self) -> String`

#### `impl Error for BytesError`

#### `impl From(BytesError) for String`

- `fn from(value: BytesError) -> Self`

## `chan`

std/chan — decoupled concurrency: `spawn` concurrent tasks, communicate over first-class `channel`s. Spawning and channels are independent — you can spawn without a channel, and a channel is a value you create and pass around, not a task's mailbox. Built on a pure-witchy cooperative executor with a deterministic round-robin schedule, so a concurrent run is byte-identical on the interpreter and the compiled WebAssembly — no scheduler state in the runtime, no `Pin`.

Messages: channels are per-type generic (RFC-0055). A `Sender(m)`/`Receiver(m)` pair carries values of ITS OWN type `m`, and independent channels in one program may carry different types — a library may pipeline work through a private channel without forcing its message type on the whole program. Under the hood the executor is ERASED: its effects and buffers carry the opaque `__Msg`; the typed endpoints erase a message on `send` and recover it on `recv`. The erasure is representationally the identity on both backends (a message already rides the universal slot), so interleavings stay byte-identical. Spawned tasks return `Nil`; a task reports a result by sending it on a channel, not by returning it (a typed `JoinHandle(T)` would force a native runtime and break the parity contract). `send`/`recv` are always `await`ed because messaging is an effect on the executor-owned buffer; a *bounded* channel additionally blocks the sender when full while the executor is making progress (backpressure), an unbounded one never does. If every live task parks with no progress, the executor runs its quiescence close pass: parked receivers/selects resume with `None`, parked senders are released, and parked joins resume. This replaces sender refcounting and destructors; it is deterministic on both backends, but "closed" means quiescent, not "no `Sender` value can ever be used again".

The `async`/`await` CPS transform lowers onto the `std/task` executor (task.lazy/and_then/done/run); channel ops (`await chan.recv(rx)` / `await chan.send(tx, x)`) run on the same protocol.

#### `sealed type Sender(m)`

The typed sending endpoint of a channel carrying `m` messages.

- `Sender(ChannelId)`

#### `sealed type Receiver(m)`

The typed receiving endpoint of a channel carrying `m` messages.

- `Receiver(ChannelId)`

#### `type Selected(m)`

The outcome of a `select`: a message from the first or the second receiver, or `Closed` once neither can deliver.

- `First(m)`
- `Second(m)`
- `Closed`

#### `fn done(x: a) -> Task(a)`

A finished task.

#### `fn ready_unit() -> Task(Nil)`

An already-complete `Task(Nil)` — the async/await lowering target for a body that falls off its end.

#### `fn yield_now() -> Task(Nil)`

Hand control back to the executor once, then continue.

#### `fn and_then(t: Task(a), k: fn(a) -> Task(b)) -> Task(b)`

Sequence: run `t`, then continue with `k` applied to its result. This is what `await` lowers to — the continuation `k` is the rest of the body.

#### `fn map(t: Task(a), f: fn(a) -> b) -> Task(b)`

Transform a task's result.

#### `fn lazy(thunk: fn() -> Task(a)) -> Task(a)`

Build the task `thunk()` lazily: nothing runs until this task is polled. A `Task` is a replayable execution recipe, not a memo cell; the standard driver advances through continuations, but driving the same task value again reruns its thunk.

#### `fn for_each(xs: List(a), f: fn(a) -> Task(Nil)) -> Task(Nil)`

Run `f(x)` as a task for each `x` in `xs`, in order — the lowering target for an `await` inside a `for x in xs:` loop.

#### `fn channel(capacity: Int) -> Task((Sender(m), Receiver(m)))`

A channel of logical `capacity`: a positive capacity is bounded (the sender blocks when the buffer is full), while 0 — or any non-positive value — is unbounded and never blocks the sender, matching the convention that a non-positive bound means "no bound".

#### `fn unbounded() -> Task((Sender(m), Receiver(m)))`

An unbounded channel — `send` never blocks (the buffer grows without limit).

#### `fn send(tx: Sender(m), msg: m) -> Task(Nil)`

Send `msg`; on a bounded channel this blocks until there is room while some task can make progress. If the whole executor reaches quiescence with this send parked, the close pass releases it and stores the message, even if that temporarily exceeds the logical capacity. Always awaited. The message is erased to the executor's opaque slot at this boundary.

#### `fn recv(rx: Receiver(m)) -> Task(Option(m))`

Receive the next message, or `None` when the executor reaches quiescence with this receive parked. `for await x in rx:` loops until this `None`. Because witchy does not refcount sender values, a `Sender` retained by later code may still send after such a quiescent close. The erased value is recovered at `m` at this boundary.

#### `fn spawn(child: Task(Nil)) -> Task(Handle)`

Start `child` as a concurrent task; the returned handle completes when it does.

#### `fn join(h: Handle) -> Task(Nil)`

Wait for the spawned task behind `h` while the executor can make progress. If the whole executor reaches quiescence with this join parked, the close pass releases it even if the joined task has a continuation that will run afterward.

#### `fn cancel(h: Handle) -> Task(Nil)`

Cancel the spawned task behind `h`: it is stepped no further and is treated as finished, so anyone `join`ing it unblocks immediately. Cancellation is shallow — it stops this one task, not any tasks it itself spawned — and idempotent (already finished or already cancelled is a no-op). Deterministic on the cooperative schedule, hence byte-identical on both backends. Used by `race` to drop the loser.

#### `fn spawn_all(children: List(Task(Nil))) -> Task(List(Handle))`

Spawn every task in `children` concurrently, returning their handles. The children begin running on the next executor turns; nothing is joined yet.

#### `fn join_all(hs: List(Handle)) -> Task(Nil)`

Join every handle in `hs` while the executor can make progress. Like `join`, quiescence releases a parked join even if a child has a continuation that will run afterward.

#### `fn cancel_all(hs: List(Handle)) -> Task(Nil)`

Cancel every handle in `hs` — the companion to `spawn_all`/`join_all`. Each is stopped and treated as finished; idempotent, so cancelling an already-finished handle is a no-op. Used by `race_n` to drop the losers.

#### `fn scope(children: List(Task(Nil))) -> Task(Nil)`

STRUCTURED concurrency (a "nursery"): run every task in `children` concurrently and wait for their handles while the executor can make progress. No handle escapes the call, so the normal case has no leaked tasks; if the whole executor reaches quiescence, `join_all` releases just like `join`. Prefer this over a bare `spawn` whose handle you must remember to `join`. The children interleave on the cooperative executor, so a concurrent run is byte-identical on both backends (the parity contract). Results flow out over channels, as with any task (a child returns `Nil`).

#### `fn gather(jobs: List(Task(m))) -> Task(List(m))`

STRUCTURED fan-out-and-collect: run every task in `jobs` concurrently and return the results that arrive before the collecting channel quiesces. In the normal case every job completes and sends exactly one result. Each job produces a value of the message type `m` (results ride the same channels), so `gather` is the typed companion to `scope` — the same no-escaping-handle shape, with the results handed back. Results are in COMPLETION order (deterministic on the cooperative executor, hence byte-identical on both backends), not input order.

#### `fn par_map(items: List(a), f: fn(a) -> Task(m)) -> Task(List(m))`

STRUCTURED parallel map (the level-1 combinator): run `f` over every item of `items` concurrently and return the results in INPUT order. The tasks are never visible — spawn and join happen inside — so this cannot leak a handle, cannot deadlock on a forgotten join, and is the ergonomic default for data parallelism. Each item's result rides its own channel, so the returned order is the input order regardless of completion order: the result is a pure function of `items` and `f`, hence DETERMINISTIC by construction and byte-identical on both backends. That determinism is exactly what lets a future parallel backend run the items on separate cores without changing the observable result (see RFC-0032). Results are of the message type `m` (they ride channels), as with `gather`.

#### `fn par_reduce(items: List(a), f: fn(a) -> Task(m), init: m, combine: fn(m, m) -> m) -> Task(m)`

STRUCTURED parallel reduce: `par_map` the items, then fold the results with `combine` starting from `init`. `combine` should be associative for the fold to be meaningful independent of evaluation order; the map runs concurrently while the fold is a deterministic left fold over the input-ordered results.

#### `fn race(a: Task(m), b: Task(m)) -> Task(Option(m))`

Run `a` and `b` concurrently and return the FIRST result, cancelling the loser. `None` only if neither ever produces a value. The winner is decided by the deterministic round-robin schedule (a tie favours `a`), so the outcome is byte-identical on both backends — and under a future parallel backend the cancel genuinely stops the loser's remaining work. This is the cancellation-enabled combinator of RFC-0032's ladder; build `timeout` by racing a task against one that yields a sentinel.

#### `fn race_n(tasks: List(Task(m))) -> Task(Option(m))`

`race` generalized to a pool: run every task in `tasks` concurrently and return the FIRST result, cancelling all the others. `None` only if none ever produces a value (e.g. an empty list). The winner is fixed by the deterministic schedule, so the outcome is byte-identical on both backends.

#### `fn select(a: Receiver(m), b: Receiver(m)) -> Task(Selected(m))`

Receive from whichever of `a` or `b` has a message first; a tie favours `a`. Yields `Closed` once both channels are closed.

#### `fn consume(rx: Receiver(m), f: fn(m) -> Task(Nil)) -> Task(Nil)`

Receive from `rx`, run `f` on each message, until the channel closes. The stateless server loop; `for await x in rx:` lowers to this.

#### `fn serve(rx: Receiver(m), state: s, handler: fn(s, m) -> Task(s)) -> Task(Nil)`

The stateful server loop: receive a message, run `handler` with the current `state` to get the next state, and repeat until the channel closes. State threads through every message with no hand-written recursion.

#### `fn run(root: Task(Nil))`

Drive `root` through the canonical `std/task` executor. `chan` keeps this facade for channel-centric programs, but scheduling has a single implementation.

## `cmp`

The witchy standard comparison hierarchy, mirroring Rust's `std::cmp`: `PartialEq` → `Eq` → `PartialOrd` → `Ord`. The comparison operators desugar through these traits, so `a == b` and `x < y` work on your own types once you implement (or derive) them — there is no separate `compare`/`greater` to call by name. Built-in impls cover the primitives; `Self` in a method signature stands for the implementing type. Pure and capability-free.

#### `type Ordering`

The result of a comparison: `a` is `Less` than, `Equal` to, or `Greater` than `b`. The return of `Ord.compare`, and (wrapped in `Some`) `PartialOrd`.

- `Less`
- `Equal`
- `Greater`

#### `trait PartialEq`

`==` and `!=`. The minimal equality trait: implement `eq` and `ne` comes free. `Float` is `PartialEq` but NOT `Eq` — `NaN != NaN`, so equality is not reflexive.

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool` _(default)_

#### `trait Eq: PartialEq`

Total equality: a marker refining `PartialEq` with reflexivity (no `NaN`). Types usable as `Set` / `Dict` keys are `Eq`.


#### `trait PartialOrd: PartialEq`

`<` `>` `<=` `>=`. `partial_compare` returns `None` for incomparable values (a `NaN`); the four ordering operators are then all false, as in Rust. Implement `partial_compare`; the rest come free.

- `fn partial_compare(self, other: Self) -> Option(Ordering)`
- `fn less(self, other: Self) -> Bool` _(default)_
- `fn greater(self, other: Self) -> Bool` _(default)_
- `fn less_equal(self, other: Self) -> Bool` _(default)_
- `fn greater_equal(self, other: Self) -> Bool` _(default)_

#### `trait Ord: Eq + PartialOrd`

A total order: `compare` never reports "incomparable". Sorting and the `min`/ `max` helpers require `Ord`. Implement `compare`; the companion `PartialOrd` impl's `partial_compare` is `Some` of it.

- `fn compare(self, other: Self) -> Ordering`

#### `fn reverse(o: Ordering) -> Ordering`

Flip an ordering — `Less` <-> `Greater`, `Equal` unchanged — for reverse sorts.

#### `fn max_of(x: a, y: a) -> a where a: Ord`

#### `fn min_of(x: a, y: a) -> a where a: Ord`

#### `fn clamp(x: a, lo: a, hi: a) -> a where a: Ord`

`x` confined to the range [lo, hi]. `lo` must not exceed `hi` (RFC-0044 rule 3): inverted bounds describe an empty range, so they fail loudly instead of silently returning `lo` (matching `math.clamp`).

#### `fn maximum(xs: List(a), default: a) -> a where a: Ord`

The largest element of `xs`, or `default` when `xs` is empty. `default` is only the empty fallback — it never participates in the comparison, so it is returned unchanged only for an empty list (a small `default` no longer wins over the elements, nor a large one lose to them).

#### `fn minimum(xs: List(a), default: a) -> a where a: Ord`

The smallest element of `xs`, or `default` when `xs` is empty. As with `maximum`, `default` is the empty fallback only, never a competing bound.

### Trait implementations

#### `impl PartialEq for Ordering`

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for Ordering`

#### `impl PartialEq for Int`

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for Int`

#### `impl PartialOrd for Int`

- `fn partial_compare(self, other: Self) -> Option(Ordering)`

#### `impl Ord for Int`

- `fn compare(self, other: Self) -> Ordering`

#### `impl PartialEq for Bool`

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for Bool`

#### `impl PartialEq for String`

Lexicographic ordering by code point — the same order `<` gives on strings, and content-correct in compiled code (both backends compare bytes, not pointers).

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for String`

#### `impl PartialOrd for String`

- `fn partial_compare(self, other: Self) -> Option(Ordering)`

#### `impl Ord for String`

- `fn compare(self, other: Self) -> Ordering`

#### `impl PartialEq for Bytes`

Byte buffers compare by byte contents, matching direct `==` and String's content equality. `Bytes` deliberately has equality but no ordering protocol.

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for Bytes`

#### `impl PartialEq for Duration`

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for Duration`

#### `impl PartialOrd for Duration`

- `fn partial_compare(self, other: Self) -> Option(Ordering)`

#### `impl Ord for Duration`

- `fn compare(self, other: Self) -> Ordering`

#### `impl PartialEq for Float`

`Float` is `PartialEq` + `PartialOrd` only: a `NaN` is unequal to everything (including itself) and unordered, so `Float` is neither `Eq` nor `Ord`. Sort a `List(Float)` with a total wrapper or by comparing a derived key.

- `fn eq(self, other: Self) -> Bool`

#### `impl PartialOrd for Float`

- `fn partial_compare(self, other: Self) -> Option(Ordering)`

#### `impl PartialEq for List(a) where a: PartialEq`

Lists compare element-by-element through the element type's `PartialEq` impl.

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for List(a) where a: Eq`

#### `impl PartialEq for Option(a) where a: PartialEq`

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for Option(a) where a: Eq`

#### `impl PartialEq for Result(a, e) where a: PartialEq, e: PartialEq`

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for Result(a, e) where a: Eq, e: Eq`

#### `impl PartialEq for Dict(k, v) where k: Eq, v: PartialEq`

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for Dict(k, v) where k: Eq, v: Eq`

#### `impl PartialEq for (a, b) where a: PartialEq, b: PartialEq`

Tuples compare slot-by-slot through each slot's equality protocol. The tuple protocol surface is explicit through arity 8, matching `Show` and `Reflect`.

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for (a, b) where a: Eq, b: Eq`

#### `impl PartialEq for (a, b, c) where a: PartialEq, b: PartialEq, c: PartialEq`

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for (a, b, c) where a: Eq, b: Eq, c: Eq`

#### `impl PartialEq for (a, b, c, d) where a: PartialEq, b: PartialEq, c: PartialEq, d: PartialEq`

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for (a, b, c, d) where a: Eq, b: Eq, c: Eq, d: Eq`

#### `impl PartialEq for (a, b, c, d, e) where a: PartialEq, b: PartialEq, c: PartialEq, d: PartialEq, e: PartialEq`

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for (a, b, c, d, e) where a: Eq, b: Eq, c: Eq, d: Eq, e: Eq`

#### `impl PartialEq for (a, b, c, d, e, f) where a: PartialEq, b: PartialEq, c: PartialEq, d: PartialEq, e: PartialEq, f: PartialEq`

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for (a, b, c, d, e, f) where a: Eq, b: Eq, c: Eq, d: Eq, e: Eq, f: Eq`

#### `impl PartialEq for (a, b, c, d, e, f, g) where a: PartialEq, b: PartialEq, c: PartialEq, d: PartialEq, e: PartialEq, f: PartialEq, g: PartialEq`

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for (a, b, c, d, e, f, g) where a: Eq, b: Eq, c: Eq, d: Eq, e: Eq, f: Eq, g: Eq`

#### `impl PartialEq for (a, b, c, d, e, f, g, h) where a: PartialEq, b: PartialEq, c: PartialEq, d: PartialEq, e: PartialEq, f: PartialEq, g: PartialEq, h: PartialEq`

- `fn eq(self, other: Self) -> Bool`
- `fn ne(self, other: Self) -> Bool`

#### `impl Eq for (a, b, c, d, e, f, g, h) where a: Eq, b: Eq, c: Eq, d: Eq, e: Eq, f: Eq, g: Eq, h: Eq`

## `compiler`

compiler — witchy's own toolchain, exposed to witchy programs.

A native intrinsic module (implemented in Rust, like `crypto`): it gives a program access to the compiler's capability analyzer, so a (self-hosted) package manager can compute a rune's supply-chain footprint from within witchy — on either backend. The body below is a placeholder the runtime never executes (the call is intercepted by its qualified name).

#### `type CompilerError`

Matchable compiler-service failures. `SourceRejected` is the ordinary parse, type, or comptime boundary reported by the compiler; the internal variants are for malformed native bridge output and should be treated as toolchain bugs.

- `SourceRejected(String)`
- `InternalJson(json.DecodeError)`
- `InternalShape(String)`

#### `fn compiler_error_message(e: CompilerError) -> String`

#### `fn footprint(source: String) -> String`

The capability footprint of witchy `source`, as JSON:   {"total":[..],"build":[..],"user_caps":[..],"entries":[{"name":..,"capabilities":[..],"brands":[..]}]} or {"error":".."} if the source does not parse or contains `comptime:` blocks. Use the source-file CLI path for expanded comptime introspection. `build` is the build-time footprint — the build capabilities the rune's `build` entrypoint demands (gated separately from the runtime `total`). `user_caps` lists grantable user-capability names the source requires. Parse it with `import json`.

#### `fn diff(old: String, new: String) -> String`

Compare two sources by capability footprint, as JSON:   {"widened":bool,"added":[..],"removed":[..],"build_added":[..],    "build_removed":[..],"user_caps_added":[..],"user_caps_removed":[..]} or {"error":".."} if either source does not parse. `widened` is the rights-precise block-on-widening gate: true when `new` demands any runtime capability, build capability, or user cap that `old` did not.

#### `fn try_doc(name: String, source: String) -> Result(String, CompilerError)`

Render `source` to Markdown API documentation, or return a typed parse/comptime boundary error as `Err`. This is the tooling API for registries and package managers: it gives callers an inspectable error channel instead of hiding a failure inside presentation Markdown.

#### `fn try_doc_string(name: String, source: String) -> Result(String, String)`

Render docs with String errors for application-style boundaries.

#### `fn doc(name: String, source: String) -> String`

Render `source` to Markdown API documentation (the same output as `witchy doc` for source-only modules): the module's public types, traits, trait implementations, and functions with their signatures and doc-comments, under a heading titled `name`. This only PARSES the source — it never runs it — so a registry can safely generate browsable docs from a rune's stored source on either backend. Display callers get parse/comptime-boundary errors as an HTML comment; tooling should prefer `try_doc`.

### Trait implementations

#### `impl Show for CompilerError`

- `fn show(self) -> String`

#### `impl Error for CompilerError`

#### `impl From(CompilerError) for String`

- `fn from(value: CompilerError) -> Self`

## `convert`

Conversion traits, following Rust's `std::convert`. `From(a)` builds the implementing type from an `a`; `Into(b)` consumes `self` into a `b`. Implementing `From` is enough, since the blanket impl below derives the matching `Into`:

  Celsius.from(deg)   build via From   value.into()        convert via the derived Into

`from` takes no `self`, so the blanket impl calls it on the target type as `b.from(self)`. The `where` bound decides which `from` to call when the use site is monomorphized.

#### `trait From(a)`

- `fn from(value: a) -> Self`

#### `trait Into(b)`

- `fn into(self) -> b`

### Trait implementations

#### `impl Into(b) for a where b: From(a)`

- `fn into(self) -> b`

## `crypto`

crypto — cryptographic hashing and signatures.

Like Go's `crypto/*` packages, these are *native intrinsics*: SHA-256 and Ed25519 cannot be expressed in witchy itself (no byte access; elliptic-curve field arithmetic), so they are implemented in Rust. They are reachable only through this module — there is no global builtin. The function bodies below are placeholders the runtime never executes for native-backed functions: the interpreter intercepts their qualified names (`crypto.sha256`, `crypto.__ed25519_verify_status`, ...), and the WASM backend bridges them to the same implementation as host imports. Fallible public verify functions are ordinary Witchy wrappers around private native status intrinsics.

#### `type VerifyError`

Malformed verifier input is typed so callers can distinguish "bad key bytes", "bad message encoding", "bad signature encoding", and a missing verifier from a well-formed signature that simply does not match (`Ok(false)`).

- `MalformedPublicKey(String)`
- `MalformedMessage(String)`
- `MalformedSignature(String)`
- `VerifierUnavailable(String)`

#### `fn verify_error_message(e: VerifyError) -> String`

Human-readable verifier failure text. Kept as an explicit helper so existing String-oriented callers can preserve diagnostics while matching on `VerifyError` remains available for libraries.

#### `fn sha256(data: String) -> String`

SHA-256 of a string's UTF-8 bytes, as 64 lowercase hex characters.

#### `fn rune_hash(paths: List(String), contents: List(String)) -> String`

The canonical content hash of a rune's source tree, as `sha256:<hex>`. Pass parallel lists — one entry per file (`witchy.toml` plus each `src/**/*.witchy`): `paths[i]` is the relative path, `contents[i]` its text. Entries are sorted and length-prefixed before hashing, so the result is the rune's stable content address — the package manager's tamper-evident identity.

#### `fn ed25519_verify(public_key: String, message: String, signature: String) -> Result(Bool, VerifyError)`

Verify an Ed25519 signature. `public_key` and `signature` are hex-encoded; `message` is the raw string. Malformed inputs are `Err`; a well-formed but non-matching signature is `Ok(false)`.

#### `fn sign(key: Secret, message: String) -> String`

Sign `message` with a `Secret` capability (the host grants it; it cannot be forged), returning the hex signature.

#### `fn public_key(key: Secret) -> String`

The hex Ed25519 public key for a `Secret` — what verifiers check against.

#### `fn reveal(key: Secret) -> String`

Reveal a `Secret`'s raw bytes as a string — for revealable value secrets (tokens, passwords) that must be handed to an external sink. Errors on secrets that are not revealable: signing keys (granted with `--signing-key`, used via `sign`/`public_key`) and any secret granted use-only (`--secret-file name=path,use-only`, e.g. a TLS private key).

#### `fn ecdsa_p256_verify(public_key: String, message: String, signature: String) -> Result(Bool, VerifyError)`

Verify an ECDSA P-256 / SHA-256 signature — WebAuthn "ES256" (COSE alg -7). `public_key` is the hex SEC1 uncompressed point (`04 || x || y`); `signature` is the hex ASN.1-DER signature; `message` is the raw bytes it covers. Malformed inputs are `Err`; a well-formed but non-matching signature is `Ok(false)`. (Native/interpreter-only.)

#### `fn ecdsa_p256_verify_hex(public_key: String, message: String, signature: String) -> Result(Bool, VerifyError)`

Like `ecdsa_p256_verify` but the message is also hex — for binary messages such as WebAuthn's `authenticatorData || SHA256(clientDataJSON)`. Malformed message hex is `Err`, not a false signature. (Native-only.)

#### `fn rsa_pkcs1_sha256_verify(public_key: String, message: String, signature: String) -> Result(Bool, VerifyError)`

Verify an RSASSA-PKCS1-v1_5 / SHA-256 signature — JWT/OIDC "RS256" (the algorithm GitHub Actions and Google sign their identity tokens with). `public_key` is the hex of a DER-encoded RSA public key (PKCS#1 `RSAPublicKey`); `signature` is hex; `message` is the raw signed bytes (`header.payload` for a JWT). Malformed inputs are `Err`; a well-formed but non-matching signature is `Ok(false)`. (Native-only.)

#### `fn sha512(data: String) -> String`

SHA-512 of a string's UTF-8 bytes, as 128 lowercase hex characters. (Native-only.)

#### `fn sha3_256(data: String) -> String`

SHA3-256 (FIPS 202) of a string's UTF-8 bytes, as 64 hex characters. (Native-only.)

#### `fn hmac_sha256(key: String, message: String) -> String`

HMAC-SHA256 (FIPS 198-1). `key` is hex (so binary keys are representable); `message` is raw text. Returns the 64-hex-char tag. (Native-only.)

### Trait implementations

#### `impl Show for VerifyError`

- `fn show(self) -> String`

#### `impl Error for VerifyError`

#### `impl From(VerifyError) for String`

- `fn from(value: VerifyError) -> Self`

## `dict`

dict — the associative map.

The core operations are native primitives (intercepted by both backends; the bodies are self-recursive placeholders giving the type checker their signatures): `dict.new`, `dict.insert`, `dict.get_or`, `dict.at`, `dict.update`, `dict.contains_key`, `dict.remove`, `dict.keys`, `dict.values`, `dict.pairs`, `dict.length`. The rest is the compositional layer — a lookup returning `Option`, constructors from pairs, and the map/filter/merge transforms.

#### `fn new() -> Dict(k, v)`

An empty Dict.

#### `fn insert(var d: unique Dict(k, v), key: k, val: v) -> Option(v) where k: Eq`

Insert `key` with `val`, returning the displaced value when the key existed. The dictionary performs one semantic key search. A uniquely owned dictionary preserves its hash index and updates or grows geometrically; a shared root is copied before repair so aliases keep their old contents in normal mode. The `unique` receiver makes that cost a checked contract in `mode opt`: a call whose dictionary was aliased or loaned is rejected with the ownership reason.

#### `fn get_or(d: Dict(k, v), key: k, default: v) -> v where k: Eq`

The value for `key`, or `default` when absent.

#### `fn at(d: Dict(k, v), key: k) -> v where k: Eq`

The value for `key`, or a runtime error when absent. This is the read half of the `d[key]` subscript surface; use `get`/`get_or` when absence is ordinary.

#### `fn update(var d: Dict(k, v), key: k, default: v, f: fn(v) -> v) where k: Eq`

#### `fn contains_key(d: Dict(k, v), key: k) -> Bool where k: Eq`

Whether `key` is present. The `_key` suffix is deliberate: a Dict contains key/value pairs, so bare `contains` would be ambiguous between keys and values.

#### `fn remove(var d: unique Dict(k, v), key: k) -> Option(v) where k: Eq`

Remove `key`, returning its old value when present. The dictionary performs one semantic key search. Unique storage moves the old value out and repairs insertion order in place; shared storage uses copy-on-write in normal mode. In `mode opt`, the `unique` receiver rejects a shared or loaned call site instead of silently taking that copy.

#### `fn keys(d: Dict(k, v)) -> List(k)`

The keys, in insertion order.

#### `fn values(d: Dict(k, v)) -> List(v)`

The values, in insertion order.

#### `fn pairs(d: Dict(k, v)) -> List((k, v))`

The (key, value) pairs, in insertion order.

#### `fn length(d: Dict(k, v)) -> Int`

The number of entries.

#### `fn from_pairs(entries: List((k, v))) -> Dict(k, v) where k: Eq`

Build a Dict from (key, value) pairs; a later pair overrides an earlier one.

#### `Dict.length() -> Int`

The number of entries.

#### `Dict.is_empty() -> Bool`

Whether the dict has no entries.

#### `Dict.keys() -> List(k)`

The keys, in insertion order.

#### `Dict.values() -> List(v)`

The values, in insertion order.

#### `Dict.pairs() -> List((k, v))`

The (key, value) pairs, in insertion order.

#### `Dict.values_where(pred: fn(k) -> Bool) -> List(v)`

The values whose keys satisfy `pred`, in the Dict's iteration order.

#### `Dict.insert(key: k, val: v) -> Option(v)`

Insert `key` with `val`, returning the displaced value when the key existed. The dictionary performs one semantic key search. A uniquely owned dictionary preserves its hash index and updates or grows geometrically; a shared root is copied before repair so aliases keep their old contents in normal mode. The `unique` receiver makes that cost a checked contract in `mode opt`: a call whose dictionary was aliased or loaned is rejected with the ownership reason.

#### `Dict.update(key: k, default: v, f: fn(v) -> v)`

#### `Dict.remove(key: k) -> Option(v)`

Remove `key`, returning its old value when present. The dictionary performs one semantic key search. Unique storage moves the old value out and repairs insertion order in place; shared storage uses copy-on-write in normal mode. In `mode opt`, the `unique` receiver rejects a shared or loaned call site instead of silently taking that copy.

#### `Dict.get(key: k) -> Option(v)`

A lookup that says whether the key was present, rather than forcing a default.

#### `Dict.get_or(key: k, default: v) -> v`

The value for `key`, or `default` when absent.

#### `Dict.at(key: k) -> v`

The value for `key`, or a runtime error when absent. This is the read half of the `d[key]` subscript surface; use `get`/`get_or` when absence is ordinary.

#### `Dict.contains_key(key: k) -> Bool`

Whether `key` is present. The `_key` suffix is deliberate: a Dict contains key/value pairs, so bare `contains` would be ambiguous between keys and values.

#### `Dict.map_values(f: fn(v) -> w) -> Dict(k, w)`

A new Dict with every value passed through `f` (keys unchanged).

#### `Dict.filter(keep: fn(k, v) -> Bool) -> Dict(k, v)`

Keep only the entries for which `keep(key, value)` holds.

#### `Dict.merge(other: Dict(k, v)) -> Dict(k, v)`

`self` with `other`'s entries laid over it (on a key collision, `other` wins).

#### `Dict.invert() -> Dict(v, k)`

Swap keys and values. With duplicate values, a later entry wins.

## `duration`

Pure helpers for the built-in `Duration` type — a length of time, written as a literal like `30s`, `2hr`, or `500ms`. Durations are combined and compared with the language operators (`a + b`, `d * 3`, `a < b`); this module adds construction from plain numbers, component access, and human formatting. Capability-free, so it compiles to WASM. A Duration is carried as whole milliseconds (`int_to_duration`/`duration_to_int` are the Int<->Duration bridge).

#### `type DurationParseError`

Matchable duration parse failures. The payloads keep the original input, and `UnitWithoutCount` also carries the unit token that caused the failure.

- `DurationOverflow(String)`
- `InvalidDurationShape(String)`
- `UnitWithoutCount(String, String)`
- `TrailingUnitlessNumber(String)`
- `EmptyDuration(String)`

#### `fn parse_error_message(e: DurationParseError) -> String`

#### `fn milliseconds(n: Int) -> Duration`

Construct from a count of one unit. Numeric constructors are convenience contracts: they return `Duration`, but abort on overflow instead of wrapping. Use `parse` for fallible user input.

#### `fn seconds(n: Int) -> Duration`

#### `fn minutes(n: Int) -> Duration`

#### `fn hours(n: Int) -> Duration`

#### `fn days(n: Int) -> Duration`

#### `fn weeks(n: Int) -> Duration`

#### `fn from_clock(h: Int, m: Int, s: Int) -> Duration`

Build a duration from hours, minutes, and seconds. Components may be negative, but scaling and addition abort if the total millisecond count would overflow.

#### `fn to_milliseconds(d: Duration) -> Int`

#### `fn to_seconds(d: Duration) -> Int`

#### `fn to_minutes(d: Duration) -> Int`

#### `fn to_hours(d: Duration) -> Int`

#### `fn to_days(d: Duration) -> Int`

#### `fn to_weeks(d: Duration) -> Int`

#### `fn max(a: Duration, b: Duration) -> Duration`

The longer of two durations.

#### `fn min(a: Duration, b: Duration) -> Duration`

The shorter of two durations.

#### `fn is_zero(d: Duration) -> Bool`

Whether the duration is exactly zero.

#### `fn abs(d: Duration) -> Duration`

The magnitude of a duration (a negative span, e.g. from subtraction, is made positive). The most-negative representable value has no positive counterpart in 64 bits (negating it would wrap back to itself), so it saturates to the largest positive magnitude rather than staying negative.

#### `fn part_hours(d: Duration) -> Int`

The hours component (everything an hour and above).

#### `fn part_minutes(d: Duration) -> Int`

The minutes component (0..59).

#### `fn part_seconds(d: Duration) -> Int`

The seconds component (0..59).

#### `fn part_milliseconds(d: Duration) -> Int`

The milliseconds component (0..999).

#### `fn clock(d: Duration) -> String`

A clock string "H:MM:SS": minutes and seconds zero-padded, hours in full (so a long span reads as e.g. "100:00:00"); the sub-second part is dropped. A negative span renders as a single leading "-" over the absolute components ("-0:00:01"), never as negative fields inside the clock.

#### `fn human(d: Duration) -> String`

A compact label that omits leading zero units: `1h1m1s`, `1m30s`, `5s`, and `500ms` for a pure sub-second span. A negative span (e.g. from subtraction) keeps its sign as a single leading "-" over the absolute magnitude: `-1s`, `-1m30s` — not the truncated-division fields of the raw negative count.

#### `fn parse(s: String) -> Result(Duration, DurationParseError)`

Parse a duration string to a `Duration` — the inverse of `human`. Accepts unit-tagged input ("1h2m3s", "500ms", "2hr", any subset) using ms/s/m/h/hr/d/w, and a bare number as plain milliseconds. `Err` on a stray character, a unit with no preceding count ("ms", "1hms"), a dangling unit-less number after units were given ("1h30"), or a value that overflows a 64-bit millisecond count. The typed error lets libraries classify malformed input without parsing display text.

#### `fn parse_string(s: String) -> Result(Duration, String)`

Parse with String errors for application-style boundaries.

### Trait implementations

#### `impl Show for DurationParseError`

- `fn show(self) -> String`

#### `impl Error for DurationParseError`

#### `impl From(DurationParseError) for String`

- `fn from(value: DurationParseError) -> Self`

## `dynamic`

Checked runtime values (RFC-0082 Stage 1).

`Dynamic` is an explicit owned boundary. Its payload is the representation-safe RFC-0081 existential envelope for `Reflect`; its descriptor is generated by the compiler from authenticated declaration identity rather than reconstructed from this display name.

#### `sealed type RuntimeType`

- `RuntimeType(Int, String)`

#### `sealed type Dynamic`

- `Dynamic(RuntimeType, dyn Reflect)`

#### `sealed type RuntimeField`

- `RuntimeField(String, RuntimeType)`

#### `sealed type RuntimeMethod`

- `RuntimeMethod(String, List(RuntimeType), RuntimeType, List(String))`

#### `sealed type DynamicFieldStatus`

- `FieldFound(RuntimeType)`
- `FieldPrivate`
- `FieldSealed`
- `FieldMissing`
- `FieldMalformed`

#### `type DynamicError`

Checked failures remain ordinary matchable data and never become unchecked casts or backend traps.

- `TypeMismatch(RuntimeType)`
- `MissingField(String)`
- `MissingMethod(String)`
- `ArityMismatch(String, Int, Int)`
- `ArgumentMismatch(Int, RuntimeType, RuntimeType)`
- `ResultMismatch(RuntimeType, RuntimeType)`
- `TraitMismatch(RuntimeType)`
- `CapabilityDenied(String)`
- `PrivateField(String)`
- `SealedType(RuntimeType)`
- `MalformedRequest(String)`
- `MalformedDescriptor(RuntimeType)`
- `MalformedPayload(RuntimeType)`

#### `fn dynamic(own value: a) -> Dynamic where a: Reflect`

Convert one reflectable value into an owned dynamic envelope. The compiler replaces `__dynamic_descriptor` with an immutable descriptor constant and rejects direct or transitive capability payloads before either backend runs.

#### `fn type_of(value: Dynamic) -> RuntimeType`

Return the immutable descriptor carried by a dynamic value.

#### `fn type_name(ty: RuntimeType) -> String`

Human-readable only. Descriptor equality and decoding never key on this name.

#### `fn fields(ty: RuntimeType) -> List(RuntimeField)`

Declared public readable fields in source order. The compiler replaces the intrinsic with an authenticated lookup over canonical descriptor IDs.

#### `fn field_name(field: RuntimeField) -> String`

#### `fn field_type(field: RuntimeField) -> RuntimeType`

#### `fn methods(ty: RuntimeType) -> List(RuntimeMethod)`

Public methods explicitly registered with `@dynamic`. The compiler replaces this lookup with immutable data from the closed authenticated method plan.

#### `fn method_name(method: RuntimeMethod) -> String`

#### `fn method_args(method: RuntimeMethod) -> List(RuntimeType)`

#### `fn method_result(method: RuntimeMethod) -> RuntimeType`

#### `fn method_capabilities(method: RuntimeMethod) -> List(String)`

#### `fn call(value: Dynamic, name: String, args: List(Dynamic)) -> Result(Dynamic, DynamicError)`

Invoke only a descriptor-registered method. Arguments are checked against exact authenticated descriptors before the compiler-generated typed call.

#### `fn call_with(value: Dynamic, name: String, args: List(Dynamic), capabilities: c) -> Result(Dynamic, DynamicError)`

Capability-bearing methods use a separate statically typed authority bundle. One capability is passed directly; multiple capabilities use a tuple in the same order as the reflected method parameters. Capabilities never enter `Dynamic` or the ordinary argument list.

#### `fn implements(value: Dynamic, trait_type: RuntimeType) -> Bool`

Query an authenticated `dyn Trait` descriptor produced by `dynamic.runtime_type(dyn Trait)`. The compiler closes this relation over the linked RFC-0081 impl catalog; display names never participate.

#### `fn as_trait(value: Dynamic, trait_type: RuntimeType) -> Result(Dynamic, DynamicError)`

Validate the same closed relation while retaining the original owned dynamic envelope. Trait identity is evidence for later reflection, not an unchecked payload cast.

#### `fn field(value: Dynamic, name: String) -> Result(Dynamic, DynamicError)`

#### `fn try_decode(value: Dynamic) -> Option(a)`

Decode only when the inferred expected type has the exact canonical descriptor. The compiler specializes this private call after generic monomorphization.

#### `fn decode(value: Dynamic) -> Result(a, DynamicError)`

Decode to the result type inferred from the surrounding static context. A mismatch retains the actual canonical descriptor for diagnostics and recovery.

## `encoding`

encoding — hex and base64 for text conveniences and raw Bytes payloads.

The byte-level codecs need access witchy source cannot express directly, so the raw transforms are native intrinsics (like `crypto`): the private helpers below are placeholders the runtime never executes — each is intercepted by its qualified name and run in Rust on both backends.

Encoding is total, so the encoders return a plain `String`. Decoding can fail, so the public `*decode` functions guard the raw codec with a pure-witchy alphabet check and return `Result` (RFC-0044): valid input decodes to `Ok`, and any non-alphabet character or a truncated final group is a reachable `Err` — never a silent truncation (the JWT/WebAuthn segment-decoding hazard BUG-006 named).

#### `type EncodingError`

Matchable decode failures for the text encodings this module supports.

- `InvalidHex(String)`
- `InvalidBase64(String)`
- `InvalidBase64Url(String)`

#### `fn encoding_error_message(e: EncodingError) -> String`

#### `fn hex_encode(data: String) -> String`

Lowercase hex of `data`'s UTF-8 bytes.

#### `fn hex_encode_bytes(data: Bytes) -> String`

Lowercase hex of raw bytes.

#### `fn hex_decode(data: String) -> Result(String, EncodingError)`

Decode a hex string (an even count of `0-9a-fA-F` digits) back to text (lossy UTF-8 for non-text payloads), or a typed `Err` when it is not hex.

#### `fn hex_decode_bytes(data: String) -> Result(Bytes, EncodingError)`

Decode a hex string (an even count of `0-9a-fA-F` digits) to raw bytes, or a typed `Err` when it is not hex.

#### `fn hex_decode_string(data: String) -> Result(String, String)`

Decode hex with String errors for application-style boundaries.

#### `fn hex_decode_bytes_string(data: String) -> Result(Bytes, String)`

Decode hex bytes with String errors for application-style boundaries.

#### `fn base64_encode(data: String) -> String`

Standard base64 (with `=` padding) of `data`'s UTF-8 bytes.

#### `fn base64_encode_bytes(data: Bytes) -> String`

Standard base64 (with `=` padding) of raw bytes.

#### `fn base64url_encode_bytes(data: Bytes) -> String`

base64url (no padding; `-`/`_`) of raw bytes.

#### `fn base64_decode(data: String) -> Result(String, EncodingError)`

Decode standard base64 (the `A-Za-z0-9+/` alphabet, `=` padding) back to text (lossy UTF-8), or a typed `Err` when it is not valid base64.

#### `fn base64_decode_bytes(data: String) -> Result(Bytes, EncodingError)`

Decode standard base64 (the `A-Za-z0-9+/` alphabet, `=` padding) to raw bytes, or a typed `Err` when it is not valid base64.

#### `fn base64_decode_string(data: String) -> Result(String, String)`

Decode base64 with String errors for application-style boundaries.

#### `fn base64_decode_bytes_string(data: String) -> Result(Bytes, String)`

Decode base64 bytes with String errors for application-style boundaries.

#### `fn hex_to_base64url(hex: String) -> Result(String, EncodingError)`

base64url (no padding; `-`/`_`) of the bytes given as a HEX string, or an `Err` naming the input when it is not valid hex. The hex indirection lets binary round-trip through UTF-8 strings — e.g. a WebAuthn `clientDataJSON.challenge` is base64url of the raw challenge bytes. Fallible like `hex_decode` (RFC-0044): malformed hex is a reachable `Err`, never the silent drop the raw codec would do.

#### `fn hex_to_base64url_string(hex: String) -> Result(String, String)`

Convert hex to base64url with String errors for application-style boundaries.

#### `fn base64url_decode(data: String) -> Result(String, EncodingError)`

Decode base64url (URL-safe `-`/`_`, no padding) back to text (lossy UTF-8) — the JSON header/payload segments of a JWT/OIDC identity token — or an `Err` naming the input when it is not valid base64url.

#### `fn base64url_decode_bytes(data: String) -> Result(Bytes, EncodingError)`

Decode base64url (URL-safe `-`/`_`, no padding) to raw bytes, or a typed `Err` when it is not valid base64url.

#### `fn base64url_decode_string(data: String) -> Result(String, String)`

Decode base64url with String errors for application-style boundaries.

#### `fn base64url_decode_bytes_string(data: String) -> Result(Bytes, String)`

Decode base64url bytes with String errors for application-style boundaries.

#### `fn base64url_to_hex(data: String) -> Result(String, EncodingError)`

Decode base64url to a HEX string — for binary that must round-trip through a witchy String, e.g. a JWT's RS256 signature fed to `crypto.rsa_pkcs1_sha256_verify` — or an `Err` naming the input when it is not valid base64url.

#### `fn base64url_to_hex_string(data: String) -> Result(String, String)`

Decode base64url to hex with String errors for application-style boundaries.

### Trait implementations

#### `impl Show for EncodingError`

- `fn show(self) -> String`

#### `impl Error for EncodingError`

#### `impl From(EncodingError) for String`

- `fn from(value: EncodingError) -> Self`

## `error`

The common bound for typed errors.

Errors are ordinary values carried in `Result(_, e)`. Library-specific error enums should implement `Show` for display, `Error` for the conventional bound, and `From(source)` for each lower-level error they want `?` to propagate.

#### `trait Error: Show`


### Trait implementations

#### `impl Error for String`

## `exec`

The `Exec` capability: spawn a confined native subprocess. The executable is named through a `Dir[Read]` — you can only run a file you can read — so `run` takes both the `Exec` right and the `Dir` the binary lives under. Pure data otherwise; the only authority is the `Exec`/`Dir` it is handed.

This wraps the low-level `exec` primitive (which takes a single `\0`-joined argv string and returns a `"<exit_code>\n<output>"` payload) with a `List(String)` argv and a parsed `(Int, String)` result, where `output` is the child's stdout followed by its stderr. See rfcs/0004-self-hosted-cli.md.

#### `fn run(e: Exec, dir: Dir[Read], path: String, args: List(String), stdin: String) -> (Int, String)`

Run `path` (resolved within `dir`) with `args` and `stdin`, returning `(exit_code, output)`.

#### `fn run_args(e: Exec, dir: Dir[Read], path: String, args: List(String)) -> (Int, String)`

Run `path` with `args` and no stdin — the common case.

## `fs`

fs — small directory helpers over the `Dir` capability. Each function's authority is exactly the `Dir` the caller passes: `collect_files`/`parent_dir` need only read, `ensure_dir` needs write. Nothing here widens a capability — the confinement of the `Dir` you hand in is preserved.

#### `fn ensure_dir(root: Dir, path: String)`

Create each level of `path` under `root` (the confined `Dir`'s `make_dir` is not recursive). Idempotent. Needs a writable `Dir`.

#### `fn parent_dir(path: String) -> Option(String)`

The parent component of a relative path as `Some`: "a/b/c" -> Some("a/b"). A single component has no parent, so "a" is `None` (RFC-0044 rule 1: absence is `Option`, never an empty-string sentinel). Callers ensuring a parent exists can default explicitly: `fs.parent_dir(p) ?? ""` (ensure_dir of "" creates nothing).

#### `fn collect_files(root: Dir, path: String, rel: String, ext: String) -> List((String, String))`

Recursively collect every file under `path` whose name ends with `ext`, as (relpath, contents) pairs. `rel` is prefixed to each result path (so a nested `a/b.witchy` keeps its full relative path). An empty `path` walks from the Dir root itself. Needs a readable `Dir`.

## `func`

The witchy standard function-combinator library. Pure and capability-free. With first-class functions these build new functions from existing ones without writing wrapper lambdas by hand.

#### `fn identity(x: a) -> a`

Return the argument unchanged.

#### `fn compose(f: fn(b) -> c, g: fn(a) -> b) -> fn(a) -> c`

`compose(f, g)` is the function `x -> f(g(x))` — apply `g`, then `f`.

#### `fn flip(f: fn(a, b) -> c) -> fn(b, a) -> c`

`flip(f)` applies the two-argument `f` with its arguments swapped.

#### `fn on_key(op: fn(b, b) -> c, key: fn(a) -> b) -> fn(a, a) -> c`

`on_key(op, key)` is the function `(x, y) -> op(key(x), key(y))` — run a two-argument `op` on the projections of two values. Pairs with the comparator-taking list functions: `list.sort_by(people, func.on_key(fn(a, b): a < b, person_age))` sorts by age. (Named `on_key` because bare `on` is a keyword.)

#### `fn constant(x: a) -> fn(b) -> a`

A function that ignores its argument and always returns `x`.

#### `fn first(p: (a, b)) -> a`

The first / second component of a pair — handy for the tuples `iter.zip` and `iter.enumerate` produce, without writing a `match` each time.

#### `fn second(p: (a, b)) -> b`

## `future`

std/future — cooperative single-threaded futures via CPS over closures.

A `Future(a)` is a thunk that, when polled, either completes (`Done`) or hands back the rest of the work (`More`). It is a standalone building block for racing and joining independent computations (`select`, `join_all`) — NOT what the `async`/`await` surface lowers onto (that targets `std/task`, which points here for `select`). A future's live state is the captured values of its continuation closure — owned values, never internal references, so unlike Rust there is nothing self-referential and no `Pin`. Pure structure (closures + sum types), so it runs byte-identically on both backends.

`More` is the cooperative yield point: an executor drives a future by polling it one step at a time, so several futures can interleave at their `pending` points.

#### `type Poll`

The result of polling a future once: either the final value, or the rest.

- `Done(a)`
- `More(Future(a))`

#### `type Future`

A future is a thunk producing its next `Poll`. Pull it with `poll`.

- `Future(fn() -> Poll(a))`

#### `type Slot`

A scheduling slot: a task still running, or its finished result.

- `Running(Future(a))`
- `Finished(a)`

#### `fn poll(f: Future(a)) -> Poll(a)`

#### `fn ready(x: a) -> Future(a)`

An already-complete future.

#### `fn pending(x: a) -> Future(a)`

Completes with `x`, but yields control once first — a cooperative scheduling point an executor can interleave other tasks across.

#### `fn and_then(f: Future(a), k: fn(a) -> Future(b)) -> Future(b)`

Sequence: run `f` to completion, then continue with `k` applied to its result. The CPS analogue of `let y = await f; k(y)` — continuation-passing over futures.

#### `fn map(f: Future(a), g: fn(a) -> b) -> Future(b)`

Transform the result of a future.

#### `fn defer(thunk: fn() -> a) -> Future(a)`

Run `thunk` at poll time and complete with its result. Unlike `ready`, which captures an already-computed value, `defer` delays the work (and any effects in it) until this future is polled. A `Future` is a replayable execution recipe, not a memo cell: polling the same `defer` value again reruns `thunk`.

#### `fn lazy(thunk: fn() -> Future(a)) -> Future(a)`

Build the future `thunk()` lazily: nothing runs until this future is polled. The usual drivers replace a `More` result with its continuation, so a normal drive path enters the lazy wrapper once. The value itself is not consumed or memoized, though: polling the same `Future` value again reruns `thunk`.

#### `fn ready_unit() -> Future(Nil)`

An already-complete `Future(Nil)` — the lowering target for an async body that falls off its end.

#### `fn yield_now() -> Future(Nil)`

Yield control to the executor once, then continue. `await yield_now()` is a cooperative scheduling point an async task uses to let its peers run.

#### `fn block_on(f: Future(a)) -> a`

Drive a single future to completion and return its value. Recursive on `More` so it needs no initial `a` (there is no default value for a generic type).

#### `fn join_all(tasks: List(Future(a))) -> List(a)`

Drive every task in `tasks` concurrently to completion, returning their results in the original order. The schedule is a deterministic round-robin — one poll step per task per round — so it is single-threaded and fixed-order, and both backends produce byte-identical output. Tasks interleave at their `pending`/ `await` points. This is the structured-concurrency primitive (`scope`/`join`): hand it the task list and it fans them out, joining all before it returns.

#### `fn select(tasks: List(Future(a))) -> (Int, a)`

Race `tasks`: drive them concurrently until the FIRST one finishes, and return its index and value; the losers are simply dropped (no further polling — and because futures are pure and lazy, dropping is cancellation, no cleanup hook needed). On a tie within a round the lowest index wins, so the result is deterministic and both backends agree. This is the `select` of structured concurrency (e.g. race a task against `sleep` for a timeout).

## `http`

Portable HTTP request/response types and a Fetch-based client.

Client authority is the origin-scoped `Fetch` root. DNS resolution, pinned dialing, redirects, timeouts, and response-size limits are host-provider responsibilities, so browser and native callers share this exact API. The `server` module continues to use the Request/Response types and parsing helpers below; HTTP servers remain on Net[Listen].

#### `type HttpError`

Uniform failures produced by every Fetch provider, plus strict response parsing failures in this module.

- `Denied(String)`
- `InvalidRequest(String)`
- `Timeout`
- `Redirect(String)`
- `Network(String)`
- `ProviderMalformedResponse(String)`
- `ResponseTooLarge(String)`
- `UnknownProviderFailure(String, String)`
- `MalformedResponse(ResponseParseError)`

#### `type ResponseParseError`

Matchable strict-response parse failures. These are surfaced through `HttpError.MalformedResponse` by client APIs and `try_parse_response`.

- `MissingChunkSizeTerminator`
- `InvalidChunkSize(String)`
- `MissingFinalChunkDelimiter`
- `InvalidChunkUtf8`
- `TruncatedChunk`
- `MissingChunkDelimiter`
- `InternalChunkDecoderState`

#### `type Response`

A response: status code, headers (name lowercased for case-insensitive lookup), and body. Shared by the client and the `server` framework.

- `Response(Int, List((String, String)), String)`

#### `type Request`

A parsed request the server hands to a handler: method, raw path, the path params the router captured (`:id`), the parsed query string, the (lowercased) headers, and the body.

- `Request(String, String, List((String, String)), List((String, String)), List((String, String)), String)`

#### `type RequestBuilder`

- `RequestBuilder(String, String, List((String, String)), String)`

#### `fn response_parse_error_message(e: ResponseParseError) -> String`

#### `fn http_error_message(e: HttpError) -> String`

#### `fn origin(raw: String) -> String`

Canonical origin text for deriving Fetch from Net. Invalid or unsupported URLs are configuration errors here; request-time URL failures remain typed.

#### `fn request_with(fetch: Fetch, method: String, request_url: String, headers: List((String, String)), body: String) -> Response`

Perform one request and trap on a provider/response failure. Use `try_request_with` when network weather belongs in application control flow.

#### `fn try_request_with(fetch: Fetch, method: String, request_url: String, headers: List((String, String)), body: String) -> Result(Response, HttpError)`

#### `fn get(fetch: Fetch, request_url: String) -> Response`

#### `fn post(fetch: Fetch, request_url: String, body: String) -> Response`

#### `fn put(fetch: Fetch, request_url: String, body: String) -> Response`

#### `fn delete(fetch: Fetch, request_url: String) -> Response`

#### `fn patch(fetch: Fetch, request_url: String, body: String) -> Response`

#### `fn head(fetch: Fetch, request_url: String) -> Response`

#### `fn try_get(fetch: Fetch, request_url: String) -> Result(Response, HttpError)`

#### `fn try_post(fetch: Fetch, request_url: String, body: String) -> Result(Response, HttpError)`

#### `fn build(method: String, request_url: String) -> RequestBuilder`

#### `fn get_request(request_url: String) -> RequestBuilder`

#### `fn post_request(request_url: String) -> RequestBuilder`

#### `fn put_request(request_url: String) -> RequestBuilder`

#### `fn delete_request(request_url: String) -> RequestBuilder`

#### `fn patch_request(request_url: String) -> RequestBuilder`

#### `fn is_success(r: Response) -> Bool`

Whether the response status is in the 2xx success range.

#### `fn status(r: Response) -> Int`

The numeric status code, e.g. 200.

#### `fn body(r: Response) -> String`

The response body.

#### `fn header(r: Response, name: String) -> Option(String)`

The value of a response header, looked up case-insensitively, or None.

#### `fn find_header(hdrs: List((String, String)), name: String) -> Option(String)`

Look up a header by its (already-lowercased) name. Shared with `server`.

#### `fn has_crlf(s: String) -> Bool`

Whether `s` contains a CR or LF — the response/request-splitting bytes.

#### `fn check_field(what: String, value: String)`

Trap unless `value` (a header value) is free of forbidden controls. HTAB is allowed in header values; all other C0 controls and DEL are rejected. Request method/path/host use `check_request_field`, which is stricter.

#### `fn check_header_name(name: String)`

Trap unless `name` is a valid header-name token (rejects `:`, space, and CR/LF).

#### `fn check_header(name: String, value: String)`

Validate one `(name, value)` header pair — the name is a token, the value has no forbidden controls. Shared by the client request builder and server renderer.

#### `fn check_request_field(what: String, value: String)`

(BUG-364/506) Trap unless `value` is safe to splice into the request LINE: no C0 controls, DEL, space, or tab. The request line is space-delimited (`METHOD SP TARGET SP HTTP/1.1`), so a space/tab in method/path/host would split it into extra tokens (request smuggling). Stricter than `check_field`, because header values legitimately contain spaces and may contain HTAB.

#### `fn is_framing_header(name: String) -> Bool`

(BUG-358 / BUG-393) Whether `name` is a message-FRAMING header — Content-Length, Transfer-Encoding, or Connection. The renderer owns framing (it appends its own Content-Length / Connection), so a caller/handler-supplied framing header must be dropped rather than emitted alongside ours: two conflicting framing headers are a request/response-smuggling primitive.

#### `fn parse_response(raw: String) -> Response`

Parse a raw HTTP/1.1 response string into a `Response`: split at the blank line separating headers from body, parse the header lines into (lowercased name, trimmed value) pairs, decode a `chunked` body, and read the status code totally (a non-numeric or overflowing code becomes 0 rather than trapping). Public so a proxy or test can parse a response it obtained by other means. This total helper keeps the historical lossy chunked behavior; use `try_parse_response` at trust boundaries where malformed framing must be an error.

#### `fn try_parse_response(raw: String) -> Result(Response, HttpError)`

Strict response parsing for public client paths. In particular, malformed `Transfer-Encoding: chunked` bodies return `Err` instead of handing callers a recovered prefix that may look like valid JSON.

#### `fn try_parse_response_string(raw: String) -> Result(Response, String)`

#### `RequestBuilder.with_header(name: String, value: String) -> RequestBuilder`

#### `RequestBuilder.with_body(body: String) -> RequestBuilder`

#### `RequestBuilder.with_query(key: String, value: String) -> RequestBuilder`

#### `RequestBuilder.send(fetch: Fetch) -> Result(Response, HttpError)`

### Trait implementations

#### `impl Show for ResponseParseError`

- `fn show(self) -> String`

#### `impl Error for ResponseParseError`

#### `impl From(ResponseParseError) for String`

- `fn from(value: ResponseParseError) -> Self`

#### `impl Show for HttpError`

- `fn show(self) -> String`

#### `impl Error for HttpError`

#### `impl From(HttpError) for String`

- `fn from(value: HttpError) -> Self`

## `iter`

std/iter — lazy, pull-based iterators: the witchy take on Rust's Iterator, minus the part Rust most regrets. Because witchy values are "data" (no borrowing), there is no lending-iterator / GAT complexity: an `Iter(a)` is just a thunk that produces the next `Step`. Adapters (`map`/`filter`/ `take_while`/...) are lazy and compose without building intermediate lists; consumers (`collect`/`fold`/`find`/`count`) drive the pulling. Infinite iterators are fine (`count_from`, `repeat`) as long as something bounds them (`take`/`take_while`/`find`). Pure and capability-free; runs on both backends. `gen fn`/`yield` lower to this representation; `from_gen` is the low-level desugaring target for compiler-generated iterators.

Adapters and consumers are METHODS on `Iter` (`it.map(f).take(3)`), so pipelines read left-to-right. The module level keeps only what has no receiver — the constructors (`iter.range`, `iter.from_list`, ...) — plus `iter.collect` (whose polymorphic return type dispatches on the EXPECTED type; keeping it a free function also avoids a measured mono-pass blowup) and the pull primitive `iter.next` (also a method; the free form drives the module's own generic internals).

#### `type Step`

One pull: either exhausted, or a value plus the rest of the iterator.

- `Empty`
- `Item(a, Iter(a))`

#### `type Iter`

An iterator is a thunk producing its next Step. Pull it with `next`.

- `Iter(fn() -> Step(a))`

#### `trait FromIterator(e)`

A type an iterator can be collected INTO. `from_iter` mentions the implementing type only in its result, so a call dispatches on the EXPECTED type — an ascribed binding, a typed parameter, a for-loop — not on any argument.

- `fn from_iter(it: Iter(e)) -> Self`

#### `fn next(it: Iter(a)) -> Step(a)`

The pull primitive. Kept at module level (the method delegates to it): internal step helpers pull from pattern-bound iterators, whose method dispatch the quiet pre-mono pass cannot resolve.

#### `fn empty() -> Iter(a)`

The empty iterator.

#### `fn once(x: a) -> Iter(a)`

One element, then done.

#### `fn unfold(seed: s, f: fn(s) -> Option((a, s))) -> Iter(a)`

Build an iterator from a `seed` by repeatedly applying `f`, which returns `Some((value, next_seed))` to yield a value or `None` to stop. The general generator primitive — count/fibonacci/range are all unfolds.

#### `fn count_from(n: Int) -> Iter(Int)`

The integers n, n+1, n+2, ... (infinite).

#### `fn range(lo: Int, hi: Int) -> Iter(Int)`

The half-open integer range [lo, hi).

#### `fn repeat(x: a) -> Iter(a)`

`x` forever (infinite).

#### `fn from_list(xs: List(a)) -> Iter(a)`

A list's elements, in order.

#### `fn from_gen(f: fn(Int) -> Option(a)) -> Iter(a)`

Build an iterator from an index function: `f(0)`, `f(1)`, ... each `Some(x)` is the next element and the first `None` ends it. This is the de-sugaring target for the `gen`/`yield` syntax: a generator body becomes a function from "which yield" to its value, and pulling element i re-runs it to the i-th yield (so the body may use any control flow, including unbounded loops).

#### `fn collect(it: Iter(a)) -> c where c: FromIterator(a)`

Collect into any FromIterator type, chosen by the call site's expected type (drives the iterator to exhaustion — don't call on an unbounded one):     let xs: List(Int) = iter.collect(it)     let joined: String = iter.collect(pieces)     let s: Set(Int) = iter.collect(it)        # de-duplicates; needs `a: Eq` Free-function-only ON PURPOSE: the polymorphic return type in an impl block causes a measured mono-pass performance explosion.

#### `Iter.next() -> Step(a)`

One pull: `Empty`, or `Item(value, rest)`.

#### `Iter.map(f: fn(a) -> b) -> Iter(b)`

Apply `f` to every element.

#### `Iter.filter(keep: fn(a) -> Bool) -> Iter(a)`

Keep only the elements for which `keep` holds.

#### `Iter.filter_map(f: fn(a) -> Option(b)) -> Iter(b)`

Apply `f` to each element, keeping every `Some(y)` and dropping every `None` — a `map` and `filter` fused into one pass (Rust's `Iterator::filter_map`).

#### `Iter.take(k: Int) -> Iter(a)`

The first `k` elements (fewer if the iterator is shorter).

#### `Iter.take_while(pred: fn(a) -> Bool) -> Iter(a)`

Elements up to (not including) the first one failing `pred`.

#### `Iter.drop(k: Int) -> Iter(a)`

Skip the first `k` elements. Lazy like every adapter: nothing is pulled from the source at construction — the skip runs inside the returned iterator's thunk on first pull, and iteratively, so a large `k` cannot exhaust the stack.

#### `Iter.drop_while(pred: fn(a) -> Bool) -> Iter(a)`

Skip the leading elements while `pred` holds, then yield the rest.

#### `Iter.enumerate() -> Iter((Int, a))`

Pair each element with its index: (0, x0), (1, x1), ...

#### `Iter.zip(other: Iter(b)) -> Iter((a, b))`

Zip with another iterator into pairs, stopping at the shorter one.

#### `Iter.chain(other: Iter(a)) -> Iter(a)`

The elements of `self`, then the elements of `other`.

#### `Iter.flat_map(f: fn(a) -> Iter(b)) -> Iter(b)`

Map each element to an iterator and concatenate the results.

#### `Iter.flatten() -> Iter(b)`

Concatenate an iterator OF iterators into one flat iterator, lazily and in order — `flatten` is `flat_map` with the identity function.

#### `Iter.scan(state: s, f: fn(s, a) -> (s, b)) -> Iter(b)`

A lazy STATEFUL map: thread `state` through `f`, which returns the new state and the value to emit. `xs.scan(0, fn(s, x): (s + x, s + x))` yields the running sums. Unlike `fold`, it produces an iterator, so it is lazy and composable, and unlike `map` it can carry state between elements.

#### `Iter.for_each(f: fn(a) -> Nil)`

Call `f` on every element for its effect (drives to exhaustion). The right consumer for a generator when you don't need to early-exit — no list is built.

#### `Iter.fold(init: b, f: fn(b, a) -> b) -> b`

Left fold over the elements.

#### `Iter.count() -> Int`

Number of elements (drives to exhaustion).

#### `Iter.sum() -> Int`

Sum of an Int iterator.

#### `Iter.split_first() -> Option((a, Iter(a)))`

Split an iterator into its first element and the rest, or None if it is empty. The building block for writing your own recursive iterator transforms (e.g. a prime sieve): pair it with `unfold`, which threads the "rest" as its seed.

#### `Iter.find(pred: fn(a) -> Bool) -> Option(a)`

The first element satisfying `pred`, or None (stops at the first match, so it is safe on an unbounded iterator if a match exists).

#### `Iter.any(pred: fn(a) -> Bool) -> Bool`

Whether at least one element satisfies `pred` — stops (short-circuits) at the first match, so it terminates on an unbounded iterator once one is found. `false` for the empty iterator.

#### `Iter.all(pred: fn(a) -> Bool) -> Bool`

Whether every element satisfies `pred` — stops at the first failure. `true` for the empty iterator (vacuously). Don't call on an unbounded iterator whose elements all satisfy `pred`: it never stops.

#### `Iter.last() -> Option(a)`

The last element (drives the iterator to exhaustion), or None if it is empty. Don't call on an unbounded iterator — it never stops.

#### `Iter.position(pred: fn(a) -> Bool) -> Option(Int)`

The 0-based index of the first element satisfying `pred`, or None. Stops at the first match, so it is safe on an unbounded iterator if a match exists.

#### `Iter.min() -> Option(a)`

The smallest element by the type's `Ord`, or None if the iterator is empty (drives to exhaustion; don't call on an unbounded iterator).

#### `Iter.max() -> Option(a)`

The largest element by the type's `Ord`, or None if the iterator is empty (drives to exhaustion; don't call on an unbounded iterator).

### Trait implementations

#### `impl FromIterator(a) for List(a)`

- `fn from_iter(it: Iter(a)) -> List(a)`

#### `impl FromIterator((k, v)) for Dict(k, v) where k: Eq`

- `fn from_iter(it: Iter((k, v))) -> Dict(k, v)`

#### `impl FromIterator(a) for Set(a) where a: Eq`

A conditional impl: collecting into a `Set` needs `Eq` to deduplicate, so the `where a: Eq` rides on the impl head (the trait method itself stays bound-free).

- `fn from_iter(it: Iter(a)) -> Set(a)`

#### `impl FromIterator(String) for String`

- `fn from_iter(it: Iter(String)) -> String`

## `json`

A JSON library — the witchy take on Go's encoding/json. This slice is the value type and the encoder (serialization); the decoder (parsing) follows. Pure and capability-free, so — unlike networking — it compiles to WASM like the rest of the data std.

#### `type Json`

- `JsonNull`
- `JsonBool(Bool)`
- `JsonInt(Int)`
- `JsonFloat(Float)`
- `JsonString(String)`
- `JsonArray(List(Json))`
- `JsonObject(List((String, Json)))`

#### `type DecodeError`

- `DecodeError { message: String }`

#### `type DeserializeError`

- `DeserializeMissingField(String)`
- `DeserializeExpected(String)`

#### `trait Deserialize`

Reconstruct a value from a `Json`. Reflection can read a value's structure but not rebuild a value from structure, so `derive(Deserialize)` generates `from_json` per type. Encoding needs no trait: `from_value`, `stringify`, and `x.into()` already serialize any `Reflect` value.

- `fn from_json(j: Json) -> Result(Self, DeserializeError)`

#### `fn decode_error_message(e: DecodeError) -> String`

#### `fn deserialize_error_message(e: DeserializeError) -> String`

#### `fn decode(s: String) -> Result(Json, DecodeError)`

Parse a complete JSON document, or return an error message. The whole input must be a single value: trailing content after it (other than whitespace) is rejected, so `decode("1 2")` is an Err rather than silently yielding `1`.

#### `fn optional(o: Option(Json), each: fn(Json) -> Result(a, e)) -> Result(Option(a), e)`

Decode an optional field: an absent key or an explicit `null` is `None`; otherwise the value is decoded via `each`. Used for `Option(_)` fields.

#### `fn object_sorted(pairs: List((String, Json))) -> Json`

Build a JSON object whose keys are sorted (matching a serialized BTreeMap), e.g. TUF `targets`. Use this only for dynamic key/value sets whose order must be deterministic for signing; reflectively-encoded records keep their declared field order instead.

#### `fn from_option(o: Option(a), each: fn(a) -> Json) -> Json`

Encode an `Option` as payload-or-`null` — `Some(x)` through `each`, `None` as `JsonNull`. Keeps a derived `to_json`'s Option field a single-line call. (The param is `each`, not `encode`, so it doesn't shadow `json.encode`.)

#### `fn from_value(x: a) -> Json where a: Reflect`

`from_value(x)` encodes a value to `Json` by reflecting over its structure, so it works for any type with no derive. `stringify(x)` returns the encoded string.

#### `fn stringify(x: a) -> String where a: Reflect`

#### `Json.encode() -> String`

Serialize to the compact textual form.

#### `Json.encode_pretty() -> String`

Serialize with 2-space indentation, for human-readable output. Empty arrays and objects stay on one line (`[]` / `{}`).

#### `Json.get(key: String) -> Option(Json)`

Look up a key in a JSON object.

#### `Json.contains_key(key: String) -> Bool`

Whether a JSON object has `key` (false for non-objects).

#### `Json.merge(other: Json) -> Json`

A shallow merge of two JSON objects: every key of `other` overrides the same key in `self`, and `self`'s other keys are kept. If either value is not an object, `other` wins (so it works as "patch `self` with `other`"). Override is top-level only — nested objects are replaced, not deep-merged.

#### `Json.index(i: Int) -> Option(Json)`

The element at index `i` of a JSON array.

#### `Json.as_int() -> Option(Int)`

`self` as an integer, when it is one.

#### `Json.as_string() -> Option(String)`

`self` as a string, when it is one.

#### `Json.as_bool() -> Option(Bool)`

`self` as a bool, when it is one.

#### `Json.as_array() -> Option(List(Json))`

`self` as a list of elements, when it is an array.

#### `Json.as_object() -> Option(List((String, Json)))`

`self` as its key/value pairs, when it is an object — for iterating an object whose keys aren't known ahead of time.

#### `Json.require(key: String) -> Result(Json, DeserializeError)`

The value at `key`, or `Err(DeserializeMissingField)` when absent.

#### `Json.int_of() -> Result(Int, DeserializeError)`

Coerce a Json value to a scalar, or `Err` describing the expected shape.

#### `Json.string_of() -> Result(String, DeserializeError)`

#### `Json.bool_of() -> Result(Bool, DeserializeError)`

#### `Json.float_of() -> Result(Float, DeserializeError)`

A JSON number with no fraction/exponent decodes to `JsonInt`, but it is still a valid `Float` field value (`{"ratio": 1}`), so widen it here.

#### `Json.array_of() -> Result(List(Json), DeserializeError)`

#### `Json.get_string(key: String) -> Option(String)`

`get` composed with each `as_*` — the common case of reading a typed field out of an object without spelling the two steps every time.

#### `Json.get_int(key: String) -> Option(Int)`

#### `Json.get_bool(key: String) -> Option(Bool)`

#### `Json.get_strings(key: String) -> List(String)`

The string array at `key` as a `List(String)`, dropping any non-string element; `[]` when the key is absent or not an array. Collapses the very common "decode an object's array-of-strings field" pattern into one call.

LENIENT BY DESIGN (RFC-0044): a missing key, a non-array value, and a non-string element all collapse to the same empty/filtered result — ergonomic for display, but it cannot tell "absent" from "malformed". At a trust boundary (package, registry, or security code) where a wrong shape must be an ERROR, use the strict path instead: `get` then `array_of` (`Result`), coercing each element with `as_string`.

#### `Json.strings() -> List(String)`

A JSON array value as a `List(String)`, dropping any non-string element. Lenient by design (RFC-0044), like `get_strings`; for a strict decode use `array_of` (`Result`) and coerce each element with `as_string`.

#### `Json.index_string(i: Int) -> Option(String)`

The string at index `i` of a JSON array, when it is a string.

#### `Json.get_in(segments: List(String)) -> Option(Json)`

Follow exact object-key segments. Unlike `get_path`, segment strings are not parsed, so dots and empty strings are literal key names. Any missing key (or a non-object along the way) yields `None`.

#### `Json.get_path(path: String) -> Option(Json)`

Follow a dotted path of object keys, e.g. `resp.get_path("user.name")`. This splits on every `.`, so use `get_in` when a key itself may contain a dot. Any missing key (or a non-object along the way) yields `None`.

### Trait implementations

#### `impl Show for DecodeError`

- `fn show(self) -> String`

#### `impl Show for DeserializeError`

- `fn show(self) -> String`

#### `impl Error for DecodeError`

#### `impl Error for DeserializeError`

#### `impl From(DecodeError) for String`

- `fn from(value: DecodeError) -> Self`

#### `impl From(DeserializeError) for String`

- `fn from(value: DeserializeError) -> Self`

#### `impl Reflect for Json`

A `Json` reflects to its own value rather than to the `Json` enum's shape, so `mirror_to_json(reflect(j))` returns `j`. This lets an already-built `Json` (a decoded record, a signed blob) sit inside a reflected value such as an anonymous struct, e.g. `json.stringify(.{record: decoded, ok: true})`.

- `fn reflect(self) -> Mirror`

#### `impl From(a) for Json where a: Reflect`

Convert any reflectable value to `Json`, which also gives `Json.from(x)` and `x.into()` through `std/convert`'s blanket `Into`. This is `from_value` exposed as `From`, so a handler can accept `x: Into(Json)` instead of a pre-encoded string.

- `fn from(value: a) -> Json`

## `jwt`

jwt — verify a compact JWS / JWT (the OIDC identity-token shape), in PURE witchy over `crypto` (RS256), `encoding` (base64url), and `json`. Verification is computation, so this module has no host capability of its own — fetching the signing keys (JWKS discovery, over HTTPS) is a separate, network-bearing concern.

A compact JWT is `header.payload.signature`, each base64url. The signature covers the ASCII bytes of `header.payload`; for RS256 it is verified against the issuer's RSA public key (DER PKCS#1, hex). On success the decoded payload `claims` are returned for the caller to inspect (`sub`, `iss`, provider-specific owner fields).

#### `type JwtError`

Matchable compact-JWT/OIDC verification failures. These distinguish malformed wire segments, malformed JSON/JWKS metadata, invalid signatures, and semantic claim rejection without forcing callers to parse display strings.

- `MalformedCompact`
- `NoHeaderSegment`
- `NoPayloadSegment`
- `HeaderBase64(String)`
- `HeaderJson(String)`
- `HeaderMissingAlg`
- `HeaderMissingKid`
- `UnsupportedAlg(String)`
- `SignatureSegmentBase64(String)`
- `SignatureInputMalformed(String)`
- `SignatureInvalid`
- `PayloadBase64(String)`
- `PayloadJson(String)`
- `MissingClaim(String)`
- `IntClaimExpected(String)`
- `StringClaimExpected(String)`
- `AudienceClaimExpected`
- `AudienceArrayStringExpected`
- `TokenExpired`
- `AudienceMismatch`
- `IssuerMismatch`
- `NotYetValid`
- `AuthorizedPartyMismatch`
- `FreshnessPolicyInvalid`
- `IssuedAtBeforeEpoch(Int)`
- `IssuedAtAfterExpiry(Int, Int)`
- `IssuedAtInFuture(Int, Int)`
- `TokenLifetimeTooLong(Int, Int)`
- `JwkModulusBase64(String)`
- `JwkExponentBase64(String)`
- `JwkModulusEmpty`
- `JwkExponentEmpty`
- `JwksMissingKeys`
- `JwksKeysNotArray`
- `JwksNoMatchingRsaKey(String)`

#### `fn jwt_error_message(e: JwtError) -> String`

Human-readable JWT/OIDC failure text for logs, CLI output, and HTTP responses.

#### `fn verify_rs256(token: String, rsa_pubkey_der_hex: String, audience: String, now: Int) -> Result(Json, JwtError)`

Verify an RS256 (RSASSA-PKCS1-v1_5 / SHA-256) compact JWT against a DER PKCS#1 RSA public key (hex), checking the signature, `exp > now` (unix seconds), and `aud == audience`. Returns the decoded claims, or a typed error.

#### `fn verify_oidc(token: String, rsa_pubkey_der_hex: String, issuer: String, audience: String, now: Int) -> Result(Json, JwtError)`

The generic OIDC relying-party check: verify the RS256 signature AND that the token was minted by the expected `issuer` for the expected `audience`, and is valid now (`exp`/`nbf`). Returns the identity claims — `sub` plus provider-specific fields like GitHub's `repository` or Google's `email` — for the caller to authorize. This is the base check once it holds the issuer's JWKS key (`rsa_key_from_jwk`). Login and trusted-publishing flows should use `verify_oidc_fresh`, which adds their explicit `iat` and maximum-lifetime policy. The issuer check is what binds a token to a TRUSTED provider: without it, anyone who can mint a JWT for the right audience would be admitted.

#### `fn verify_oidc_fresh(token: String, rsa_pubkey_der_hex: String, issuer: String, audience: String, now: Int, max_lifetime: Int, clock_skew: Int) -> Result(Json, JwtError)`

The stricter relying-party entrypoint for short-lived identity tokens. RFC 7519 defines `iat` as optional in generic JWTs; OIDC relying parties choose their own acceptable freshness range. This variant makes that application policy explicit: `iat` is required, `exp - iat` may not exceed `max_lifetime`, and an issuer clock may lead the relying-party clock by at most `clock_skew` seconds. Signature, issuer, audience, expiry, nbf, and azp checks are still those of `verify_oidc`.

`clock_skew` applies only to a future `iat`. Expiry and nbf remain strict, so this API never extends the token's signed validity interval.

#### `fn header(token: String) -> Result(Json, JwtError)`

The decoded JOSE header of a compact JWT (its first segment), or an error — so a verifier can read `alg`/`kid` to select the JWKS key before checking the signature.

#### `fn claims_unverified(token: String) -> Result(Json, JwtError)`

The payload claims of a compact JWT WITHOUT verifying its signature — for reading the routing fields (`iss`, and `kid` via `header`) needed to SELECT the verification key before `verify_oidc`. DANGER: never authorize on these claims; verify the signature first and read the claims `verify_oidc` returns.

#### `fn rsa_key_from_jwk(n: String, e: String) -> Result(String, JwtError)`

Build the DER PKCS#1 `RSAPublicKey` (as hex — the shape `verify_rs256` wants) from a JWK's base64url modulus `n` and exponent `e`, so an OIDC verifier can turn a JWKS entry (`{"kty":"RSA","n":…,"e":…}`) into a key. The result is the ASN.1 DER `SEQUENCE { INTEGER n, INTEGER e }`; an INTEGER gains a leading `00` when its top bit is set (DER integers are signed two's-complement, RSA values are unsigned magnitudes).

#### `fn require_kid(token: String) -> Result(String, JwtError)`

Require the `kid` (key id) from a compact JWT header, preserving malformed compact/header input and a missing `kid` as matchable `JwtError` cases.

#### `fn kid(token: String) -> Option(String)`

Optional convenience for callers that genuinely treat a malformed/missing key id as absence. Verification boundaries should use `require_kid`.

#### `fn rsa_key_for_kid(jwks: Json, key_id: String) -> Result(String, JwtError)`

Select the RSA public key for `kid` from a JWKS document (`{"keys":[{"kty":"RSA","kid": …,"n":…,"e":…}, …]}`) and return it as the DER PKCS#1 hex `verify_rs256`/`verify_oidc` want. This is how an OIDC verifier consumes a provider's published keys (Google, GitHub Actions): fetch the JWKS, read the token's `kid` (`jwt.kid`), then pick the key.

### Trait implementations

#### `impl Show for JwtError`

- `fn show(self) -> String`

#### `impl Error for JwtError`

#### `impl From(JwtError) for String`

- `fn from(value: JwtError) -> Self`

## `list`

The witchy standard list library. Every function here is pure: the module declares no capability parameters, so importing it grants no authority — it can only transform data. This is the capability model in miniature: a library you didn't hand a Console/Dir/Net to literally cannot reach them.

#### `fn length(xs: List(a)) -> Int`

The number of elements.

#### `fn at(xs: List(a), index: Int) -> a`

The element at `index` (0-based). Out of bounds is a runtime error on every backend.

#### `fn push(var xs: List(a), x: a)`

Append `x` in place.

#### `fn concat(xs: List(a), ys: List(a)) -> List(a)`

A new list that is `xs` followed by `ys`.

#### `fn set_at(var xs: List(a), index: Int, value: a)`

Store `value` at `index`. An out-of-range (or negative) index is a runtime error on both backends, symmetric with `list.at` and `xs[i]`.

#### `fn update_at(var xs: List(a), index: Int, f: fn(a) -> a)`

A copy of `xs` with the function `f` applied to the element at `index`. An out-of-range (or negative) index is a runtime error on both backends, exactly like `list.at` — a silently discarded update is a contract violation (RFC-0044 rule 3), so it aborts rather than leaving the list unchanged.

#### `fn pop(var xs: unique List(a)) -> Option(a)`

Remove and return the final element. A uniquely owned non-empty list moves the leaf out in O(1) without copying its spine; a shared root is copied so aliases retain their old contents in normal mode. In `mode opt`, the `unique` receiver rejects a shared or loaned call site with the ownership reason. An empty list returns None without copying.

#### `fn slice(xs: List(a), start: Int, end: Int) -> List(a)`

The elements in the half-open index range [start, end), clamped to bounds. `slice(xs, 1, 3)` of [a,b,c,d] is [b,c].

#### `fn range(n: Int) -> List(Int)`

[0, 1, ..., n-1].

#### `fn range_between(lo: Int, hi: Int) -> List(Int)`

The half-open span `lo..hi`: [lo, lo+1, ..., hi-1], empty when `lo >= hi`.

#### `fn range_step(start: Int, stop: Int, step: Int) -> List(Int)`

The span from `start` toward `stop` (exclusive) advancing by `step`. A positive `step` counts up while below `stop`, a negative `step` counts down while above `stop`, and a zero `step` yields [] rather than looping forever.

#### `fn repeat(x: a, n: Int) -> List(a)`

A list of `n` copies of `x` (empty when `n <= 0`).

#### `List.push(x: a)`

Append `x` in place.

#### `List.concat(ys: List(a)) -> List(a)`

A new list that is `xs` followed by `ys`.

#### `List.map(f: fn(a) -> b) -> List(b)`

Apply `f` to every element, collecting the results.

#### `List.filter(keep: fn(a) -> Bool) -> List(a)`

Keep only the elements for which `keep` returns true.

#### `List.reverse()`

Reverse in place.

#### `List.sort_by(less: fn(a, a) -> Bool)`

Sort using a caller-supplied "is-less-than" comparator — a stable merge sort (O(n log n)), so equal elements keep their original order. Generic over the element type.

#### `List.set_at(index: Int, value: a)`

Store `value` at `index`. An out-of-range (or negative) index is a runtime error on both backends, symmetric with `list.at` and `xs[i]`.

#### `List.update_at(index: Int, f: fn(a) -> a)`

A copy of `xs` with the function `f` applied to the element at `index`. An out-of-range (or negative) index is a runtime error on both backends, exactly like `list.at` — a silently discarded update is a contract violation (RFC-0044 rule 3), so it aborts rather than leaving the list unchanged.

#### `List.pop() -> Option(a)`

Remove and return the final element. A uniquely owned non-empty list moves the leaf out in O(1) without copying its spine; a shared root is copied so aliases retain their old contents in normal mode. In `mode opt`, the `unique` receiver rejects a shared or loaned call site with the ownership reason. An empty list returns None without copying.

#### `List.pop_front() -> Option(a)`

Remove and return the first element, or `None` for the empty list.

#### `List.swap(i: Int, j: Int)`

Exchange the elements at `i` and `j`. An out-of-range index is a runtime error on both backends, like `list.at`.

#### `List.length() -> Int`

The number of elements.

#### `List.is_empty() -> Bool`

Whether the list has no elements.

#### `List.at(index: Int) -> a`

The element at `index` (0-based). Out of bounds is a runtime error on every backend.

#### `List.slice(start: Int, end: Int) -> List(a)`

The elements in the half-open index range [start, end), clamped to bounds. `slice(xs, 1, 3)` of [a,b,c,d] is [b,c].

#### `List.get(index: Int) -> Option(a)`

The element at `index` as `Some`, or `None` when `index` is out of range — a total, bounds-checked alternative to the `at` builtin.

#### `List.head() -> Option(a)`

The first element as `Some`, or `None` for the empty list.

#### `List.last() -> Option(a)`

The last element as `Some`, or `None` for the empty list.

#### `List.head_or(default: a) -> a`

The first element, or `default` when the list is empty. (A total accessor in the style of `get_or`/`unwrap_or`, so it never indexes out of bounds.)

#### `List.last_or(default: a) -> a`

The last element, or `default` when the list is empty.

#### `List.find(pred: fn(a) -> Bool) -> Option(a)`

The first element satisfying `pred` as `Some`, or `None` if none do.

#### `List.find_map(f: fn(a) -> Option(b)) -> Option(b)`

The first non-`None` result of applying `f` across the list (search and transform in one pass), or `None` if every result is `None`.

#### `List.find_or(pred: fn(a) -> Bool, default: a) -> a`

The first element satisfying `pred`, or `default` if none do.

#### `List.any(pred: fn(a) -> Bool) -> Bool`

Whether at least one element satisfies `pred`.

#### `List.all(pred: fn(a) -> Bool) -> Bool`

Whether every element satisfies `pred` (true for the empty list).

#### `List.position(pred: fn(a) -> Bool) -> Option(Int)`

The index of the first element satisfying `pred` as `Some`, or `None` if none do — the by-predicate search (`index_of` is the by-value search). One name per axis, both `Option`, no sentinel (RFC-0044/0049).

#### `List.count_where(pred: fn(a) -> Bool) -> Int`

How many elements satisfy `pred`.

#### `List.fold(init: b, f: fn(b, a) -> b) -> b`

Reduce the list to a single value, left to right.

#### `List.reduce(f: fn(a, a) -> a) -> Option(a)`

Combine the elements left to right using the first as the seed, as `Some`; `None` for the empty list. (A `fold` that needs no initial value — handy for max/min/sum over a non-empty list with a plain binary op.)

#### `List.scan(init: b, f: fn(b, a) -> b) -> List(b)`

Like `fold`, but collect every intermediate accumulator left to right, starting from `init`: scan([1,2,3], 0, +) -> [0, 1, 3, 6].

#### `List.sum() -> Int`

The sum of a list of integers (0 for the empty list).

#### `List.sum_by(f: fn(a) -> Int) -> Int`

The sum of `f` applied to each element (0 for the empty list) — e.g. a total over a record field: `cart.sum_by(fn(it): it.price * it.qty)`.

#### `List.product() -> Int`

The product of a list of integers (1 for the empty list).

#### `List.flat_map(f: fn(a) -> List(b)) -> List(b)`

Map each element to a list, then concatenate the results.

#### `List.flatten() -> List(b)`

Concatenate a list of lists into one.

#### `List.transpose() -> List(List(b))`

Turn a list of rows into a list of columns. Rows are read only up to the length of the SHORTEST row, so a ragged tail is dropped and the result stays rectangular: `[[1, 2, 3], [4, 5, 6]].transpose()` is `[[1, 4], [2, 5], [3, 6]]`.

#### `List.take(n: Int) -> List(a)`

The first `n` elements (fewer if the list is shorter).

#### `List.drop(n: Int) -> List(a)`

All but the first `n` elements.

#### `List.take_while(pred: fn(a) -> Bool) -> List(a)`

The longest leading run of elements satisfying `pred`.

#### `List.drop_while(pred: fn(a) -> Bool) -> List(a)`

Drop the longest leading run satisfying `pred`, keeping the rest.

#### `List.split_at(n: Int) -> (List(a), List(a))`

Split the list at index `n` into `(first n, the rest)`. `n` is clamped, so `xs.split_at(0)` is `([], xs)` and an `n` past the end gives `(xs, [])`.

#### `List.tail() -> List(a)`

All elements after the first; the empty list maps to the empty list.

#### `List.drop_last() -> List(a)`

All elements except the last; the empty list maps to the empty list.

#### `List.chunks(n: Int) -> List(List(a))`

Split the list into consecutive sublists of length `n` (the final one may be shorter). `[1,2,3,4,5].chunks(2)` is `[[1,2],[3,4],[5]]`; there are no chunks of a non-positive length, so `n < 1` yields `[]` (like `windows`).

#### `List.windows(n: Int) -> List(List(a))`

All contiguous sublists of length `n` (a sliding window of step 1). Empty when `n < 1` or longer than the list. `[1,2,3,4].windows(2)` is `[[1,2],[2,3],[3,4]]`.

#### `List.zip(ys: List(b)) -> List((a, b))`

Pair up two lists element-wise, stopping at the shorter one.

#### `List.zip_with(ys: List(b), f: fn(a, b) -> c) -> List(c)`

Combine two lists element-wise with `f`, stopping at the shorter one.

#### `List.unzip() -> (List(x), List(y))`

Split a list of pairs into a pair of lists — the inverse of `zip`.

#### `List.enumerate() -> List((Int, a))`

Pair each element with its index: `[a, b]` -> `[(0, a), (1, b)]`.

#### `List.intersperse(sep: a) -> List(a)`

Insert `sep` between adjacent elements: [a, b, c] -> [a, sep, b, sep, c].

#### `List.partition(pred: fn(a) -> Bool) -> (List(a), List(a))`

Split the list into (matching, non-matching) by `pred`, each preserving the original order. A single pass — the dual of running `filter` twice.

#### `List.max_by(less: fn(a, a) -> Bool) -> Option(a)`

The maximum element under a caller-supplied "is-less-than" comparator, as `Some` (the first of equal maxima), or `None` for the empty list. Generic, so it works for any type — e.g. max by a record field.

#### `List.min_by(less: fn(a, a) -> Bool) -> Option(a)`

The minimum element under `less`, as `Some`; `None` for the empty list.

#### `List.contains(target: a) -> Bool`

Whether `target` appears in the list, by the element type's `Eq` impl. The `where a: Eq` bound monomorphizes the equality per element type, so the comparison is content-correct on both backends — including user record element types, which the compiled backend cannot compare through an unbounded generic `==` (RFC-0046).

#### `List.index_of(target: a) -> Option(Int)`

The index of the first element equal to `target` as `Some`, or `None` if absent (RFC-0044 rule 1: absence is `Option`, never a -1 sentinel). The `where a: Eq` bound makes the equality content-correct on both backends.

#### `List.remove(target: a) -> Bool`

Remove the first occurrence of `target`, reporting whether one was removed; the list is unchanged when absent. To remove every occurrence, use `xs.filter(fn(y): y != target)`.

#### `List.count(target: a) -> Int`

The number of elements equal to `target`, by the element type's `Eq` impl. This is the counted companion to `contains`, so equality dispatch stays in the list module alongside membership, indexing, and de-duplication.

#### `List.unique() -> List(a)`

The list with duplicates removed, keeping the first occurrence of each element (by the element type's `Eq`), in original order.

#### `List.sort()`

Sort any list whose elements are `Ord` ascending — a stable merge sort (O(n log n)) that dispatches through the element type's total order, so `xs.sort()` works for `Int`, `String`, `Duration`, or your own derived-`Ord` records, content-correct on both backends (RFC-0046). A merely-partial type like `Float` (not `Ord`) is rejected at the bound; sort those with `sort_by`.

#### `List.min() -> Option(a)`

The smallest element as `Some`, or `None` for the empty list. Generic over any `Ord` element type (like `sort`), dispatching through the total order — so `xs.min()` works for `Int`, `String`, `Duration`, or a derived-`Ord` record. For a merely-partial type or a custom criterion, use `min_by`.

#### `List.max() -> Option(a)`

The largest element as `Some`, or `None` for the empty list. Generic over any `Ord` element type — see `min`.

#### `List.join(sep: String) -> String`

Concatenate the strings, inserting `sep` between adjacent elements: `["a", "b", "c"].join("-")` is `"a-b-c"`, and `[].join(sep)` is `""`.

## `math`

The witchy standard math library: small integer helpers, pure and capability-free. (Comparison can't be generic without type classes, so these are Int-specific.)

#### `fn to_float(n: Int) -> Float`

Int -> Float, exactly (within f64 precision).

#### `fn to_int(x: Float) -> Int`

Float -> Int, truncating toward zero. NaN is a runtime error; out-of-range finite values and infinities saturate.

#### `fn sqrt(x: Float) -> Float`

The square root.

#### `fn min(a: Int, b: Int) -> Int`

#### `fn max(a: Int, b: Int) -> Int`

#### `fn abs(n: Int) -> Int`

The absolute value of `n`. `Int.MIN` (`-9223372036854775808`) is the one input with no positive `Int` counterpart, so negating it would wrap back to itself (a negative result that contradicts `abs`). That single unrepresentable case is a contract violation (RFC-0044 rule 3): abort rather than return a negative value.

#### `fn sign(n: Int) -> Int`

-1, 0, or 1 depending on the sign of `n`.

#### `fn clamp(x: Int, lo: Int, hi: Int) -> Int`

Constrain `x` to the inclusive range [lo, hi]. `lo` must not exceed `hi` (RFC-0044 rule 3): inverted bounds describe an empty range, so they fail loudly instead of silently returning `lo`.

#### `fn pow(base: Int, exp: Int) -> Int`

`base` raised to a non-negative `exp` (`pow(base, 0)` is 1). A negative `exp` has no integer answer, so it is a contract violation (RFC-0044 rule 3): abort naming the bad argument rather than silently returning 1.

#### `fn ceil_div(a: Int, b: Int) -> Int`

Ceiling division: the smallest integer >= a / b (e.g. items split into pages of `b`). The divisor must be positive (RFC-0044 rule 3): a non-positive `b` fails loudly rather than trapping raw or computing off-contract values. `ceil_div(7, 3)` is 3, `ceil_div(6, 3)` is 2.

#### `fn round_div(a: Int, b: Int) -> Int`

Division rounded to the nearest integer (ties away from zero). The divisor must be positive (RFC-0044 rule 3; a non-positive `b` fails loudly): `round_div(7, 2)` is 4, `round_div(5, 3)` is 2, `round_div(-7, 2)` is -4.

#### `fn gcd(a: Int, b: Int) -> Int`

Greatest common divisor (Euclid's algorithm).

#### `fn lcm(a: Int, b: Int) -> Int`

Least common multiple (0 if either argument is 0). Divides before multiplying to keep the intermediate value small.

#### `fn is_even(n: Int) -> Bool`

Whether `n` is even.

#### `fn is_odd(n: Int) -> Bool`

Whether `n` is odd.

#### `fn factorial(n: Int) -> Int`

`n!` — the product 1*2*...*n (1 for n in {0, 1}). Watch the 64-bit range: factorial is exact through 20!; 21! overflows and wraps. `n < 0` has no factorial, so it is a contract violation (RFC-0044 rule 3): abort naming the bad argument rather than silently returning 1.

#### `fn is_prime(n: Int) -> Bool`

Whether `n` is prime (trial division up to math.sqrt(n); n < 2 is not prime).

#### `fn isqrt(n: Int) -> Int`

Integer square root: the largest `r` with `r*r <= n` (`isqrt(0)` is 0). A negative `n` has no real square root, so it is a contract violation (RFC-0044 rule 3): abort naming the bad argument rather than silently returning 0. Uses `mid <= n / mid` instead of `mid * mid <= n` so it never overflows.

#### `fn is_perfect_square(n: Int) -> Bool`

Whether `n` is a perfect square (0, 1, 4, 9, ...). Negative `n` is never one.

#### `fn to_base(n: Int, base: Int) -> String`

Render `n` in `base` (2..16) with lowercase digits; a negative `n` gets a leading "-". A base outside 2..16 has no digit alphabet, so it fails loudly naming the bad argument (RFC-0044 rule 3) rather than returning "".

#### `fn to_hex(n: Int) -> String`

`n` in hexadecimal (e.g. 255 -> "ff").

#### `fn to_binary(n: Int) -> String`

`n` in binary (e.g. 5 -> "101").

#### `fn float_min(a: Float, b: Float) -> Float`

#### `fn float_max(a: Float, b: Float) -> Float`

#### `fn float_abs(x: Float) -> Float`

#### `fn float_clamp(x: Float, lo: Float, hi: Float) -> Float`

#### `fn format_float(x: Float, decimals: Int) -> String`

Format `x` with `decimals` digits after the decimal point, rounded half-up: format_float(3.14159, 2) = "3.14", format_float(-0.5, 1) = "-0.5", format_float(2.0, 0) = "2". Built from float arithmetic, so unlike the `to_string` builtin it works on the compiled WASM backend too (which has no float formatting). Best for a fixed number of places; very large magnitudes lose precision to the Float itself. `decimals` is capped at 18 — the most places the `Int` scale `pow(10, decimals)` can hold without overflowing.

## `meta`

Compile-time type introspection — the `typeInfo` half of witchy's comptime reflection (Zig's `@typeInfo`). A `comptime:` block can read the structure of every type in its module as ordinary data and generate code from it (e.g. a `to_json` specialized to a record's fields), with zero runtime cost. The compiler injects the type list as the `module_types` value; these are the shapes it hands you.

This is COMPILE-TIME structure (field names + declared type expressions), distinct from `std/reflect`'s runtime `Mirror` (a value's structure at runtime).

#### `sealed type ItemSyntax`

RFC-0080's typed boundary for whole generated items. `quote item:` and literal whole-item `meta.item("...")` values retain compiler-owned item AST. For an item quote with holes, the compiler substitutes typed expression, type, and pattern nodes into that AST. Dynamic `item(source)` input is parsed once at this boundary and then travels as a compiler-owned module fragment, including any imports required by the generated declaration.

- `ItemSyntax(String)`

#### `sealed type ModuleSyntax`

An opaque aggregate of generated items. Keeping the aggregate typed lets generators build and transform a whole generated module without flattening its items back into source text.

- `ModuleSyntax(List(ItemSyntax))`

#### `sealed type Span`

Source spans are compiler-owned metadata. The constructor is sealed so user code cannot forge locations; expansion records definition, invocation, and hole-ancestry spans for diagnostics and generated-symbol navigation.

- `Span(String, Int, Int)`

#### `sealed type TypeSyntax`

Hole-free `quote type:` values retain a compiler-owned type AST. The internal constructor carries its opaque identity plus canonical source so existing builders can compose it through the compatibility path.

- `TypeSyntax(String)`
- `CompilerTypeSyntax(String, String)`

#### `sealed type ExprSyntax`

Hole-free `quote expr:` and literal `meta.expr_raw("...")` values retain a compiler-owned expression AST. The second constructor is compiler-internal; structural call, field, and match builders retain it, while remaining compatibility builders may still project canonical source.

- `ExprSyntax(String)`
- `CompilerExprSyntax(String, String)`

#### `sealed type PatternSyntax`

Hole-free `quote pattern:` values retain compiler-owned pattern AST. Existing builders project canonical source when composing compatibility values.

- `PatternSyntax(String)`
- `CompilerPatternSyntax(String, String)`

#### `sealed type SyntaxHole`

- `ExprHole(ExprSyntax)`
- `TypeHole(TypeSyntax)`
- `PatternHole(PatternSyntax)`

#### `sealed type StmtSyntax`

Hole-free `quote stmt:` values retain compiler-owned statement AST.

- `StmtSyntax(String)`
- `CompilerStmtSyntax(String, String)`

#### `sealed type BlockSyntax`

Hole-free `quote block:` values retain compiler-owned block AST.

- `BlockSyntax(String)`
- `CompilerBlockSyntax(String, String)`

#### `sealed type MatchArmSyntax`

- `MatchArmSyntax(String)`
- `CompilerMatchArmSyntax(String, String)`

#### `sealed type ParamSyntax`

- `ParamSyntax(String)`
- `CompilerParamSyntax(String, String)`

#### `sealed type Ident`

- `Ident(String)`
- `CallSiteIdent(String)`

#### `type TypeExpr`

A declared type expression, exposed as data so generators do not have to parse source-looking type strings.

- `TNamed(String, List(TypeExpr))`
- `TTuple(List(TypeExpr))`
- `TFn(List(TypeExpr), TypeExpr, List(String))`
- `TQualified(String, TypeExpr)`
- `TBorrowed(TypeExpr, String)`

#### `type TypeKind`

The declaration shape. `TypeUninhabited` is intentionally not called "unit": a fieldless declaration has no constructor and therefore no values.

- `TypeRecord`
- `TypeSum`
- `TypeUninhabited`

#### `type FieldInfo`

One field of a record: its name and declared type.

- `FieldInfo { name: String, type_expr: TypeExpr }`

#### `type VariantInfo`

One constructor of a sum type: its name and positional payload types.

- `VariantInfo { name: String, field_type_exprs: List(TypeExpr) }`

#### `type TypeInfo`

A type's structure. `fields` is populated for records and `variants` for sums; both are empty for an uninhabited fieldless declaration.

- `TypeInfo { name: String, kind: TypeKind, params: List(String), fields: List(FieldInfo), variants: List(VariantInfo) }`

#### `fn item(source: String) -> ItemSyntax`

Parse one complete dynamic declaration (and its imports) into compiler-owned syntax. Literal whole-item arguments take the parser's equivalent zero-runtime path; dynamic input is rejected here unless it is exactly one declaration.

#### `fn item_join_syntax(parts: List(String), holes: List(SyntaxHole)) -> ItemSyntax`

Join parser-checked item quote fragments with typed syntax holes.

#### `fn ident(name: String) -> Ident`

A validated Witchy identifier. This rejects keywords, `_`, non-ASCII source spelling, and compiler-reserved `__` names before generated source is parsed.

#### `fn call_site(name: String) -> Ident`

An explicit invocation-site reference. The syntax constructor that consumes this Ident determines whether it denotes a value/function, type, or constructor; the compiler retains that category and origin without rendering a forgeable source marker.

#### `fn fresh(hint: String) -> Ident`

A deterministic compiler-owned binding name. The returned identifier cannot collide with source names because its spelling lives in the reserved `__` namespace; repeated calls, blocks, and tagged-literal invocations are distinct.

#### `fn is_identifier(name: String) -> Bool`

#### `fn type_named(name: Ident, args: List(TypeSyntax)) -> TypeSyntax`

Source-backed type syntax. Prefer this to assembling type names at each generator call site; it is still a migration helper, not full hygiene.

#### `fn type_qualified(module: Ident, name: Ident, args: List(TypeSyntax)) -> TypeSyntax`

#### `fn type_tuple(types: List(TypeSyntax)) -> TypeSyntax`

A tuple type such as `(Int, String)`.

#### `fn type_fn(params: List(TypeSyntax), ret: TypeSyntax) -> TypeSyntax`

A function type such as `fn(Int) -> String`.

#### `fn type_fn_with_conventions(params: List(TypeSyntax), conventions: List(String), ret: TypeSyntax) -> TypeSyntax`

#### `fn type_frozen(ty: TypeSyntax) -> TypeSyntax`

A deeply immutable type qualifier.

#### `fn type_unique(ty: TypeSyntax) -> TypeSyntax`

A unique-reference type qualifier.

#### `fn type_local_unique(ty: TypeSyntax) -> TypeSyntax`

A local unique-reference type qualifier.

#### `fn type_capability(name: Ident, rights: List(TypeSyntax)) -> TypeSyntax`

A capability type with rights, such as `Dir[Read]`.

#### `fn type_expr(ty: TypeExpr) -> TypeSyntax`

#### `fn type_join(parts: List(String), holes: List(TypeSyntax)) -> TypeSyntax`

Join parser-checked type quote fragments with typed type holes.

#### `fn expr_name(name: Ident) -> ExprSyntax`

Expression syntax builders. Ordinary names retain the compatibility payload; call-site names become compiler-owned nodes so their origin cannot be forged or lost before linking.

#### `fn expr_call(callee: ExprSyntax, args: List(ExprSyntax)) -> ExprSyntax`

#### `fn expr_field(base: ExprSyntax, field: Ident) -> ExprSyntax`

#### `fn expr_int(n: Int) -> ExprSyntax`

#### `fn expr_bool(b: Bool) -> ExprSyntax`

#### `fn expr_match(scrutinee: ExprSyntax, arms: List(MatchArmSyntax)) -> ExprSyntax`

#### `fn expr_raw(source: String) -> ExprSyntax`

#### `fn expr_join_syntax(parts: List(String), holes: List(SyntaxHole)) -> ExprSyntax`

Join parser-checked expression fragments with mixed typed syntax holes.

#### `fn pattern_var(name: Ident) -> PatternSyntax`

Source-backed pattern syntax. Patterns share validated identifiers with expressions, so generated matches cannot accidentally mint a reserved binding.

#### `fn pattern_wildcard() -> PatternSyntax`

#### `fn pattern_int(n: Int) -> PatternSyntax`

#### `fn pattern_bool(b: Bool) -> PatternSyntax`

#### `fn pattern_str(value: String) -> PatternSyntax`

A string literal pattern.

#### `fn pattern_duration_ms(ms: Int) -> PatternSyntax`

A duration literal pattern from its whole-millisecond value.

#### `fn pattern_range(lo: Int, hi: Int, inclusive: Bool) -> PatternSyntax`

An integer range pattern.

#### `fn pattern_ctor(name: Ident, args: List(PatternSyntax)) -> PatternSyntax`

#### `fn pattern_qualified_ctor(module: Ident, name: Ident, args: List(PatternSyntax)) -> PatternSyntax`

A module-qualified constructor pattern such as `iter.Item(x)`.

#### `fn pattern_anon_ctor(tag: Ident, args: List(PatternSyntax)) -> PatternSyntax`

#### `fn pattern_tuple(patterns: List(PatternSyntax)) -> PatternSyntax`

A tuple pattern.

#### `fn pattern_list(patterns: List(PatternSyntax)) -> PatternSyntax`

An exact-length list pattern.

#### `fn pattern_list_rest(patterns: List(PatternSyntax), rest: Option(Ident)) -> PatternSyntax`

A list pattern with `..` or `..rest`.

#### `fn pattern_or(patterns: List(PatternSyntax)) -> PatternSyntax`

An or-pattern. Every alternative must bind the same names when type-checked.

#### `fn pattern_join(parts: List(String), holes: List(PatternSyntax)) -> PatternSyntax`

Join parser-checked pattern quote fragments with typed pattern holes.

#### `fn expr_hole(expr: ExprSyntax) -> SyntaxHole`

#### `fn type_hole(ty: TypeSyntax) -> SyntaxHole`

#### `fn pattern_hole(pattern: PatternSyntax) -> SyntaxHole`

#### `fn match_arm(pattern: PatternSyntax, body: ExprSyntax) -> MatchArmSyntax`

#### `fn stmt_let(mutable: Bool, name: Ident, ty: Option(TypeSyntax), value: ExprSyntax) -> StmtSyntax`

Source-backed statement and block syntax. These are still text at the compiler boundary, but they make block-shaped generators compose through typed, validated pieces instead of one large string template.

#### `fn stmt_expr(expr: ExprSyntax) -> StmtSyntax`

#### `fn stmt_return(value: ExprSyntax) -> StmtSyntax`

#### `fn stmt_return_none() -> StmtSyntax`

#### `fn stmt_raw(source: String) -> StmtSyntax`

A source-backed statement wrapper for parser-checked `quote stmt:`.

#### `fn stmt_join_syntax(parts: List(String), holes: List(SyntaxHole)) -> StmtSyntax`

Join parser-checked statement quote fragments with typed syntax holes.

#### `fn block(stmts: List(StmtSyntax), tail: Option(ExprSyntax)) -> BlockSyntax`

#### `fn block_raw(source: String) -> BlockSyntax`

A source-backed block wrapper for parser-checked `quote block:`.

#### `fn block_join_syntax(parts: List(String), holes: List(SyntaxHole)) -> BlockSyntax`

Join parser-checked block quote fragments with typed syntax holes.

#### `fn param(name: Ident, ty: TypeSyntax) -> ParamSyntax`

A function parameter and function item constructors. The single-expression form now delegates to the block form so item generation has one body shape.

#### `fn function(public: Bool, name: Ident, params: List(ParamSyntax), ret: Option(TypeSyntax), body: ExprSyntax) -> ItemSyntax`

#### `fn function_block(public: Bool, name: Ident, params: List(ParamSyntax), ret: Option(TypeSyntax), body: BlockSyntax) -> ItemSyntax`

#### `fn impl_block(trait_ty: TypeSyntax, target_ty: TypeSyntax, items: List(ItemSyntax)) -> ItemSyntax`

Build a trait implementation directly from typed trait/target heads and compiler-owned function items. No impl source is rendered or reparsed.

#### `fn module(items: List(ItemSyntax)) -> ModuleSyntax`

#### `fn module_items(module_syntax: ModuleSyntax) -> List(ItemSyntax)`

#### `fn type_source(ty: TypeExpr) -> String`

Render a structured type back to source text only when generated source needs to name the type. Semantic branching should stay on `TypeExpr`, not this string.

#### `fn derive_show(t: TypeInfo) -> ItemSyntax`

`derive(Show)` -> constructor-shaped rendering with each field/payload rendered through its own `Show` impl. Primitive fields still match structural interpolation bytes, while fields with custom `Show` keep their public display form. Generic parameters carry a `: Show` bound, so the same code path compiles coherently on both backends.

#### `fn derive_eq(t: TypeInfo) -> ItemSyntax`

`derive(Eq)` → the total-equality marker. Refines `PartialEq` (derive both).

#### `fn derive_partial_eq(t: TypeInfo) -> ItemSyntax`

`derive(PartialEq)` → field-wise structural equality. The operators dispatch per field, so it is content-correct on both backends: a record compares each field with `==`; a sum type matches the variant and compares payloads.

#### `fn derive_reflect(t: TypeInfo) -> ItemSyntax`

`derive(Reflect)` → an `impl Reflect for T` building the value's `Mirror`: a record to `MRecord("T", [(field, reflect field)…])`, a sum type to a `match` over variants to `MVariant`. (The module must `import reflect`; caller checks.)

#### `fn derive_ord(t: TypeInfo) -> ItemSyntax`

`derive(Ord)` → lexicographic `compare` returning `Ordering` (records only; the caller validates the shape). Requires the `PartialEq`/`Eq`/`PartialOrd` impls too.

#### `fn derive_partial_ord(t: TypeInfo) -> ItemSyntax`

`derive(PartialOrd)` → lexicographic `partial_compare` (records only).

#### `fn derive_deserialize(t: TypeInfo) -> ItemSyntax`

`derive(Deserialize)` generates `from_json` for a record (the caller validates the shape). It decodes and coerces each field, returning on the first error. There is no matching `Serialize` derive, because reflection (`json.from_value`, `stringify`, `Into(Json)`) already encodes any value, so only this reconstruction is per-type. The generated code uses only json/result/list/option.

## `oauth`

oauth — the OAuth 2.0 Authorization Code flow (RFC 6749 §4.1), the basis of "Log in with GitHub / Google". Pure witchy over `std/http` (HTTPS) + `url`. A relying party:   1. redirects the user to `authorize_url(...)`;   2. receives a `code` (and the `state` it sent) at its registered callback;   3. exchanges the code for an access token with `exchange_code(...)`. Identity is then read from a provider endpoint (GitHub `/user`) or, for OIDC, the `id_token` (verify with `std/jwt`). `state` is an opaque anti-CSRF token the caller signs before the redirect and re-checks on the callback — bind it to the session.

#### `type OAuthError`

Matchable OAuth/OIDC client failures. Transport, provider HTTP status, JSON response shape, and missing token fields stay distinct until the application boundary renders them.

- `TokenEndpointNotHttps(String)`
- `BearerEndpointNotHttps(String)`
- `TokenEndpointUnreachable(http.HttpError)`
- `BearerRequestFailed(http.HttpError)`
- `TokenEndpointRejected(Int, String)`
- `BearerEndpointRejected(Int)`
- `TokenResponseJson(json.DecodeError)`
- `BearerResponseJson(json.DecodeError)`
- `ProviderError(String)`
- `MissingTokenField(String)`

#### `fn oauth_error_message(e: OAuthError) -> String`

#### `fn authorize_url(authorize_endpoint: String, client_id: String, redirect_uri: String, scope: String, state: String) -> String`

The provider authorization-endpoint URL to redirect the user to. After the user approves, the provider redirects to `redirect_uri?code=...&state=...`. `scope` is the provider's space-separated permission list (e.g. GitHub `read:user`, OIDC `openid email`). An endpoint that already carries fixed parameters (a tenant, `prompt`, ...) is extended with `&`, never a second `?`.

#### `fn exchange_code(fetch: Fetch, token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(String, OAuthError)`

Exchange an authorization `code` for an access token at the provider's token endpoint — an HTTPS POST with a form-encoded body and `Accept: application/json`. Returns the `access_token`, or a reason. Needs a `Net` that reaches the token host over TLS; the `client_secret` should come from a `Secret`, never a literal.

#### `fn exchange_code_string(fetch: Fetch, token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(String, String)`

#### `fn exchange_code_id_token(fetch: Fetch, token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(String, OAuthError)`

Like `exchange_code`, but returns the OIDC `id_token` (a JWT carrying the user's identity) instead of the access token — for "Log in with Google" and other OIDC providers. Verify the returned token with `std/jwt` (`kid` → `rsa_key_for_kid` over the provider's JWKS → `verify_oidc_fresh` with that provider's lifetime policy).

#### `fn exchange_code_id_token_string(fetch: Fetch, token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(String, String)`

#### `fn token_response(fetch: Fetch, token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(Json, OAuthError)`

The raw token-endpoint response as JSON (the HTTPS POST exchanging the code) — the level beneath `exchange_code`; read `access_token` / `id_token` / `refresh_token` from it.

#### `fn token_response_string(fetch: Fetch, token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(Json, String)`

#### `fn bearer_get_json(fetch: Fetch, url: String, token: String) -> Result(Json, OAuthError)`

GET `url` with a `Bearer` access token and parse the JSON body — the "read the signed-in user" step after `exchange_code` (GitHub `/user`, an OIDC userinfo endpoint). Sends a `User-Agent` (GitHub rejects requests without one). Returns the parsed JSON, or a reason. The caller reads identity fields it trusts (`login`, `id`, `email`).

#### `fn bearer_get_json_string(fetch: Fetch, url: String, token: String) -> Result(Json, String)`

### Trait implementations

#### `impl Show for OAuthError`

- `fn show(self) -> String`

#### `impl Error for OAuthError`

#### `impl From(OAuthError) for String`

- `fn from(value: OAuthError) -> Self`

## `option`

The witchy standard `Option` type and helpers. `Option`, `Some`, and `None` are prelude names and never need an import; `import option` brings in the qualified helper forms such as `option.map`. Pure and capability-free.

#### `type Option`

- `Some(a)`
- `None`

#### `fn ok_or(o: Option(a), err: e) -> Result(a, e)`

Turn an Option into a Result: `Some(v)` becomes `Ok(v)`, `None` becomes `Err(err)` — the inverse of `result.ok`.

#### `fn all(xs: List(Option(a))) -> Option(List(a))`

Collect a list of Options into an Option of the list: `Some` of every value in order, or `None` if any element is `None`.

#### `Option.is_some() -> Bool`

True if the option holds a value.

#### `Option.is_none() -> Bool`

True if the option holds no value.

#### `Option.unwrap_or(default: a) -> a`

The Some value, or `default` if it's None.

#### `Option.unwrap_or_else(f: fn() -> a) -> a`

The Some value, or the result of calling `f` (a lazily-computed default).

#### `Option.map(f: fn(a) -> b) -> Option(b)`

Transform the Some value, leaving None untouched.

#### `Option.map_or(default: b, f: fn(a) -> b) -> b`

Apply `f` to the Some value, or return `default` for None — `map` then `unwrap_or` in one step.

#### `Option.and_then(f: fn(a) -> Option(b)) -> Option(b)`

Chain a fallible step: apply `f` (which itself yields an Option) to the Some value, or short-circuit on None.

#### `Option.filter(keep: fn(a) -> Bool) -> Option(a)`

Keep the Some value only if it satisfies `keep`; otherwise None.

#### `Option.or(alt: Option(a)) -> Option(a)`

The option if it is Some, otherwise the `alt` option.

#### `Option.or_else(f: fn() -> Option(a)) -> Option(a)`

The option if it is Some, otherwise the option produced by `f` (lazy).

#### `Option.ok_or(err: e) -> Result(a, e)`

Turn an Option into a Result: `Some(v)` becomes `Ok(v)`, `None` becomes `Err(err)` — the inverse of `result.ok`.

#### `Option.ok_or_else(f: fn() -> e) -> Result(a, e)`

Like `ok_or`, but the error for `None` is produced lazily by `f`.

#### `Option.zip(other: Option(b)) -> Option((a, b))`

Combine two options into an option of a pair: `Some((x, y))` only when both are `Some`, otherwise `None`.

#### `Option.flatten() -> Option(a)`

Collapse one layer of nesting: `Some(Some(v))` becomes `Some(v)`, and both `Some(None)` and `None` become `None`.

## `path`

path — pure manipulation of '/'-separated path strings.

This is string surgery only — it never touches the filesystem (that is the `Dir` capability, wrapped by `std/fs`). Splitting, joining, the base/dir/ext components, and a `normalize` that collapses `.` and `..`.

#### `fn is_abs(p: String) -> Bool`

Whether `p` is absolute (rooted at `/`).

#### `fn join(a: String, b: String) -> String`

Join two path pieces with a single `/`. An empty piece is ignored; an absolute `b` replaces `a`.

#### `fn base(p: String) -> String`

The final component: "a/b/c.txt" -> "c.txt", "a/b/" -> "b", "/" -> "/". The empty relative path has the conventional lexical component "."; it is never confused with the absolute root.

#### `fn dir(p: String) -> Option(String)`

The parent before the final component as `Some`: "a/b/c" -> Some("a/b"), "/x" -> Some("/"). A single-component path and the root have no parent, so "c" and "/" are `None` (RFC-0044 rule 1: absence is `Option`, never an "" sentinel). In particular, repeatedly taking `dir` cannot loop at root.

#### `fn ext(p: String) -> Option(String)`

The extension after the final `.` in the base name, WITHOUT the leading dot, as `Some` ("a/b.tar.gz" -> Some("gz")) — matching Rust's `Path::extension` both in dropping the dot AND in the `Option` shape (RFC-0044 rule 1). A base with no extension, a dotfile base (".bashrc"), or the parent marker ("..") is `None`. A trailing dot on an ordinary filename is an empty extension.

#### `fn stem(p: String) -> String`

The base name without its extension (".bashrc" -> ".bashrc"; "a.b.c" -> "a.b").

#### `fn normalize(p: String) -> String`

Collapse `.` and `..` segments and redundant slashes. A relative path that backs out past its start keeps the leading `..`s; an absolute one cannot escape its root. An empty result is "." (relative) or "/" (absolute).

## `policy`

policy — typed capability refinement policies (RFC-0011, RFC-0057). A policy is a pure value built by a type-associated constructor on the capability it belongs to, then handed to that capability's refinement verb: `net.only(policy)` narrows a `Net`, `net.deny(policy)` subtracts it, `dir.only(policy)` confines a `Dir`. The constructors live under the capability's OWN type — `Net.tcp(…)`, `Dir.ext(…)` — so a reader finds a capability's whole refinement vocabulary (verbs and policy values) in one place, with no shared grab-bag reaching across capabilities:

    let db  = net.only(Net.tcp("10.0.0.5", 6379))     // one plaintext host     let lan = net.deny(Net.cidr_any("10.0.0.0/8"))    // hold everything EXCEPT this block     let log = dir.only(Dir.ext(".log"))               // only `.log` files

These are pure value builders (empty capability footprint). A `NetPolicy` wraps the same `host:port` allowlist pattern the host enforces (RFC-0003); the `tls:` HTTPS scheme is a connect-time choice on the address, not a property of the policy (RFC-0009). The module is preluded, so `Net.tcp(…)` / `Dir.ext(…)` resolve without an import.

#### `sealed type NetPolicy`

A sealed `Net` address policy. Only the checked `Net.*` builders below can mint one; the newline-delimited host grammar is not a user-facing string escape hatch.

- `NetPolicy { pattern: String }`

#### `sealed type DirPolicy`

A sealed `Dir` ENTRY policy (RFC-0011): which entries the Dir may read/write/open. Only `Dir.ext`/`files`/`dirs` can mint one, so malformed raw grammar cannot turn a requested refinement into a silent no-op. Refinement only ever shrinks the set.

- `DirPolicy { pattern: String }`

#### `Net.tcp(host: String, port: Int) -> NetPolicy`

#### `Net.any_port(host: String) -> NetPolicy`

#### `Net.cidr(block: String, port: Int) -> NetPolicy`

#### `Net.cidr_any(block: String) -> NetPolicy`

#### `Net.union(a: NetPolicy, b: NetPolicy) -> NetPolicy`

#### `Net.private() -> NetPolicy`

#### `Dir.ext(suffix: String) -> DirPolicy`

#### `Dir.files() -> DirPolicy`

#### `Dir.dirs() -> DirPolicy`

## `prng`

A small deterministic PRNG: the Park-Miller "minimal standard" LCG, `state' = state * 16807 mod (2^31 - 1)`. The intermediate fits in i64 (no overflow), so it is content-correct on both backends. State is threaded explicitly — the same seed always replays the same sequence, which is what you want for tests, sampling, and games. NOT for cryptography. Pure and capability-free.

#### `sealed type Rng`

- `Rng(Int)`

#### `fn seed(s: Int) -> Rng`

A generator from seed `s` (any Int is mapped into the valid range 1..modulus).

#### `fn choice(xs: List(a), var r: Rng) -> Option(a)`

A pseudo-randomly chosen element of `xs` (`None` if empty). The index comes from `next_below`, whose `% len` reducer carries a negligible modulo bias for ordinary list lengths — plenty for tests, sampling, and games, but not a strict uniform distribution (see `next_below`). Module-level with the list FIRST for historical callers: the method form is `r.choice(xs)`, and deleting this function would flip the qualified call's argument order to the alias's `prng.choice(r, xs)`, silently breaking existing `prng.choice(xs, r)` call sites.

#### `Rng.next() -> Int`

Advance the generator and return a pseudo-random Int in [1, 2^31-1). The incoming state is normalized first, so a hand-built `Rng(0)` / `Rng(-5)` (which bypasses `seed`) still yields an in-range draw instead of sticking at 0 or going negative.

#### `Rng.next_below(bound: Int) -> Int`

A pseudo-random Int in [0, bound). `bound` must be positive (RFC-0044 rule 3): a non-positive bound has no valid range, so it fails loudly naming the bad argument rather than dividing by zero. It must also be below the generator range `2^31-1` (`next`'s cardinality): the reducer is `n % bound`, so a `bound` at or above that range cannot produce every value in `[0, bound)` and would silently under-cover it — that impossible case fails loudly too. For a `bound` well below the range the modulo bias is negligible; for a strictly uniform bounded draw, use the `Rand` capability's byte-oriented helpers.

#### `Rng.next_bool() -> Bool`

A pseudo-random Bool (true ~half the time).

#### `Rng.choice(xs: List(a)) -> Option(a)`

A pseudo-randomly chosen element of `xs` (`None` if empty) — see the module-level `choice`.

## `rand`

The witchy randomness library. Every draw comes from the `Rand` capability's source: the OS CSPRNG host-side (`getrandom` on the compiled backend), or — when `WITCHY_RAND_SEED` is set — a shared deterministic sequence both backends agree on, so randomness-using programs stay parity-stable and reproducible for tests.

#### `fn u64(rand: Rand) -> Int`

A fresh 64-bit draw spanning the full `Int` range (it may be negative). The primitive both other helpers build on.

#### `fn below(rand: Rand, n: Int) -> Int`

A non-negative integer in `[0, n)`. `n` must be positive (RFC-0044 rule 3): `[0, 0)` is an impossible range, so a non-positive bound fails loudly naming the bad argument (matching `prng.next_below`) instead of returning a plausible-looking `0`. Clears the sign bit, then takes the remainder; the modulo bias is negligible for ordinary small ranges. For cryptographic uniformity draw bytes with `hex` instead.

#### `fn bool(rand: Rand) -> Bool`

A fair coin.

#### `fn hex(rand: Rand, nbytes: Int) -> String`

`nbytes` random bytes rendered as lowercase hex (2 chars per byte) — the form a WebAuthn challenge, a CSRF nonce, or a session/token id wants. Draws full 64-bit words and truncates to the requested length.

## `reflect`

Reflection: a value's structure as data, so one function can work over any type. `reflect(x)` returns a `Mirror` describing `x`: a record's named fields, a sum type's variant, a list's elements, or a scalar. Code that would otherwise need a per-type `derive` (JSON encoding, debug rendering, structural diffing) is written once against `Mirror`. `MNil` is the unit value and maps to JSON null. Scalars, `Bytes`, `Ordering`, and the built-in containers (`List`, `Option`, `Result`, `Set`, tuples through arity 8, `Dict`) are reflectable out of the box. Container reflection is an encoding/debug protocol, not a promise that `derive(Deserialize)` can rebuild every container shape: `Dict` reflects as a string-keyed object. A user `type` becomes reflectable when you add `derive(Reflect)` to it (which needs `import reflect`), much like Zig's `@typeInfo` but opt-in per type — so `reflect(x)` / `json.stringify(x)` work without a per-type macro once the type derives it.

`reflect` is a trait method; `derive(Reflect)` generates `impl Reflect for T`, building the `Mirror` from the declared fields and variants. The scalar impls below are the leaves.

#### `type Mirror`

The reflected shape of a value.

- `MInt(Int)`
- `MFloat(Float)`
- `MBool(Bool)`
- `MString(String)`
- `MList(List(Mirror))`
- `MTuple(List(Mirror))`
- `MRecord(String, List((String, Mirror)))`
- `MVariant(String, String, List(Mirror))`
- `MNil`

#### `type DynamicFieldValue`

Compiler-authenticated field projection payload. The two descriptor IDs bind the accessor's concrete receiver and returned field to the runtime shape plan; `dynamic.field` validates both before constructing a new Dynamic envelope.

- `DynamicFieldValue(Int, Int, dyn Reflect)`

#### `trait Reflect`

Values that can describe themselves as a `Mirror`.

- `fn reflect(self) -> Mirror`
- `fn __dynamic_field(self, name: String) -> Option(DynamicFieldValue)` _(default)_

#### `fn reflect_bytes(b: Bytes) -> Mirror`

Bytes are binary data, not text. Reflection exposes their raw byte values as a list of Ints, so JSON/debug consumers can inspect payloads without a lossy UTF-8 decode.

#### `fn reflect_one(x: a) -> Mirror where a: Reflect`

Reflect a single value through a generic helper. Generated record/tuple impls use this at heterogeneous field sites so each call specializes independently.

#### `fn reflect_option(o: Option(a)) -> Mirror where a: Reflect`

Reflect an `Option` to a `Some`/`None` `MVariant`. Match bindings retain the constructor field's generic type, so the payload dispatches directly.

#### `fn reflect_list(xs: List(a)) -> Mirror where a: Reflect`

Reflect a list of `Reflect` elements. This is a free function rather than an `impl Reflect for List(a)` because method dispatch on a `List` receiver binds to the `list` module, so the generated `reflect` for a record's List field calls this.

#### `fn reflect_result(r: Result(a, e)) -> Mirror where a: Reflect, e: Reflect`

Reflect a `Result` to an `Ok`/`Err` `MVariant`, dispatching directly on the typed constructor binding.

#### `fn reflect_set(s: Set(a)) -> Mirror where a: Reflect`

A `Set` reflects to the same `MList` shape as its insertion-order `to_list` view. Reflection is an encoding protocol, not a reconstruction protocol, so JSON sees a set as an array and `derive(Deserialize)` remains per-type.

#### `fn reflect_dict(d: Dict(k, v)) -> Mirror where k: Reflect, v: Reflect`

A `Dict` reflects to an `MRecord` — the same string-keyed shape a record uses, so `json` encodes it as an object and `debug` renders it record-style. Each key is rendered to a string (an object/record key is always a string), each value reflects through the `v: Reflect` bound. This is intentionally one-way for encoding/debug: reflection does not preserve enough key structure for a general `Dict(k, v)` deserialize round trip. A free function for the same reason `reflect_list` is: a `Dict` receiver's `.reflect()` would bind to the `dict` module, so the generated `reflect` for a `Dict` field routes here.

#### `fn debug(x: a) -> String where a: Reflect`

`debug(x)` renders any value from its reflection, using the same `reflect` that backs `json`.

### Trait implementations

#### `impl Reflect for Int`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Float`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Bool`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for String`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Bytes`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Nil`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Duration`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Ordering`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for List(a) where a: Reflect`

`List` and `Option` reflect through generic impls, so `reflect(x)` (and therefore `json.stringify`, `debug`, and any other reflective consumer) works on a bare list or option, not only on one held in a record field. Each impl specializes per element: dispatch falls back from `List<Int>` to the generic `List` impl, and the element type resolves through the `where` bound.

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Option(a) where a: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Result(a, e) where a: Reflect, e: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Set(a) where a: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for Dict(k, v) where k: Reflect, v: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for (a, b) where a: Reflect, b: Reflect`

Tuples reflect to `MTuple` (a JSON array, or a parenthesized debug). Each supported arity has its own impl; `reflect_one` on the destructured slots dispatches per slot type. The protocol surface is explicit through arity 8; larger tuples remain structural values but do not carry blanket `Reflect`.

- `fn reflect(self) -> Mirror`

#### `impl Reflect for (a, b, c) where a: Reflect, b: Reflect, c: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for (a, b, c, d) where a: Reflect, b: Reflect, c: Reflect, d: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for (a, b, c, d, e) where a: Reflect, b: Reflect, c: Reflect, d: Reflect, e: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for (a, b, c, d, e, f) where a: Reflect, b: Reflect, c: Reflect, d: Reflect, e: Reflect, f: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for (a, b, c, d, e, f, g) where a: Reflect, b: Reflect, c: Reflect, d: Reflect, e: Reflect, f: Reflect, g: Reflect`

- `fn reflect(self) -> Mirror`

#### `impl Reflect for (a, b, c, d, e, f, g, h) where a: Reflect, b: Reflect, c: Reflect, d: Reflect, e: Reflect, f: Reflect, g: Reflect, h: Reflect`

- `fn reflect(self) -> Mirror`

## `regex`

Regular expressions, powered by the Rust `regex` crate (RE2 semantics): linear time, with full alternation `a|b` and grouping `(...)` — and a loud error, not a silent non-match, on an invalid pattern. The engine itself is the native `match_spans`, which returns the character spans of every match; the whole public API here is built on those spans in plain witchy, so it runs the same on the interpreter and the compiled backend. Positions are character indices.

#### `fn matches(pattern: String, text: String) -> Bool`

True if `pattern` matches anywhere in `text`.

#### `fn find(pattern: String, text: String) -> Option((Int, Int))`

The leftmost match as a (start, end) character span — end exclusive — or None.

#### `fn find_all(pattern: String, text: String) -> List((Int, Int))`

Every non-overlapping match, leftmost first.

#### `fn extract(pattern: String, text: String) -> List(String)`

The matched substrings, leftmost first: extract("\\d+", "a1b22") is ["1", "22"].

#### `fn replace_all(pattern: String, text: String, replacement: String) -> String`

`text` with every match replaced (the replacement is literal — no `$1` group expansion): replace_all("\\s+", "a  b", "-") is "a-b".

#### `fn split(pattern: String, text: String) -> List(String)`

`text` split on every match: split(",\\s*", "a, b,c") is ["a", "b", "c"].

## `result`

The witchy standard `Result` type and helpers. `Result`, `Ok`, and `Err` are prelude names and never need an import; `import result` brings in the qualified helper forms such as `result.map_ok`. Pure and capability-free.

#### `type Result`

- `Ok(a)`
- `Err(e)`

#### `fn map_err(r: Result(a, e), f: fn(e) -> g) -> Result(a, g)`

Transform the Err value, leaving an Ok untouched.

#### `fn all(xs: List(Result(a, e))) -> Result(List(a), e)`

Collect a list of Results into a Result of the list: `Ok` of every value in order, or the first `Err` encountered (the "sequence" of fallible steps).

#### `fn partition(xs: List(Result(a, e))) -> (List(a), List(e))`

Split a list of Results into the Ok values and the Err values, each in order — for batch work that reports every failure, not just the first.

#### `Result.is_ok() -> Bool`

True if the result is an Ok.

#### `Result.is_err() -> Bool`

True if the result is an Err.

#### `Result.unwrap_or(default: a) -> a`

The Ok value, or `default` if it's an Err.

#### `Result.unwrap_or_else(f: fn() -> a) -> a`

The Ok value, or the result of calling `f` (a lazily-computed default).

#### `Result.unwrap_err_or(default: e) -> e`

The Err value, or `default` if it's Ok. This is the error-side counterpart of `unwrap_or`: it is a defaulting helper, not a strict assertion that the result is Err.

#### `Result.map_ok(f: fn(a) -> b) -> Result(b, e)`

Transform the Ok value, leaving an Err untouched.

#### `Result.map_err(f: fn(e) -> g) -> Result(a, g)`

Transform the Err value, leaving an Ok untouched.

#### `Result.map_or(default: b, f: fn(a) -> b) -> b`

Apply `f` to the Ok value, or return `default` for an Err — `map_ok` then `unwrap_or` in one step.

#### `Result.and_then(f: fn(a) -> Result(b, e)) -> Result(b, e)`

Chain a fallible step: apply `f` (which itself yields a Result) to the Ok value, or propagate the Err unchanged.

#### `Result.or(alt: Result(a, e)) -> Result(a, e)`

The result if it is Ok, otherwise the `alt` result.

#### `Result.or_else(f: fn(e) -> Result(a, e)) -> Result(a, e)`

The result if it is Ok, otherwise the result produced by applying `f` to the error — a lazy, error-aware recovery step.

#### `Result.ok() -> Option(a)`

The Ok value as `Some`, or `None` for an Err — discards the error, turning a Result into an Option.

#### `Result.err() -> Option(e)`

The Err value as `Some`, or `None` for an Ok.

#### `Result.flatten() -> Result(a, e)`

Collapse one layer of nesting: `Ok(Ok(v))` becomes `Ok(v)`; `Ok(Err(e))` and `Err(e)` become `Err(e)`. The Result counterpart of `option.flatten`.

## `rights`

rights — rights-precise reasoning over capability footprints.

A capability is rendered the way the compiler's footprint prints it: "Console", "Dir[Read]", "Net[Connect, Tcp]". A *declared* capability covers a *demanded* one when they share a base kind AND the declared authority is at least as broad: a bare "Net" admits any rights of that kind, while "Net[Connect]" admits only a subset — so Net[Connect] does NOT cover full Net. This rights-precision is what the package manager's declared-vs-actual check and the block-on-widening gate both rely on, so it lives in one tested place.

#### `fn covers(declared: String, demanded: String) -> Bool`

Whether `declared` covers `demanded` (same kind, broad enough rights).

#### `fn any_covers(declared: List(String), demanded: String) -> Bool`

Whether any capability in `declared` covers `demanded`.

#### `fn uncovered(declared: List(String), demanded: List(String)) -> List(String)`

The demanded capabilities no declared capability covers — the gap a gate blocks on (an under-declaration, or a widening of authority). Empty means admitted.

#### `fn is_full(cap: String) -> Bool`

A bracket-free capability ("Net") is the full authority of its kind.

#### `fn base_name(cap: String) -> String`

The base kind: "Dir[Read]" -> "Dir"; "Console" -> "Console".

#### `fn rights_of(cap: String) -> List(String)`

The rights inside "Kind[A, B]" as ["A", "B"] (trimmed, blanks dropped).

## `secretstore`

secretstore — read named secrets from the host-granted `SecretStore`. The secrets come from `--secret name=value` / `--secret-file name=path` (append `,use-only` to forbid `crypto.reveal`). `--signing-key <path>` grants the `signing` secret as a protected, non-revealable signing key — it is NOT the same as `--secret-file signing=<path>`, which grants an ordinary revealable named secret. Their bytes stay host-side. `get` is intercepted by the runtime, since a `SecretStore` is a capability, not plain data. A `Secret` is opaque host-held material consumed by specific operations: `crypto.sign` / `crypto.public_key` (Ed25519 signing keys), `server.serve_tls` / `serve_tls_n` (a TLS private key, by opaque reference), and `crypto.reveal` — which succeeds only for revealable value secrets, and errors on signing keys and use-only secrets.

#### `fn get(store: SecretStore, name: String) -> Option(Secret)`

Fetch the secret named `name`, or `None` if it was not granted.

#### `fn require(store: SecretStore, name: String) -> Secret`

Fetch a *required* secret named `name`, returning the `Secret` directly. Use this when absence is a configuration error (e.g. a server's root signing key): it fails loudly rather than handing back an `Option` to unwrap. The body is a placeholder — the runtime intercepts the call (interpreter) / lowers it to a host Secret lookup (WASM); it is never executed.

## `semver`

semver — semantic versions and constraints, for dependency resolution.

Intentionally minimal (matching the package manager's needs): `major.minor.patch` versions and the `^`, `~`, exact, `>=`, and `*` constraints — enough for deterministic resolution without a full SemVer grammar. A missing component parses as 0 (`1.2` is `1.2.0`); a non-numeric component is an error.

#### `sealed type Version derive(PartialEq, Eq, PartialOrd, Ord)`

A version orders by major, then minor, then patch — exactly the lexicographic order `derive(Ord)` gives the fields in declaration order, so `<`, `>`, and `==` compare versions directly. `Version` is sealed (RFC-0065): external code cannot forge one with the raw data constructor, so a `Version` can only be built through this module — `parse`, which rejects negative components, or `version`. This closes the arbitrary-raw-construction vector of BUG-191 (`semver.Version(-1, 2, 3)` from another module is now a compile error).

- `Version { major: Int, minor: Int, patch: Int }`

#### `sealed type Req`

A version constraint (requirement). `Req` is sealed so requirements retain the component precision recorded by `parse_req`; use that parser rather than forging a raw variant.

- `Caret(Version, Int)`
- `Tilde(Version, Int)`
- `Exact(Version)`
- `AtLeast(Version)`
- `Any`

#### `type SemverError`

Matchable semantic-version parse failures. Requirement parsing reuses the same cases because every non-`*` requirement wraps a version coordinate.

- `BadVersionShape(String)`
- `SignedVersionComponent(String)`
- `NegativeVersionComponent(String)`
- `NonNumericVersionComponent(String)`

#### `fn semver_error_message(e: SemverError) -> String`

#### `fn version(major: Int, minor: Int, patch: Int) -> Version`

A convenience constructor for known-good components (e.g. a computed bump). A negative component is not a version at all, so it is a contract violation (RFC-0044 rule 3): fail loudly naming the bad coordinate rather than minting an impossible one. Untrusted input belongs in `parse`, which returns `Err` instead of aborting.

#### `fn parse(s: String) -> Result(Version, SemverError)`

Parse `major.minor.patch` (missing trailing components default to 0), returning a typed error a library can match.

#### `fn parse_string(s: String) -> Result(Version, String)`

Parse with String errors for application-style boundaries.

#### `fn format(v: Version) -> String`

#### `fn compare(a: Version, b: Version) -> Int`

-1 if a < b, 0 if equal, 1 if a > b. `Version` derives `Ord`, so callers that only need a Bool can compare with `<` / `>` / `==` directly.

#### `fn less(a: Version, b: Version) -> Bool`

#### `fn parse_req(s: String) -> Result(Req, SemverError)`

#### `fn parse_req_string(s: String) -> Result(Req, String)`

#### `fn matches(req: Req, v: Version) -> Bool`

Whether `v` satisfies the constraint `req`.

#### `fn best(versions: List(Version), req: Req) -> Option(Version)`

The highest version in `versions` that satisfies `req`, or None if none do. Keep the matching versions and fold to the highest — dogfoods std/iter.

### Trait implementations

#### `impl Show for SemverError`

- `fn show(self) -> String`

#### `impl Error for SemverError`

#### `impl From(SemverError) for String`

- `fn from(value: SemverError) -> Self`

#### `impl Show for Version`

- `fn show(self) -> String`

## `server`

The witchy web framework — a slice of axum/tower over the `Net` capability, built on the shared `Request`/`Response` types in `http`.

A handler is a pure `fn(Request) -> Response`: it has NO capability parameters, so it is *structurally* unable to touch the network, filesystem, or console. To give a handler authority (a logger, a store, an outbound client), capture it in the closure — capture IS dependency injection. `serve` holds the `Net` to listen and never hands it to a handler, so even a mounted third-party handler can only compute over the request; it cannot phone home.

  let app = server.router()       .get("/", home)       .get("/users/:id", show)       .layer(logging(console))   server.serve(net, "127.0.0.1:8080", app)

#### `type RequestParseError`

Matchable failures when parsing an inbound HTTP request frame. The server boundary maps these to 400 responses, but libraries and tests can match them without scraping a rendered response body.

- `UnsupportedTransferEncoding`
- `ConflictingContentLength`
- `BadRequestLine`

#### `type Route`

One route: HTTP method, a path pattern (`:param` captures a segment, `*rest` captures the remainder), and the handler.

- `Route(String, String, fn(Request) -> Response)`

#### `type Router`

A router: its routes plus the middleware layers wrapping the whole dispatch.

- `Router(List(Route), List(fn(fn(Request) -> Response) -> fn(Request) -> Response))`

#### `fn request_parse_error_message(e: RequestParseError) -> String`

#### `fn method(req: Request) -> String`

#### `fn path(req: Request) -> String`

The request path, normalized and percent-decoded for the handler. Routing and this accessor see the same normalized form (BUG-432): consecutive slashes are collapsed and trailing slashes stripped. Percent-decoding is applied after normalization so a `%2F` stays inside one segment (no forged separator, BUG-375).

#### `fn param(req: Request, name: String) -> Option(String)`

A captured path parameter (`:name`), or None if absent.

#### `fn param_or(req: Request, name: String, default: String) -> String`

A captured path parameter, or `default` if absent.

#### `fn query(req: Request, name: String) -> Option(String)`

A query-string parameter (`?name=...`), or None if absent.

#### `fn query_or(req: Request, name: String, default: String) -> String`

A query-string parameter, or `default` if absent.

#### `fn request_header(req: Request, name: String) -> Option(String)`

A request header, looked up case-insensitively, or None.

#### `fn request_body(req: Request) -> String`

#### `fn json_body(req: Request) -> Result(Json, json.DecodeError)`

Decode the request body as JSON — the role of axum's `Json` extractor. Returns a typed Err (rather than panicking) on malformed input, so handlers stay total without collapsing trust-boundary failures to display text.

#### `fn json_body_string(req: Request) -> Result(Json, String)`

Compatibility bridge for applications that intentionally want rendered errors.

#### `fn form_body(req: Request) -> List((String, String))`

Parse an `application/x-www-form-urlencoded` body (`a=1&b=2`) into key/value pairs — for HTML form POSTs.

#### `fn form_field(req: Request, name: String) -> Option(String)`

A single form field, or None if absent.

#### `fn form_field_or(req: Request, name: String, default: String) -> String`

A single form field, or `default` if absent.

#### `fn text(code: Int, b: String) -> Response`

#### `fn html(code: Int, b: String) -> Response`

#### `fn json(code: Int, b: String) -> Response`

`b` is an already-encoded JSON string (e.g. from `json.encode`).

#### `fn json_value(code: Int, j: Json) -> Response`

A JSON response from a `Json` value — encodes it for you.

#### `fn send(code: Int, value: a) -> Response where a: Reflect`

A JSON response from any reflectable value. Reflection serializes it, so a handler can return `server.send(200, .{names: names})`, or a record, without building `Json` by hand. Use `json` or `json_value` for pre-encoded bytes or a `Json` value.

#### `fn status_only(code: Int) -> Response`

#### `fn ok(b: String) -> Response`

#### `fn created(b: String) -> Response`

#### `fn accepted(b: String) -> Response`

#### `fn no_content() -> Response`

#### `fn bad_request(b: String) -> Response`

#### `fn unauthorized(b: String) -> Response`

#### `fn forbidden(b: String) -> Response`

#### `fn server_error(b: String) -> Response`

#### `fn not_found() -> Response`

#### `fn method_not_allowed() -> Response`

#### `fn redirect(location: String) -> Response`

#### `fn with_header(resp: Response, name: String, value: String) -> Response`

Return `resp` with an extra header — for handlers and middleware that decorate a response (e.g. add a `set-cookie` or a tracing header). The stored name is lowercased: `Response` documents its header list as lowercase so `http.header(resp, name)` can look up case-insensitively — headers are case-insensitive on the wire, and rendering emits the lowercase form.

#### `fn with_status(resp: Response, code: Int) -> Response`

Return `resp` with its status code replaced.

#### `fn router() -> Router`

#### `fn route(r: Router, m: String, p: String, h: fn(Request) -> Response) -> Router`

#### `fn parse_request(raw: String) -> Result(Request, RequestParseError)`

Parse a whole raw HTTP/1.1 request string into a `Request`, preserving malformed framing as a typed error. This is the network-free mirror of the socket reader and of `http.try_parse_response`. Public so a router can be tested, or a request framed by another transport re-parsed, without a socket.

#### `fn parse_request_response(raw: String) -> Result(Request, Response)`

Application/server-boundary bridge for callers that want the old "parse or 400 Response" shape explicitly.

#### `fn handle(app: Router, req: Request) -> Response`

Dispatch `req` through `app` (all routes and middleware layers) and return the Response — the whole request pipeline WITHOUT a socket. The axum "oneshot" analog: handlers and routers become unit-testable with a `Request` literal, and it is the in-process way to call one app from another. `serve*` is this plus the accept loop.

#### `fn render(resp: Response) -> String`

Serialize a `Response` to its HTTP/1.1 wire form (inverse of `http.parse_response`). The framing headers (Content-Length, Connection) are owned here; a status outside 100..599 traps. Public so a test or a custom transport can render a Response itself.

#### `fn render_for(resp: Response, method: String) -> String`

`render`, honoring the request method: a HEAD response carries the same status/headers — including the Content-Length the body WOULD have — but the body bytes themselves are never sent (RFC 9110 §9.3.2). This is enforced at the rendering boundary so a `.head(...)` handler may simply return the GET response and the framework suppresses the body.

#### `fn serve(net: Net[Listen, Tcp], addr: String, app: Router)`

Serve `app` on `addr` forever, using ALL cores. Needs the `Net` capability to listen; handlers never receive it. `serve_pool` spawns one worker VM per core, each re-running this program and accepting from the SAME bound listener — the kernel load-balances connections across them, so the server scales across cores with no extra effort from you. Handlers are pure `fn(Request) -> Response` whose state lives in their captured capabilities (e.g. a store `Dir` = the filesystem), so the workers are interchangeable. (For a single-threaded server, use `serve_one`.)

#### `fn serve_one(net: Net[Listen, Tcp], addr: String, app: Router)`

Serve `app` on `addr` forever on a SINGLE core (one accept loop, no worker pool) — for servers with per-process in-memory state, or when one core is plenty.

#### `fn serve_n(net: Net[Listen, Tcp], addr: String, app: Router, n: Int)`

Serve exactly `n` requests then return — for tests and one-shot servers.

#### `fn serve_tls(net: Net[Listen, Tcp], addr: String, cert_pem: String, key: Secret, app: Router)`

Serve `app` over HTTPS on `addr` forever, using ALL cores — `serve` with TLS terminated by the host. `cert_pem` is the PUBLIC certificate chain (PEM text — inline, or read via an ordinary `Dir` grant); `key` is the private key as a `Secret` (`secretstore.require(store, "tls-key")`), consumed by opaque host reference: the key bytes never enter this program's memory, so even a bug in a handler cannot exfiltrate them. Grant the key use-only (`--secret-file tls-key=key.pem,use-only`) and `crypto.reveal` on it errors too. A malformed or mismatched cert/key fails LOUDLY here at startup; an individual failed handshake (a plaintext client, a bad ClientHello) drops that connection and the server keeps serving. Handlers, `Router`, and `Request`/`Response` are unchanged — TLS is transparent above the accepted connection.

#### `fn serve_tls_n(net: Net[Listen, Tcp], addr: String, cert_pem: String, key: Secret, app: Router, n: Int)`

Serve exactly `n` HTTPS requests then return — `serve_tls`'s one-shot/test twin (the TLS handling and key discipline of `serve_tls`, the loop shape of `serve_n`).

#### `Router.get(p: String, h: fn(Request) -> Response) -> Router`

#### `Router.post(p: String, h: fn(Request) -> Response) -> Router`

#### `Router.put(p: String, h: fn(Request) -> Response) -> Router`

#### `Router.delete(p: String, h: fn(Request) -> Response) -> Router`

#### `Router.patch(p: String, h: fn(Request) -> Response) -> Router`

#### `Router.head(p: String, h: fn(Request) -> Response) -> Router`

#### `Router.any(p: String, h: fn(Request) -> Response) -> Router`

#### `Router.nest(prefix: String, sub: Router) -> Router`

#### `Router.layer(mw: fn(fn(Request) -> Response) -> fn(Request) -> Response) -> Router`

### Trait implementations

#### `impl Show for RequestParseError`

- `fn show(self) -> String`

#### `impl Error for RequestParseError`

#### `impl From(RequestParseError) for String`

- `fn from(value: RequestParseError) -> Self`

## `set`

Set(a) — an unordered collection of distinct values. Members are compared by value equality (a `where a: Eq` bound on every operation that compares), so sets of Ints, Strings, tuples, or your own `Eq` types all work. Build one with `set.new()` / `set.from_list(xs)`, test membership with `s.contains(x)`, and reach for `union`/`intersection`/`difference` for the algebra. A `Set` whose members are `Show` renders as `{a, b, c}` through interpolation or `show.say`; `s.to_list()` returns the members in insertion order.

#### `sealed type Set(a)`

- `Set { items: List(a) }`

#### `fn new() -> Set(a)`

The empty set.

#### `fn from_list(xs: List(a)) -> Set(a) where a: Eq`

A set of the distinct values in `xs` (duplicates collapse, keeping the first occurrence of each member).

#### `fn to_list(s: Set(a)) -> List(a)`

The members as a list, in insertion order. (Module-level because the compiler renders a `Show`able Set through `set.to_list`; the method delegates here.)

#### `Set.length() -> Int`

The number of distinct members.

#### `Set.is_empty() -> Bool`

Whether the set has no members.

#### `Set.to_list() -> List(a)`

The members as a list, in insertion order.

#### `Set.insert(x: a) -> Bool`

Add `x`, returning whether it was newly inserted.

#### `Set.remove(x: a) -> Bool`

Remove `x`, returning whether it was present.

#### `Set.contains(x: a) -> Bool`

Whether `x` is a member of the set.

#### `Set.union(other: Set(a)) -> Set(a)`

Every member of either set.

#### `Set.intersection(other: Set(a)) -> Set(a)`

The members in both sets.

#### `Set.difference(other: Set(a)) -> Set(a)`

The members of this set that are not in `other`.

#### `Set.symmetric_difference(other: Set(a)) -> Set(a)`

The members in exactly one of the two sets.

#### `Set.is_subset(other: Set(a)) -> Bool`

Whether every member of this set is also in `other`.

#### `Set.is_disjoint(other: Set(a)) -> Bool`

Whether the two sets share no members.

### Trait implementations

#### `impl PartialEq for Set(a) where a: Eq`

Two sets are equal when they hold exactly the same members, regardless of insertion order or the backing list's layout: `from_list([1, 2, 3])` equals `from_list([3, 2, 1])`. Without this impl, `==` falls back to the derived structural equality of the backing list (order-sensitive on the interpreter, and unresolvable on the compiled backend), which is not set equality.

- `fn eq(self, other: Self) -> Bool`

#### `impl Eq for Set(a) where a: Eq`

## `show`

The witchy standard `Show` trait: render a value as a `String`. Built-in impls cover the scalars — `Int`, `Float`, `Bool`, `String`, `Bytes`, `Duration` (which shows in its human form, `1m30s`, not raw milliseconds) — and the built-in comparison result `Ordering`. A `List`, `Dict`, `Set`, `Option`, `Result`, or tuple through arity 8 whose elements are themselves `Show` renders structurally through each element's `Show` (`[a, b]`, `{k: v}`, `Some(x)`), so `show.say(console, [1, 2, 3])` and `show.say(console, someSet)` just work — and a custom element `Show` is honored (`[P<1,2>, P<3,4>]`). Implement `Show` for your own types to give them a custom readable form. `Show` is preluded: interpolation (`"${x}"`) always honors a relevant impl, while values without one keep the structural default. Pure except `say`, which takes the `Console` it prints to.

#### `trait Show`

- `fn show(self) -> String`

#### `fn render(x: impl Show) -> String`

Render one `Show` value to a `String` — `render(point)`, `render(90000ms)`, `render([1, 2, 3])`. This is the one public renderer (RFC-0053): string interpolation `"${x}"` lowers to `render(x)` for any `x` whose concrete type has a relevant `Show` impl, so interpolation and `show.say` agree. A type without a `Show` path keeps interpolation's byte-identical structural default.

#### `fn say(console: Console, x: impl Show)`

Print any `Show` value without converting it by hand — `show.say(console, 42)`, `show.say(console, point)`, `show.say(console, [1, 2, 3])`. This is the explicit Show-accepting print helper; interpolation remains the normal inline rendering form. (A thin wrapper kept out of the `print` builtin so a builtin never depends on a std trait.)

### Trait implementations

#### `impl Show for Int`

- `fn show(self) -> String`

#### `impl Show for Bool`

- `fn show(self) -> String`

#### `impl Show for String`

- `fn show(self) -> String`

#### `impl Show for Bytes`

- `fn show(self) -> String`

#### `impl Show for Float`

- `fn show(self) -> String`

#### `impl Show for Duration`

A Duration SHOWS in its human form ("1m30s"), unlike the structural `to_string`, which renders the underlying milliseconds — exactly the kind of custom rendering `Show` exists for.

- `fn show(self) -> String`

#### `impl Show for Ordering`

- `fn show(self) -> String`

#### `impl Show for List(a) where a: Show`

A list renders as `[a, b, c]`, each element through its own `Show` — the same structural form `"${xs}"` produces, but honoring a custom element `Show`.

- `fn show(self) -> String`

#### `impl Show for Option(a) where a: Show`

An option renders as `Some(x)` / `None`, the payload through its `Show`. Match bindings retain the constructor field's generic type, so dispatch is direct: no temporary 0-or-1 list is needed to recover `a`.

- `fn show(self) -> String`

#### `impl Show for Result(a, e) where a: Show, e: Show`

A result renders as `Ok(x)` / `Err(e)`, each payload through its `Show`.

- `fn show(self) -> String`

#### `impl Show for Dict(k, v) where k: Show, v: Show`

A dict renders as `{k1: v1, k2: v2}` (insertion order), keys and values each through their `Show`.

- `fn show(self) -> String`

#### `impl Show for Set(a) where a: Show`

A set renders as `{a, b, c}` (members in insertion order), each through its `Show`.

- `fn show(self) -> String`

#### `impl Show for (a, b) where a: Show, b: Show`

Tuples render as `(a, b)`; each supported arity has its own impl and dispatches per slot, as the derives do. The protocol surface is explicit through arity 8; larger tuples remain structural values but do not carry blanket `Show`.

- `fn show(self) -> String`

#### `impl Show for (a, b, c) where a: Show, b: Show, c: Show`

- `fn show(self) -> String`

#### `impl Show for (a, b, c, d) where a: Show, b: Show, c: Show, d: Show`

- `fn show(self) -> String`

#### `impl Show for (a, b, c, d, e) where a: Show, b: Show, c: Show, d: Show, e: Show`

- `fn show(self) -> String`

#### `impl Show for (a, b, c, d, e, f) where a: Show, b: Show, c: Show, d: Show, e: Show, f: Show`

- `fn show(self) -> String`

#### `impl Show for (a, b, c, d, e, f, g) where a: Show, b: Show, c: Show, d: Show, e: Show, f: Show, g: Show`

- `fn show(self) -> String`

#### `impl Show for (a, b, c, d, e, f, g, h) where a: Show, b: Show, c: Show, d: Show, e: Show, f: Show, g: Show, h: Show`

- `fn show(self) -> String`

## `string`

The witchy standard string library. Like `list`, it is pure: it declares no capability parameters, so importing it grants no authority. The primitive string operations (`split`, `replace`, `contains`, `to_upper`, ...) are builtins; these are the conveniences built on top of them.

#### `fn length(s: String) -> Int`

#### `fn char_count(s: String) -> Int`

#### `fn chars(s: String) -> List(String)`

#### `fn from_code(code: Int) -> String`

#### `fn split(s: String, sep: String) -> List(String)`

#### `fn contains(s: String, needle: String) -> Bool`

#### `fn starts_with(s: String, prefix: String) -> Bool`

#### `fn ends_with(s: String, suffix: String) -> Bool`

#### `fn replace(s: String, from: String, to: String) -> String`

#### `fn substring(s: String, start: Int, end: Int) -> String`

#### `fn to_upper(s: String) -> String`

#### `fn to_lower(s: String) -> String`

#### `fn trim(s: String) -> String`

#### `fn to_int(s: String) -> Int`

#### `String.length() -> Int`

The string's length in BYTES (UTF-8). For user-perceived characters, see `char_count`.

#### `String.char_count() -> Int`

The number of Unicode scalar values.

#### `String.chars() -> List(String)`

The characters, each as a single-character String — one O(n) pass, so callers can index characters in O(1).

#### `String.split(sep: String) -> List(String)`

Split on every occurrence of `sep`.

#### `String.contains(needle: String) -> Bool`

Whether `needle` occurs in `self`.

#### `String.starts_with(prefix: String) -> Bool`

#### `String.ends_with(suffix: String) -> Bool`

#### `String.index_of(needle: String) -> Option(Int)`

The character index (counted by Unicode scalar) of the first occurrence of `needle` as `Some`, or `None` when `needle` does not occur (RFC-0044 rule 1: absence is `Option`, never a -1 sentinel). An empty `needle` matches nothing (the module-wide empty-pattern rule, matching `last_index_of`/`count`). For a bare yes/no, use `contains`.

#### `String.replace(from: String, to: String) -> String`

Replace every occurrence of `from` with `to`.

#### `String.substring(start: Int, end: Int) -> String`

The substring from character index `start` (inclusive) to `end` (exclusive), counted by Unicode scalar; out-of-range indices clamp.

#### `String.to_upper() -> String`

ASCII case mapping (the portable set both backends share).

#### `String.to_lower() -> String`

#### `String.trim() -> String`

Strip leading and trailing ASCII whitespace.

#### `String.trim_start() -> String`

Remove leading whitespace.

#### `String.trim_end() -> String`

Remove trailing whitespace.

#### `String.to_int() -> Int`

Parse a decimal integer; junk, overflow, or an empty string ABORTS the program (a runtime error, not an `Err`) on every backend. For the total version that returns `Option(Int)`, see `parse_int`.

#### `String.repeat(n: Int) -> String`

Repeat a string `n` times.

#### `String.pad_left(width: Int, fill: String) -> String`

Left-pad `self` with copies of `fill` until it is `width` characters wide. The padding is trimmed to fit exactly, so any non-empty fill yields a result of exactly `width` chars; `self` is returned unchanged when already that long. An empty `fill` can never reach the promised width, so when padding is needed it fails loudly (RFC-0044 rule 3) instead of returning a short string.

#### `String.pad_right(width: Int, fill: String) -> String`

Right-pad `self` with copies of `fill` until it is `width` characters wide; an empty `fill` fails loudly when padding is needed (see `pad_left`).

#### `String.center(width: Int, fill: String) -> String`

Center `self` in a field `width` characters wide, padding both sides with `fill`; an odd remainder goes on the right. `self` is returned unchanged when already at least that wide; an empty `fill` fails loudly when padding is needed (see `pad_left`).

#### `String.strip_prefix(prefix: String) -> String`

Remove `prefix` from the front of `self` when present; otherwise return `self` unchanged. The complement of the `starts_with` builtin.

#### `String.strip_suffix(suffix: String) -> String`

Remove `suffix` from the end of `self` when present; otherwise return `self` unchanged. The complement of the `ends_with` builtin.

#### `String.char_at(i: Int) -> Option(String)`

The single character (as a String) at character index `i` (counted by Unicode scalar) as `Some`, or `None` when `i` is out of range (RFC-0044 rule 1: absence is `Option`, never a "" sentinel). For a clamping view use `substring`.

#### `String.is_empty() -> Bool`

Whether the string has no characters.

#### `String.reverse() -> String`

The string with its characters in reverse order. Counted by Unicode scalar (via `char_count`/`substring`), so multi-byte characters stay intact: `reverse("café")` is `"éfac"`.

#### `String.take(n: Int) -> String`

The first `n` characters (the whole string if it is shorter, "" if n <= 0).

#### `String.drop(n: Int) -> String`

All characters after the first `n` ("" if n covers the whole string).

#### `String.count(sub: String) -> Int`

The number of non-overlapping occurrences of `sub` in `self` (0 for an empty `sub`). After each match the search resumes past it.

#### `String.words() -> List(String)`

The whitespace-separated words of `self`: tabs, newlines, and carriage returns are treated as spaces, and empty pieces (from runs of whitespace) are dropped. `"the  quick\tfox".words()` is `["the", "quick", "fox"]`.

#### `String.replace_first(from: String, to: String) -> String`

Replace only the first occurrence of `from` with `to`; return `self` unchanged when `from` is absent. An empty `from` matches nothing (the module-wide empty-pattern rule: `count`/`index_of`/`last_index_of` treat it as absent). (The `replace` builtin replaces every occurrence.)

#### `String.split_once_opt(sep: String) -> Option((String, String))`

Split at the first occurrence of `sep` into `Some((before, after))`, with `sep` itself dropped. Returns `None` when `sep` is absent or empty, so parsing code can distinguish a missing separator from a present separator with an empty side (`"host"` vs `"host:"`). Counted by Unicode scalar.

#### `String.split_once(sep: String) -> (String, String)`

Split at the first occurrence of `sep` into `(before, after)`, with `sep` itself dropped. Compatibility wrapper: when `sep` is absent — including the empty separator, which matches nothing (mirroring `rsplit_once`) — returns `(s, "")`. Prefer `split_once_opt` for parsers and validators that need to distinguish absence from a present empty side. Counted by Unicode scalar.

#### `String.last_index_of(sep: String) -> Option(Int)`

The character index of the LAST occurrence of `sep` in `self` as `Some`, or `None` when absent or `sep` is empty (RFC-0044 rule 1: absence is `Option`, never -1). The right-to-left companion of `index_of`.

#### `String.rsplit_once_opt(sep: String) -> Option((String, String))`

Split on the LAST occurrence of `sep` into `Some((before, after))`, with `sep` itself dropped. Returns `None` when `sep` is absent or empty.

#### `String.rsplit_once(sep: String) -> (String, String)`

Split on the LAST occurrence of `sep` (e.g. a file extension): `rsplit_once` of `"a.b.c"` on `"."` is `("a.b", "c")`. Compatibility wrapper: when `sep` is absent the whole string is the right part: `("", s)` — mirroring `split_once`'s `(s, "")`. Prefer `rsplit_once_opt` when absence matters.

#### `String.parse_int() -> Option(Int)`

Safely parse a base-10 integer: an optional leading `-`/`+` then one or more digits. Returns None for empty, sign-only, non-digit, or out-of-range (beyond the i64 range) input — so it never traps the way the raw `string_to_int` builtin can.

#### `String.lines() -> List(String)`

Split text into its newline-separated lines.

## `task`

std/task — the cooperative task substrate and its executor.

A `Task(a)` is a CPS-over-closures computation that, when stepped, either completes (`Done`) or yields an effect back to the executor: cooperate (`Yield`), `spawn` a child (`Fork`), `join` one (`Wait`), or a channel op (`Open`/`Push`/`Pull`/`PullAny`, produced by `std/chan`). `run` drives a task (and everything it spawns) to completion on a deterministic round-robin schedule, so a concurrent run is byte-identical on the interpreter and the compiled WebAssembly — no scheduler state in the runtime, no `Pin`.

This module is the scheduling core: the `Task` monad, `spawn`/`join`/ `yield_now`, and the executor. First-class channels are layered on top in `std/chan`; lightweight value-returning structured concurrency (`join_all`/ `select` over independent futures) lives in `std/future`.

Messages: the executor is ERASED (RFC-0055). Its buffers, `Step`, and `Slot` carry the opaque `__Msg`, so ONE program can run channels of many different message types — a library may use channels privately without forcing its type on the whole program. The typed channel endpoints (`Sender(m)`/`Receiver(m)` in `std/chan`) erase a message on `send` and recover it on `recv`; the erasure is representationally the identity on both backends (a message already rides the universal slot), so interleavings stay byte-identical. Spawned tasks return `Nil`; a task reports a result by sending it on a channel, not by returning it (a typed `JoinHandle(T)` would force a native runtime and break the parity contract).

The `async`/`await` CPS transform lowers onto this substrate (`task.lazy`/ `and_then`/`done`/`run`), so `chan.recv(rx).await` / `chan.send(tx, x).await` work in async fns.

#### `sealed type Step(a)`

What a task yields to the executor when stepped. `a` is the task's own result. The channel effects (`Open`/`Push`/`Pull`/`PullAny`) are produced by the private bridge below; the executor interprets every variant. Messages are the erased `__Msg` (RFC-0055) — std/chan's typed endpoints (un)wrap at the boundary.

- `Done(a)`
- `Yield(Task(a))`
- `Fork(Task(Nil), fn(Int) -> Task(a))`
- `Open(Int, fn(Int) -> Task(a))`
- `Push(Int, __Msg, fn(Nil) -> Task(a))`
- `Pull(Int, fn(Option(__Msg)) -> Task(a))`
- `PullAny(List(Int), fn(Option((Int, __Msg))) -> Task(a))`
- `Wait(Int, fn(Nil) -> Task(a))`
- `Cancel(Int, fn(Nil) -> Task(a))`

#### `sealed type Task(a)`

A task communicating via channels, producing an `a`.

- `Task(fn() -> Step(a))`

#### `sealed type Handle`

- `Handle(Int)`

#### `sealed type ChannelId`

Channel identity shared with std/chan. User source cannot mint one; the compiler-private bridge functions below are callable only from std/task and std/chan.

- `ChannelId(Int)`

#### `sealed type Slot`

A scheduling slot: running, parked on a channel recv/send or on a join, or done. Parked messages are the erased `__Msg` (RFC-0055).

- `Active(Task(Nil))`
- `WaitRecv(Int, fn(Option(__Msg)) -> Task(Nil))`
- `WaitSend(Int, __Msg, fn(Nil) -> Task(Nil))`
- `WaitAny(List(Int), fn(Option((Int, __Msg))) -> Task(Nil))`
- `WaitJoin(Int, fn(Nil) -> Task(Nil))`
- `Ended`

#### `fn done(x: a) -> Task(a)`

A finished task.

#### `fn ready_unit() -> Task(Nil)`

An already-complete `Task(Nil)` — the async/await lowering target for a body that falls off its end.

#### `fn yield_now() -> Task(Nil)`

Hand control back to the executor once, then continue.

#### `fn and_then(t: Task(a), k: fn(a) -> Task(b)) -> Task(b)`

Sequence: run `t`, then continue with `k` applied to its result. This is what `await` lowers to — the continuation `k` is the rest of the body.

#### `fn map(t: Task(a), f: fn(a) -> b) -> Task(b)`

Transform a task's result.

#### `fn lazy(thunk: fn() -> Task(a)) -> Task(a)`

Build the task `thunk()` lazily: nothing runs until this task is polled. A `Task` is a replayable execution recipe, not a memo cell; the standard driver advances through continuations, but driving the same task value again reruns its thunk.

#### `fn for_each(xs: List(a), f: fn(a) -> Task(Nil)) -> Task(Nil)`

Run `f(x)` as a task for each `x` in `xs`, in order — the lowering target for an `await` inside a `for x in xs:` loop.

#### `fn spawn(child: Task(Nil)) -> Task(Handle)`

Start `child` as a concurrent task; the returned handle completes when it does.

#### `fn join(h: Handle) -> Task(Nil)`

Wait for the spawned task behind `h` while the executor can make progress. If the whole executor reaches quiescence with this join parked, the close pass releases it even if the joined task has a continuation that will run afterward.

#### `fn cancel(h: Handle) -> Task(Nil)`

Cancel the spawned task behind `h`: it is stepped no further and is treated as finished, so anyone `join`ing it unblocks. Shallow (stops this one task, not its descendants) and idempotent (already-finished is a no-op). Deterministic on the round-robin schedule, hence byte-identical on both backends.

#### `fn run(root: Task(Nil))`

Drive `root` (and everything it spawns) to completion on a deterministic round-robin schedule. An async `main` lowers to a single `run` of its body.

## `testing`

The Witchy test support module. `witchy test <file>` discovers `test_*` functions and reports each as passing unless it aborts. Plain tests receive zero real authority. Deterministic capability parameters come only from a validated external fixture plan; this module provides assertions and ordinary collaborators, not capability constructors.

  import testing

  fn test_addition():       testing.assert_eq("${2 + 2}", "4")

  fn test_truth():       testing.assert(1 < 2, "one is less than two")

#### `type FixedClock`

- `FixedClock(Int, Int)`

#### `type FixedRand`

- `FixedRand(Int)`

#### `trait ClockSource`

An injectable wall/monotonic clock protocol. Production code can accept a generic `c: ClockSource`: real entrypoints pass their `Clock`, while unit tests pass `fixed_clock` and need no host grant.

- `fn wall_ms(self) -> Int`
- `fn monotonic_ns(self) -> Int`

#### `trait RandSource`

An injectable one-draw randomness protocol. Real entrypoints pass `Rand`; deterministic unit tests can pass `fixed_rand` without a host grant.

- `fn draw_u64(self) -> Int`

#### `fn fixed_clock(wall_ms: Int, monotonic_ns: Int) -> FixedClock`

A fixed, authority-free clock collaborator. This is ordinary data, not a forged `Clock`; inject it through `ClockSource` in deterministic unit tests.

#### `fn fixed_rand(value: Int) -> FixedRand`

A fixed, authority-free Rand collaborator. Every draw returns `value`, which makes boundary/error paths reproducible without pretending to be a real cryptographic randomness capability.

#### `fn assert(cond: Bool, msg: String)`

Abort the test with `msg` unless `cond` holds.

#### `fn assert_eq(got: String, want: String)`

Abort unless the two strings are equal, showing both. Convert values at the call site with interpolation, or with `show.render` when you want `Show`.

#### `fn assert_ne(got: String, other: String)`

Abort if the two strings ARE equal.

#### `fn assert_value_eq(got: a, want: a) where a: PartialEq, a: Show`

Abort unless two protocol values are equal, showing both through `Show`.

#### `fn assert_value_ne(got: a, other: a) where a: PartialEq, a: Show`

Abort if two protocol values ARE equal.

#### `fn assert_int_eq(got: Int, want: Int)`

Abort unless the two Ints are equal, showing both.

#### `fn fail_with(msg: String)`

Unconditional failure with a message (e.g. an unreachable branch).

### Trait implementations

#### `impl ClockSource for Clock`

- `fn wall_ms(self) -> Int`
- `fn monotonic_ns(self) -> Int`

#### `impl ClockSource for FixedClock`

- `fn wall_ms(self) -> Int`
- `fn monotonic_ns(self) -> Int`

#### `impl RandSource for Rand`

- `fn draw_u64(self) -> Int`

#### `impl RandSource for FixedRand`

- `fn draw_u64(self) -> Int`

## `time`

time — civil (UTC) date/time from a unix timestamp.

`std/duration` models *spans*; this module models *points* on the calendar. Given seconds since the unix epoch (1970-01-01T00:00:00Z), it computes the civil year/month/day/hour/minute/second and formats them. The conversions use the standard days<->civil algorithm (proleptic Gregorian), correct for any CE date and for negative timestamps (before 1970) via floor division.

#### `type TimeError`

Matchable civil-time construction and ISO 8601 parse failures.

- `YearOutOfRange(Int)`
- `MonthOutOfRange(Int)`
- `DayOutOfRange(Int, Int, Int)`
- `ClockOutOfRange(Int, Int, Int)`
- `InvalidIsoDate(String)`
- `MissingDateTimeSeparator(String)`
- `InvalidIsoTime(String)`
- `InvalidDigits(String, Int, Int, String)`
- `EmptyFractionalSeconds(String)`
- `BadUtcOffset(String, String)`
- `UtcOffsetOutOfRange(String)`

#### `sealed type DateTime`

- `DateTime(Int, Int, Int, Int, Int, Int)`

#### `fn time_error_message(e: TimeError) -> String`

#### `fn year(d: DateTime) -> Int`

#### `fn month(d: DateTime) -> Int`

#### `fn day(d: DateTime) -> Int`

#### `fn hour(d: DateTime) -> Int`

#### `fn minute(d: DateTime) -> Int`

#### `fn second(d: DateTime) -> Int`

#### `fn from_millis(ms: Int) -> DateTime`

The civil UTC date/time at `ms` MILLISECONDS since the unix epoch — what `clock.now()` returns, so `time.from_millis(clock.now())` is the idiom for "the current date/time".

#### `fn from_unix(secs: Int) -> DateTime`

The civil UTC date/time at `secs` SECONDS since the unix epoch (a classic unix timestamp). `clock.now()` returns milliseconds — use `from_millis` for it, or this becomes the year 58000. The formatted/parsing contract is a fixed four-digit CE year, so timestamps outside 0001..9999 are out of domain.

#### `fn to_unix(d: DateTime) -> Int`

The unix timestamp for a DateTime (its inverse).

#### `fn civil(y: Int, mo: Int, da: Int, h: Int, mi: Int, s: Int) -> Result(DateTime, TimeError)`

A DateTime from civil UTC components, validated — `civil(2026, 2, 30, ...)` is an Err, not a rollover. The typed error lets libraries classify malformed components without parsing display text.

#### `fn civil_string(y: Int, mo: Int, da: Int, h: Int, mi: Int, s: Int) -> Result(DateTime, String)`

Build a DateTime with String errors for application-style boundaries.

#### `fn days_in_month(y: Int, mo: Int) -> Int`

Days in a month, honoring leap February. A month outside 1..12 is a contract violation (RFC-0044 rule 3): abort naming the bad argument rather than silently returning 31 (the old `_ -> 31` catch-all).

#### `fn parse_iso8601(text: String) -> Result(DateTime, TimeError)`

Parse RFC 3339 / ISO 8601: `2026-06-08T22:30:00Z`, an offset like `+02:00` (normalized to UTC), fractional seconds (truncated), a space instead of the `T`, or a bare `YYYY-MM-DD` (midnight UTC). The typed error preserves malformed dates, time fields, fractions, and offsets as structured cases.

#### `fn parse_iso8601_string(text: String) -> Result(DateTime, String)`

Parse with String errors for application-style boundaries.

#### `fn is_leap(y: Int) -> Bool`

#### `fn weekday(d: DateTime) -> Int`

Day of week: 0 = Sunday … 6 = Saturday.

#### `fn weekday_name(d: DateTime) -> String`

#### `fn month_name(d: DateTime) -> String`

#### `fn date_string(d: DateTime) -> String`

`YYYY-MM-DD`.

#### `fn time_string(d: DateTime) -> String`

`HH:MM:SS`.

#### `fn iso8601(d: DateTime) -> String`

RFC 3339 / ISO 8601 in UTC, e.g. `2026-06-08T22:30:00Z`.

#### `fn format(d: DateTime, layout: String) -> String`

A strftime-style layout: `%Y-%m-%d %H:%M:%S`, `%A %B %d` and friends. Directives: %Y year, %m month, %d day, %H hour, %M minute, %S second, %a/%A weekday (short/full), %b/%B month name (short/full), %% a literal percent. Anything else after `%` passes through unchanged.

### Trait implementations

#### `impl Show for TimeError`

- `fn show(self) -> String`

#### `impl Error for TimeError`

#### `impl From(TimeError) for String`

- `fn from(value: TimeError) -> Self`

#### `impl Show for DateTime`

- `fn show(self) -> String`

## `toml`

toml — a TOML reader written in pure witchy (no native code). Two ways in: `toml.decode(text)` parses a whole document into a structured `Toml` tree (the `json.decode` shape); or look individual values up by a `section.key` path with `toml.get`/`get_array`/`table`/... It supports top-level and `[section]` (and dotted `[a.b]`) tables, `key = value` for string/int/bool values, and `["a", "b"]` arrays. Comments (`#`) — whole-line and trailing — and blank lines are ignored. (Floats/dates decode as `TomlString`: witchy has no string->float primitive yet.)

#### `type Toml`

A decoded TOML value (`toml.decode`), the structured counterpart of the string-query API below. A document decodes to a `TomlTable`. Floats, dates, and other values witchy can't type are kept as `TomlString` (witchy has no string->float primitive yet), so a round-trip never loses data.

- `TomlString(String)`
- `TomlInt(Int)`
- `TomlBool(Bool)`
- `TomlArray(List(Toml))`
- `TomlTable(List((String, Toml)))`

#### `type TomlDecodeError`

- `TomlDecodeError { message: String }`

#### `fn decode_error_message(e: TomlDecodeError) -> String`

#### `fn decode(text: String) -> Result(Toml, TomlDecodeError)`

Parse a whole TOML document into a `Toml` tree (always a `TomlTable`). Supports top-level keys, `[section]` and dotted `[a.b]` tables, `#` comments, and `string`/`int`/`bool`/array values. Genuinely fallible (RFC-0044 rule 2): a non-blank, non-comment line that is neither a `[section]` header nor a `key = value` pair is structurally malformed, so decoding returns `Err` naming the offending line.

#### `fn table_field(node: Toml, key: String) -> Option(Toml)`

The value stored at `key` in a `TomlTable`, or `None` when the key is absent or the node is not a table. Preserves the decoded declaration order (first match).

#### `fn as_string(node: Toml, context: String) -> Result(String, TomlDecodeError)`

The `String` a `Toml` leaf holds, or `Err(context)` when it is a different kind (`TomlInt`/`TomlBool`/`TomlArray`/`TomlTable`). `context` names the field so the caller's message points at the offending entry.

#### `fn as_array(node: Toml, context: String) -> Result(List(Toml), TomlDecodeError)`

The elements of a `TomlArray`, or `Err(context)` when the value is not an array.

#### `fn as_table(node: Toml, context: String) -> Result(List((String, Toml)), TomlDecodeError)`

The pairs of a `TomlTable`, or `Err(context)` when the value is not a table.

#### `fn array_of_tables(node: Toml, key: String) -> Result(List(Toml), TomlDecodeError)`

The `[[key]]` array-of-tables entries of a document node, in declaration order, with every repeated entry kept distinct. `Ok([])` when `key` is absent (a lockfile with no dependencies is well-formed); `Err` when `key` is present but not an array, or an element is not a table (a corrupted array-of-tables). This is how a `witchy.lock`'s `[[rune]]` entries are enumerated through the one structured parser.

#### `fn required_string(entry: Toml, field: String, label: String) -> Result(String, TomlDecodeError)`

A required string field of a decoded table `entry`: `Err` when the field is absent, empty, or the wrong TOML kind — the three failure classes kept distinct (BUG-373). `label` names the entry kind for the message (e.g. "a registry [[rune]] entry").

#### `fn optional_string(entry: Toml, field: String) -> Result(Option(String), TomlDecodeError)`

An optional string field of a decoded table `entry`: `Ok(None)` when absent, `Ok(Some(s))` when a string, `Err` when present but the wrong kind.

#### `fn string_array_field(entry: Toml, field: String) -> Result(List(String), TomlDecodeError)`

The string elements of an array-valued field of a decoded table `entry`: `Ok([])` when absent; `Err` when the field is not an array, or an element is not a string. Used to read a `[[rune]]` capability array (`runtime_footprint`).

#### `fn get(text: String, path: String) -> Option(String)`

The string value of `path` (e.g. "rune.name"), or None if absent. Surrounding double quotes are stripped.

#### `fn get_array(text: String, path: String) -> List(String)`

The string-array value of `path` (e.g. "capabilities.runtime"), or [] if absent. Each element is unquoted.

LENIENT BY DESIGN (RFC-0044): an absent path and a malformed/empty array both return `[]`, so this cannot tell "not declared" from "declared wrong" — convenient for reading config, but wrong for a trust boundary. Where a malformed array must be an ERROR, use the strict path: `decode(text)` (a `Result`) then match the `TomlArray` at the path.

#### `fn table(text: String, section: String) -> List((String, String))`

Every `key = value` pair defined directly under `[section]`, in file order. Keys are unquoted (`"acme/money"` -> `acme/money`); values are raw (trimmed, still quoted/inline) — feed an inline-table value to `inline_get`. Use this to enumerate a table whose keys you don't know ahead of time, like `[dependencies]`.

#### `fn keys(text: String, section: String) -> List(String)`

Just the keys of `[section]` (unquoted), in file order.

#### `fn inline_get(inline: String, key: String) -> Option(String)`

Read `key` from an inline table value like `{ path = "../money", version = "1" }`. Returns the unquoted value, or None if the key is absent.

#### `fn inline_get_array(inline: String, key: String) -> List(String)`

Read a string-array field from an inline table. This is the array counterpart to `inline_get`, used by nested policy values such as `{ programs = ["git"], child-paths = ["~/.gitconfig"] }`.

### Trait implementations

#### `impl Show for TomlDecodeError`

- `fn show(self) -> String`

#### `impl Error for TomlDecodeError`

#### `impl From(TomlDecodeError) for String`

- `fn from(value: TomlDecodeError) -> Self`

## `url`

Minimal URL parsing — the witchy slice of Go's net/url. Pure and capability-free, so it compiles to WASM. Handles `scheme://host[:port][/path][?query][#fragment]`; the port defaults by scheme (443 for https, else 80) and the path to "/".

`parse` returns a matchable `UrlError`; `parse_string` is the String-rendering bridge for application-style callers. Simple scalar parses like `string.parse_int` stay `Option`.

#### `sealed type Url`

- `Url(String, String, Int, String, Option(String), Option(String))`

#### `type UrlError`

Matchable URL parse failures. The payload is the original raw URL so callers can report or classify without parsing display text.

- `MissingSchemeSeparator(String)`
- `EmptyScheme(String)`
- `UserinfoUnsupported(String)`
- `EmptyHost(String)`
- `InvalidPort(String)`
- `MalformedIpv6Literal(String)`

#### `fn url_error_message(e: UrlError) -> String`

#### `fn parse(s: String) -> Result(Url, UrlError)`

Parse a URL, or a matchable error naming what is malformed. A well-formed URL needs a non-empty scheme and host — an empty either side (`://host`, `https:///path`) is rejected rather than accepted with a blank field. The scheme is case-insensitive (RFC 3986 §3.1) and is normalized to lowercase, so `HTTPS://` gets the https default port and formats back canonically.

#### `fn parse_string(s: String) -> Result(Url, String)`

Parse a URL with String errors for application-style boundaries.

#### `fn scheme(u: Url) -> String`

#### `fn host(u: Url) -> String`

#### `fn port(u: Url) -> Int`

#### `fn pathname(u: Url) -> String`

The path component only, normalized to `/` when the URL omits a path.

#### `fn query(u: Url) -> Option(String)`

The query text without its leading `?`, retaining `Some("")` for a URL that explicitly ends its query component at `?`.

#### `fn fragment(u: Url) -> Option(String)`

The fragment text without its leading `#`, retaining `Some("")` for a URL that explicitly ends at `#`.

#### `fn with_query(u: Url, key: String, value: String) -> Url`

Add one percent-encoded query pair while preserving the fragment boundary. Existing query pairs remain in order; an explicitly empty query does not gain a leading `&`.

#### `fn request_target(u: Url) -> String`

The HTTP request target: path plus query, deliberately excluding the client-side fragment component (RFC 3986 section 3.5).

#### `fn format(u: Url) -> String`

Render a Url back to its string form — the inverse of `parse`. The port is shown only when it differs from the scheme default, so a parse/format round trip of `https://host/p` stays `https://host/p` rather than gaining `:443`.

#### `fn encode(s: String) -> String`

Percent-encode `s` for use as a query-string value (RFC 3986): the unreserved set (`A-Z a-z 0-9 - _ . ~`) passes through, every other byte becomes `%XX`. Used to build query strings safely — e.g. an OAuth `redirect_uri`, `scope`, or `state`.

#### `fn decode(s: String) -> String`

Percent-decode `s` (RFC 3986 §2.1): each `%XX` becomes the byte it names. Consecutive `%XX` bytes are decoded together so multi-byte UTF-8 escapes (`%E2%82%AC` -> `€`) round-trip. A stray `%` not followed by two hex digits passes through literally (total, lossy). `+` stays literal — for form/query decoding where `+` means space, use `decode_form`.

#### `fn decode_form(s: String) -> String`

Like `decode`, but also maps `+` to a space — the `application/x-www-form-urlencoded` convention for query strings and form bodies (RFC 1866 §8.2.1).

### Trait implementations

#### `impl Show for UrlError`

- `fn show(self) -> String`

#### `impl Error for UrlError`

#### `impl From(UrlError) for String`

- `fn from(value: UrlError) -> Self`

#### `impl Show for Url`

- `fn show(self) -> String`

## `vm`

std/vm — (RFC-0032) parallel execution across cores.

`par_map` maps a function over a list with the elements processed in PARALLEL on the compiled backend: the work is split across OS-thread worker VMs, each its own isolated WebAssembly instance, and the results are gathered back in INPUT order. Because the result is ordered by input index and the mapped function is pure, the parallel result is identical to a sequential map — so the interpreter oracle (and this module's own reference body) computes it sequentially and the two backends agree. Parallelism changes how fast the map runs, not what it returns.

The compiled backend takes the parallel path only when `f` is a BARE TOP-LEVEL function and the element representation can cross the worker boundary. A local function value, lambda, or pointer-bearing element type instead runs this module's sequential reference body, with identical results. That fallback changes only performance: `par_map` promises an ordered map, not isolation.

#### `fn par_map(xs: List(a), f: fn(a) -> b) -> List(b)`

Map `f` over every element of `xs`, in parallel where the backend supports it, returning the results in input order.

#### `fn with_dir(dir: Dir, f: fn(Dir, Bytes) -> Bytes, input: Bytes) -> Bytes`

Run `f` on `input` in an ISOLATED worker VM (on the compiled backend) that is granted EXACTLY the directory capability `dir` — and nothing else. The worker can read/write within `dir` (with `dir`'s own rights) and reach NO other host resource: it is its own WebAssembly instance with its own memory, and every ungranted capability traps. This is the capability-passing sandbox — run untrusted/partially-trusted code with precisely scoped authority. `f` must be named directly as a bare top-level function; closures and local function values are rejected rather than silently running in the parent VM. Because the result is a deterministic function of `dir`'s contents and `input`, the isolation is invisible to the output, so the interpreter (which runs `f` directly) and the compiled backend agree.

#### `fn serve(init: Bytes, requests: List(Bytes), handler: fn(Bytes, Bytes) -> Bytes) -> List(Bytes)`

Run a stateful SERVICE on a single long-lived ISOLATED worker VM. The worker is created once and processes `requests` in order, threading an accumulator `state` through `handler(state, request) -> new_state`, emitting each new state as that request's response. This is witchy's cross-VM channel: a worker that processes a message stream with persistent state. It is deliberately LOCK-STEP (ordered, not racing) — that determinism is what lets the interpreter (a sequential scan) and the compiled backend (a persistent worker VM) agree, which a truly-racing channel could not. `handler` must be named directly as a bare top-level function; closures and local function values are rejected rather than silently replacing isolation with a parent-VM loop.

## `webauthn`

webauthn — server-side verification of a WebAuthn *assertion* (the credential "get" / second-factor ceremony), in pure witchy. ES256 (P-256, COSE alg -7) only.

The browser hands every BINARY value to the server HEX-ENCODED (it holds them as ArrayBuffers, so this is free). The server then INDEPENDENTLY re-derives and checks everything that matters — it trusts none of the client's interpretation:   * clientDataJSON.type == "webauthn.get"   * clientDataJSON.challenge == the exact challenge the server issued (anti-replay)   * clientDataJSON.origin == the expected origin (anti-phishing)   * authenticatorData.rpIdHash == SHA-256(expected RP id) (wrong relying party)   * the user-presence (and, for 2FA, user-verification) flags are set   * the ECDSA-P256 signature over `authenticatorData || SHA256(clientDataJSON)`     verifies under the public key bound to this credential at registration. A forged or replayed assertion fails one of these and is rejected here.

#### `type AssertionError`

Matchable assertion-verification failures. Malformed wire inputs are distinct from semantic rejection of a well-formed but replayed, phishing, or forged assertion.

- `ClientDataJson(String)`
- `WrongClientType`
- `ChallengeMismatch`
- `OriginMismatch`
- `AuthenticatorDataHex(String)`
- `AuthenticatorDataTooShort`
- `RpIdHashMismatch`
- `UserPresenceRequired`
- `UserVerificationRequired`
- `SignatureInputMalformed(String)`
- `SignatureInvalid`

#### `fn assertion_error_message(e: AssertionError) -> String`

Human-readable assertion failure text for logs and HTTP responses.

#### `fn verify_assertion(stored_pubkey_hex: String, auth_data_hex: String, client_data_json: String, signature_hex: String, expected_challenge: String, expected_origin: String, expected_rp_id: String, require_uv: Bool) -> Result(Bool, AssertionError)`

Verify an assertion. All `*_hex` arguments are hex-encoded bytes; `client_data_json` is the exact clientDataJSON text the browser signed over (it must be re-hashed verbatim, never re-serialized). `require_uv` demands user verification — pass `true` for a genuine second-factor gate. Returns `Ok(true)` when every check passes, or a typed `Err`.

### Trait implementations

#### `impl Show for AssertionError`

- `fn show(self) -> String`

#### `impl Error for AssertionError`

#### `impl From(AssertionError) for String`

- `fn from(value: AssertionError) -> Self`

