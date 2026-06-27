# API reference

## `ascii`

ASCII character predicates over single-character strings (such as those `string.char_at` returns). Pure and capability-free, like every std module. Classification is by code point in the ASCII range; the comparisons use the standard string ordering, so every function here is correct on both the interpreter and the compiled backend. The rough equivalent of Go's `unicode` helpers for the ASCII subset.

#### `fn is_digit(c: String) -> Bool`

ASCII character predicates over single-character strings (such as those `string.char_at` returns). Pure and capability-free, like every std module. Classification is by code point in the ASCII range; the comparisons use the standard string ordering, so every function here is correct on both the interpreter and the compiled backend. The rough equivalent of Go's `unicode` helpers for the ASCII subset.

#### `fn is_upper(c: String) -> Bool`

#### `fn is_lower(c: String) -> Bool`

#### `fn is_alpha(c: String) -> Bool`

#### `fn is_alnum(c: String) -> Bool`

#### `fn is_space(c: String) -> Bool`

#### `fn to_digit(c: String) -> Int`

The numeric value of a single decimal digit, or -1 when `c` is not a digit.

#### `fn all_digits(s: String) -> Bool`

True when `s` is non-empty and every character is an ASCII digit — a safe guard before `string_to_int`, which traps on non-numeric input.

## `chan`

std/chan — decoupled concurrency: `spawn` concurrent tasks, communicate over first-class `channel`s. Spawning and channels are independent — you can spawn without a channel, and a channel is a value you create and pass around, not a task's mailbox. Built on a pure-witchy cooperative executor with a deterministic round-robin schedule, so a concurrent run is byte-identical on the interpreter and the compiled WebAssembly — no scheduler state in the runtime, no `Pin`.

Model: one message type `m` per program (a single `run`'s channels all carry `m`; union into a sum type if you need several shapes). Spawned tasks return `Nil`; a task reports a result by sending it on a channel, not by returning it (a typed `JoinHandle(T)` would force a native runtime and break the parity contract). `send`/`recv` are always `await`ed because messaging is an effect on the executor-owned buffer; a *bounded* channel additionally blocks the sender when full (backpressure), an unbounded one never does.

The `async`/`await` CPS transform lowers onto the `std/task` executor (task.lazy/and_then/done/run); channel ops (`await chan.recv(rx)` / `await chan.send(tx, x)`) run on the same protocol.

#### `type Step`

- `Done(a)`
- `Yield(Task(m, a))`
- `Fork(Task(m, Nil), fn(Int) -> Task(m, a))`
- `Open(Int, fn(Int) -> Task(m, a))`
- `Push(Int, m, fn(Nil) -> Task(m, a))`
- `Pull(Int, fn(Option(m)) -> Task(m, a))`
- `PullAny(List(Int), fn(Option((Int, m))) -> Task(m, a))`
- `Wait(Int, fn(Nil) -> Task(m, a))`

#### `type Task`

- `Task(fn() -> Step(m, a))`

#### `type Sender`

- `Sender(Int)`

#### `type Receiver`

- `Receiver(Int)`

#### `type Handle`

- `Handle(Int)`

#### `type Selected`

- `First(m)`
- `Second(m)`
- `Closed`

#### `type Slot`

- `Active(Task(m, Nil))`
- `WaitRecv(Int, fn(Option(m)) -> Task(m, Nil))`
- `WaitSend(Int, m, fn(Nil) -> Task(m, Nil))`
- `WaitAny(List(Int), fn(Option((Int, m))) -> Task(m, Nil))`
- `WaitJoin(Int, fn(Nil) -> Task(m, Nil))`
- `Ended`

#### `fn done(x: a) -> Task(m, a)`

A finished task.

#### `fn ready_unit() -> Task(m, Nil)`

An already-complete `Task(m, Nil)` — the async/await lowering target for a body that falls off its end.

#### `fn yield_now() -> Task(m, Nil)`

Hand control back to the executor once, then continue.

#### `fn and_then(t: Task(m, a), k: fn(a) -> Task(m, b)) -> Task(m, b)`

Sequence: run `t`, then continue with `k` applied to its result. This is what `await` lowers to — the continuation `k` is the rest of the body.

#### `fn map(t: Task(m, a), f: fn(a) -> b) -> Task(m, b)`

Transform a task's result.

#### `fn lazy(thunk: fn() -> Task(m, a)) -> Task(m, a)`

Build the task `thunk()` lazily: nothing runs until the first poll. This is what makes an `async fn` LAZY — calling it yields a task that does no work until driven (by `run`, or by being `spawn`ed, or `await`ed).

#### `fn for_each(xs: List(a), f: fn(a) -> Task(m, Nil)) -> Task(m, Nil)`

Run `f(x)` as a task for each `x` in `xs`, in order — the lowering target for an `await` inside a `for x in xs:` loop.

#### `fn channel(capacity: Int) -> Task(m, (Sender(m), Receiver(m)))`

A bounded channel of `capacity` (the sender blocks when it is full); pass 0 for an unbounded channel that never blocks the sender.

#### `fn unbounded() -> Task(m, (Sender(m), Receiver(m)))`

An unbounded channel — `send` never blocks (the buffer grows without limit).

#### `fn send(tx: Sender(m), msg: m) -> Task(m, Nil)`

Send `msg`; on a bounded channel this blocks until there is room. Always awaited.

#### `fn recv(rx: Receiver(m)) -> Task(m, Option(m))`

Receive the next message, or `None` once the channel is closed — i.e. once no task can send to it anymore. `for await x in rx:` loops until this `None`.

#### `fn spawn(child: Task(m, Nil)) -> Task(m, Handle)`

Start `child` as a concurrent task; the returned handle completes when it does.

#### `fn join(h: Handle) -> Task(m, Nil)`

Block until the spawned task behind `h` finishes.

#### `fn select(a: Receiver(m), b: Receiver(m)) -> Task(m, Selected(m))`

Receive from whichever of `a` or `b` has a message first; a tie favours `a`. Yields `Closed` once both channels are closed.

#### `fn consume(rx: Receiver(m), f: fn(m) -> Task(m, Nil)) -> Task(m, Nil)`

Receive from `rx`, run `f` on each message, until the channel closes. The stateless server loop; `for await x in rx:` lowers to this.

#### `fn serve(rx: Receiver(m), state: s, handler: fn(s, m) -> Task(m, s)) -> Task(m, Nil)`

The stateful server loop: receive a message, run `handler` with the current `state` to get the next state, and repeat until the channel closes. State threads through every message with no hand-written recursion.

#### `fn run(root: Task(m, Nil))`

Drive `root` (and everything it spawns) to completion on a deterministic round-robin schedule. An async `main` lowers to a single `run` of its body.

## `cmp`

The witchy standard comparison hierarchy, mirroring Rust's `std::cmp`: `PartialEq` → `Eq` → `PartialOrd` → `Ord`. The comparison operators desugar through these traits, so `a == b` and `x < y` work on your own types once you implement (or derive) them — there is no separate `compare`/`greater` to call by name. Built-in impls cover the primitives; `Self` in a method signature stands for the implementing type. Pure and capability-free, like every std module.

#### `type Ordering`

The result of a comparison: `a` is `Less` than, `Equal` to, or `Greater` than `b`. The return of `Ord.compare`, and (wrapped in `Some`) `PartialOrd`.

- `Less`
- `Equal`
- `Greater`

#### `fn reverse(o: Ordering) -> Ordering`

Flip an ordering — `Less` <-> `Greater`, `Equal` unchanged — for reverse sorts.

#### `fn max_of(x: a, y: a) -> a where a: Ord`

#### `fn min_of(x: a, y: a) -> a where a: Ord`

#### `fn clamp(x: a, lo: a, hi: a) -> a where a: Ord`

`x` confined to the range [lo, hi].

#### `fn maximum(xs: List(a), default: a) -> a where a: Ord`

The largest element of `xs`, or `default` when `xs` is empty.

#### `fn minimum(xs: List(a), default: a) -> a where a: Ord`

The smallest element of `xs`, or `default` when `xs` is empty.

#### `fn sort(xs: List(a)) -> List(a) where a: Ord`

Sort any list of an `Ord` type ascending — a stable insertion sort that dispatches through the element type's `Ord` impl, so it is content-correct on both backends (Int, String, Duration, or your own `Ord` types) without a caller-supplied comparator. For Ints, `list.sort` is the lighter default.

#### `fn member(xs: List(a), x: a) -> Bool where a: Eq`

Whether `x` is in `xs`, by the element type's `Eq` impl — correct on both backends, unlike a generic `==`-based search reached through a type variable.

#### `fn index_of(xs: List(a), x: a) -> Int where a: Eq`

The index of the first element equal to `x`, or -1 if absent.

#### `fn count(xs: List(a), x: a) -> Int where a: Eq`

How many elements equal `x` (by the element type's `Eq`).

#### `fn unique(xs: List(a)) -> List(a) where a: Eq`

The list with duplicates removed, keeping the first occurrence of each element (by the element type's `Eq`), in original order. The content-correct counterpart to `list.unique`, which compares pointers in compiled code.

## `compiler`

compiler — witchy's own toolchain, exposed to witchy programs.

A native intrinsic module (implemented in Rust, like `crypto`): it gives a program access to the compiler's capability analyzer, so a (self-hosted) package manager can compute a rune's supply-chain footprint from within witchy — on either backend. The body below is a placeholder the runtime never executes (the call is intercepted by its qualified name). The capability footprint of witchy `source`, as JSON:   {"total":[..],"build":[..],"entries":[{"name":..,"capabilities":[..],"brands":[..]}]} or {"error":".."} if the source does not parse. `build` is the build-time footprint — the build capabilities the rune's `build` entrypoint demands (gated separately from the runtime `total`). Parse it with `import json`.

#### `fn footprint(source: String) -> String`

compiler — witchy's own toolchain, exposed to witchy programs.

A native intrinsic module (implemented in Rust, like `crypto`): it gives a program access to the compiler's capability analyzer, so a (self-hosted) package manager can compute a rune's supply-chain footprint from within witchy — on either backend. The body below is a placeholder the runtime never executes (the call is intercepted by its qualified name). The capability footprint of witchy `source`, as JSON:   {"total":[..],"build":[..],"entries":[{"name":..,"capabilities":[..],"brands":[..]}]} or {"error":".."} if the source does not parse. `build` is the build-time footprint — the build capabilities the rune's `build` entrypoint demands (gated separately from the runtime `total`). Parse it with `import json`.

#### `fn diff(old: String, new: String) -> String`

Compare two sources by capability footprint, as JSON:   {"widened":bool,"added":[..],"removed":[..]}   (or {"error":".."}) `widened` is the rights-precise block-on-widening gate: true when `new` demands any capability or right that `old` did not.

#### `fn doc(name: String, source: String) -> String`

Render `source` to Markdown API documentation (the same output as `witchy doc`): the module's public types and functions with their signatures and doc-comments, under a heading titled `name`. This only PARSES the source — it never runs it — so a registry can safely generate browsable docs from a rune's stored source on either backend. A parse error comes back as an HTML comment, never a trap.

## `confine`

confine — typed `Net` address policies (RFC-0011). A `NetPolicy` is a typed value built by the constructors below instead of a hand-written string, so a policy is constructed, not spelled; `net.only(policy)` narrows a `Net` to it and `net.deny(policy)` subtracts it:

    let db  = net.only(confine.tcp("10.0.0.5", 6379))     // one plaintext host     let lan = net.deny(confine.cidr_any("10.0.0.0/8"))    // hold everything EXCEPT this block

These are pure value builders (empty capability footprint). The policy wraps the same `host:port` allowlist pattern the host enforces (RFC-0003); the `tls:` HTTPS scheme is a connect-time choice on the address, not a property of the policy (RFC-0009).

#### `type NetPolicy`

A typed `Net` address policy — one allowlist pattern (`host:port`, with `:*` / CIDR forms).

- `NetPolicy { pattern: String }`

#### `fn tcp(host: String, port: Int) -> NetPolicy`

A plaintext TCP endpoint: `<host>:<port>`.

#### `fn any_port(host: String) -> NetPolicy`

Any port on a host: `<host>:*`.

#### `fn cidr(block: String, port: Int) -> NetPolicy`

An IPv4 CIDR block (rebinding-proof — matched against the resolved IP): `<a.b.c.d/bits>:<port>`.

#### `fn cidr_any(block: String) -> NetPolicy`

An IPv4 CIDR block, any port: `<a.b.c.d/bits>:*`.

#### `fn union(a: NetPolicy, b: NetPolicy) -> NetPolicy`

The UNION of two policies — `net.only(union(a, b))` admits either (e.g. an OAuth host plus an API host: `union(tcp("github.com", 443), tcp("api.github.com", 443))`). The patterns are carried together (newline-joined internally) and the host narrows to the whole set at once.

## `convert`

Conversion traits, following Rust's `std::convert`. `From(a)` builds the implementing type from an `a`; `Into(b)` consumes `self` into a `b`. Implementing `From` is enough, since the blanket impl below derives the matching `Into`:

  Celsius.from(deg)   build via From   value.into()        convert via the derived Into

`from` takes no `self`, so the blanket impl calls it on the target type as `b.from(self)`. The `where` bound decides which `from` to call when the use site is monomorphized.

_No public API._

## `crypto`

crypto — cryptographic hashing and signatures.

Like Go's `crypto/*` packages, these are *native intrinsics*: SHA-256 and Ed25519 cannot be expressed in witchy itself (no byte access; elliptic-curve field arithmetic), so they are implemented in Rust. They are reachable only through this module — there is no global builtin. The function bodies below are placeholders the runtime never executes: the interpreter intercepts each call by its qualified name (`crypto.sha256`), and the WASM backend bridges it to the same implementation as a host import. SHA-256 of a string's UTF-8 bytes, as 64 lowercase hex characters.

#### `fn sha256(data: String) -> String`

crypto — cryptographic hashing and signatures.

Like Go's `crypto/*` packages, these are *native intrinsics*: SHA-256 and Ed25519 cannot be expressed in witchy itself (no byte access; elliptic-curve field arithmetic), so they are implemented in Rust. They are reachable only through this module — there is no global builtin. The function bodies below are placeholders the runtime never executes: the interpreter intercepts each call by its qualified name (`crypto.sha256`), and the WASM backend bridges it to the same implementation as a host import. SHA-256 of a string's UTF-8 bytes, as 64 lowercase hex characters.

#### `fn rune_hash(paths: List(String), contents: List(String)) -> String`

The canonical content hash of a rune's source tree, as `sha256:<hex>`. Pass parallel lists — one entry per file (`witchy.toml` plus each `src/**/*.witchy`): `paths[i]` is the relative path, `contents[i]` its text. Entries are sorted and length-prefixed before hashing, so the result is the rune's stable content address — the package manager's tamper-evident identity.

#### `fn ed25519_verify(public_key: String, message: String, signature: String) -> Bool`

Verify an Ed25519 signature. `public_key` and `signature` are hex-encoded; `message` is the raw string. Total: malformed input or a bad signature yields `false`, never an error.

#### `fn sign(key: Secret, message: String) -> String`

Sign `message` with a `Secret` capability (the host grants it; it cannot be forged), returning the hex signature.

#### `fn public_key(key: Secret) -> String`

The hex Ed25519 public key for a `Secret` — what verifiers check against.

#### `fn reveal(key: Secret) -> String`

Reveal a `Secret`'s raw bytes as a string — for value secrets (tokens, passwords) that must be handed to an external sink. Signing keys are used via `sign`/`public_key`, not revealed.

#### `fn ecdsa_p256_verify(public_key: String, message: String, signature: String) -> Bool`

Verify an ECDSA P-256 / SHA-256 signature — WebAuthn "ES256" (COSE alg -7). `public_key` is the hex SEC1 uncompressed point (`04 || x || y`); `signature` is the hex ASN.1-DER signature; `message` is the raw bytes it covers. Total: bad input or a bad signature yields `false`. (Native/interpreter-only.)

#### `fn ecdsa_p256_verify_hex(public_key: String, message: String, signature: String) -> Bool`

Like `ecdsa_p256_verify` but the message is also hex — for binary messages such as WebAuthn's `authenticatorData || SHA256(clientDataJSON)`. (Native-only.)

#### `fn rsa_pkcs1_sha256_verify(public_key: String, message: String, signature: String) -> Bool`

Verify an RSASSA-PKCS1-v1_5 / SHA-256 signature — JWT/OIDC "RS256" (the algorithm GitHub Actions and Google sign their identity tokens with). `public_key` is the hex of a DER-encoded RSA public key (PKCS#1 `RSAPublicKey`); `signature` is hex; `message` is the raw signed bytes (`header.payload` for a JWT). Total: bad input or a bad signature yields `false`. (Native-only.)

#### `fn sha512(data: String) -> String`

SHA-512 of a string's UTF-8 bytes, as 128 lowercase hex characters. (Native-only.)

#### `fn sha3_256(data: String) -> String`

SHA3-256 (FIPS 202) of a string's UTF-8 bytes, as 64 hex characters. (Native-only.)

#### `fn hmac_sha256(key: String, message: String) -> String`

HMAC-SHA256 (FIPS 198-1). `key` is hex (so binary keys are representable); `message` is raw text. Returns the 64-hex-char tag. (Native-only.)

## `csv`

csv — comma-separated values, parse and encode (RFC 4180-ish).

Fields are separated by commas and rows by newlines. A field that contains a comma, a quote, or a newline is wrapped in double quotes, and a literal quote inside such a field is doubled (`""`). The parser is a small character state machine, so embedded commas/newlines/quotes round-trip through `encode`.

#### `fn parse(text: String) -> List(List(String))`

Parse CSV text into rows of fields. A trailing newline is ignored; `\r\n` and `\n` line endings both work.

#### `fn encode(rows: List(List(String))) -> String`

Encode rows back to CSV text (each row newline-terminated), quoting any field that needs it.

#### `fn parse_records(text: String) -> List(Dict(String, String))`

Parse with the first row as a header: each remaining row becomes a Dict keyed by the header columns (a short row's missing columns read as "").

## `dict`

dict — the associative map.

The core operations are native primitives (intercepted by both backends; the bodies are self-recursive placeholders giving the type checker their signatures): `dict.new`, `dict.insert`, `dict.get_or`, `dict.update`, `dict.has`, `dict.remove`, `dict.keys`, `dict.values`, `dict.pairs`, `dict.size`. The rest is the compositional layer — a lookup returning `Option`, constructors from pairs, and the map/filter/merge transforms.

#### `fn new() -> Dict(k, v)`

An empty Dict.

#### `fn insert(d: Dict(k, v), key: k, val: v) -> Dict(k, v)`

A new dict with `key` set to `val` (replacing any existing entry). Insertion order of first appearance is preserved.

#### `fn get_or(d: Dict(k, v), key: k, default: v) -> v`

The value for `key`, or `default` when absent.

#### `fn update(d: Dict(k, v), key: k, default: v, f: fn(v) -> v) -> Dict(k, v)`

Single-lookup upsert: apply `f` to the current value (or `default` when `key` is absent) and store the result under `key`.

#### `fn has(d: Dict(k, v), key: k) -> Bool`

Whether `key` is present.

#### `fn remove(d: Dict(k, v), key: k) -> Dict(k, v)`

A new dict with `key` (and its value) removed; unchanged when absent.

#### `fn keys(d: Dict(k, v)) -> List(k)`

The keys, in insertion order.

#### `fn values(d: Dict(k, v)) -> List(v)`

The values, in insertion order.

#### `fn pairs(d: Dict(k, v)) -> List((k, v))`

The (key, value) pairs, in insertion order.

#### `fn size(d: Dict(k, v)) -> Int`

The number of entries.

#### `fn get(d: Dict(k, v), key: k) -> Option(v)`

A lookup that says whether the key was present, rather than forcing a default.

#### `fn is_empty(d: Dict(k, v)) -> Bool`

#### `fn from_pairs(entries: List((k, v))) -> Dict(k, v)`

Build a Dict from (key, value) pairs; a later pair overrides an earlier one.

#### `fn map_values(d: Dict(k, v), f: fn(v) -> w) -> Dict(k, w)`

A new Dict with every value passed through `f` (keys unchanged).

#### `fn filter(d: Dict(k, v), keep: fn(k, v) -> Bool) -> Dict(k, v)`

Keep only the entries for which `keep(key, value)` holds.

#### `fn merge(a: Dict(k, v), b: Dict(k, v)) -> Dict(k, v)`

`a` with `b`'s entries laid over it (on a key collision, `b` wins).

#### `fn invert(d: Dict(k, v)) -> Dict(v, k)`

Swap keys and values. With duplicate values, a later entry wins.

#### `fn values_where(d: Dict(k, v), pred: fn(k) -> Bool) -> List(v)`

The values whose keys satisfy `pred`, in the Dict's iteration order.

## `duration`

Pure helpers for the built-in `Duration` type — a length of time, written as a literal like `30s`, `2hr`, or `500ms`. Durations are combined and compared with the language operators (`a + b`, `d * 3`, `a < b`); this module adds construction from plain numbers, component access, and human formatting. Capability-free, so it compiles to WASM. A Duration is carried as whole milliseconds (`int_to_duration`/`duration_to_int` are the Int<->Duration bridge).

#### `fn milliseconds(n: Int) -> Duration`

---- Construction from a count of one unit ----

#### `fn seconds(n: Int) -> Duration`

#### `fn minutes(n: Int) -> Duration`

#### `fn hours(n: Int) -> Duration`

#### `fn days(n: Int) -> Duration`

#### `fn weeks(n: Int) -> Duration`

#### `fn from_clock(h: Int, m: Int, s: Int) -> Duration`

Build a duration from hours, minutes, and seconds.

#### `fn to_milliseconds(d: Duration) -> Int`

---- Total conversions (whole units, truncated toward zero) ----

#### `fn to_seconds(d: Duration) -> Int`

#### `fn max(a: Duration, b: Duration) -> Duration`

The longer of two durations.

#### `fn min(a: Duration, b: Duration) -> Duration`

The shorter of two durations.

#### `fn is_zero(d: Duration) -> Bool`

Whether the duration is exactly zero.

#### `fn abs(d: Duration) -> Duration`

The magnitude of a duration (a negative span, e.g. from subtraction, is made positive).

#### `fn part_hours(d: Duration) -> Int`

The hours component (everything an hour and above).

#### `fn part_minutes(d: Duration) -> Int`

The minutes component (0..59).

#### `fn part_seconds(d: Duration) -> Int`

The seconds component (0..59).

#### `fn part_milliseconds(d: Duration) -> Int`

The milliseconds component (0..999).

#### `fn clock(d: Duration) -> String`

A clock string "H:MM:SS": minutes and seconds zero-padded, hours in full (so a long span reads as e.g. "100:00:00"); the sub-second part is dropped.

#### `fn human(d: Duration) -> String`

A compact label that omits leading zero units: `1h1m1s`, `1m30s`, `5s`, and `500ms` for a pure sub-second span.

#### `fn parse(s: String) -> Option(Duration)`

Parse a duration string to a `Duration` — the inverse of `human`. Accepts unit-tagged input ("1h2m3s", "500ms", "2hr", any subset) using ms/s/m/h/hr/d/w, and a bare number as plain milliseconds. None on a stray character or a dangling unit-less number after units were given ("1h30").

## `encoding`

encoding — hex and base64 over a string's UTF-8 bytes.

Like `crypto`, these are native intrinsics: they need byte-level access that witchy strings don't expose, so they are implemented in Rust — the interpreter runs them directly, and the WASM backend bridges each to the same implementation as a host import. The function bodies below are placeholders the runtime never executes — each call is intercepted by its qualified name (`encoding.hex_encode`, …). Decoding is lenient (lossy UTF-8 for non-text payloads), never an error.

#### `fn hex_encode(data: String) -> String`

Lowercase hex of `data`'s UTF-8 bytes.

#### `fn hex_decode(data: String) -> String`

Decode a hex string back to text (lossy UTF-8); whitespace skipped.

#### `fn base64_encode(data: String) -> String`

Standard base64 (with `=` padding) of `data`'s UTF-8 bytes.

#### `fn base64_decode(data: String) -> String`

Decode standard base64 back to text (lossy UTF-8); padding/whitespace tolerated.

#### `fn base64url_of_hex(hex: String) -> String`

base64url (no padding; `-`/`_`) of the bytes given as a HEX string. The hex indirection lets binary round-trip through UTF-8 strings — e.g. a WebAuthn `clientDataJSON.challenge` is base64url of the raw challenge bytes.

#### `fn base64url_decode(data: String) -> String`

Decode base64url (URL-safe `-`/`_`, no padding) back to text (lossy UTF-8) — for the JSON header/payload segments of a JWT/OIDC identity token.

#### `fn base64url_to_hex(data: String) -> String`

Decode base64url to a HEX string — for binary that must round-trip through a witchy String, e.g. a JWT's RS256 signature fed to `crypto.rsa_pkcs1_sha256_verify`.

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

#### `fn parent_dir(path: String) -> String`

The parent component of a relative path: "a/b/c" -> "a/b"; "a" -> "".

#### `fn collect_files(root: Dir, path: String, rel: String, ext: String) -> List((String, String))`

Recursively collect every file under `path` whose name ends with `ext`, as (relpath, contents) pairs. `rel` is prefixed to each result path (so a nested `a/b.witchy` keeps its full relative path). Needs a readable `Dir`.

## `func`

The witchy standard function-combinator library. Pure and capability-free. With first-class functions these build new functions from existing ones without writing wrapper lambdas by hand.

#### `fn identity(x: a) -> a`

Return the argument unchanged.

#### `fn compose(f: fn(b) -> c, g: fn(a) -> b) -> fn(a) -> c`

`compose(f, g)` is the function `x -> f(g(x))` — apply `g`, then `f`.

#### `fn flip(f: fn(a, b) -> c) -> fn(b, a) -> c`

`flip(f)` applies the two-argument `f` with its arguments swapped.

#### `fn on_key(op: fn(b, b) -> c, key: fn(a) -> b) -> fn(a, a) -> c`

`on_key(op, key)` is the function `(x, y) -> op(key(x), key(y))` — run a two-argument `op` on the projections of two values. Pairs with the comparator-taking list functions: `list.sort_by(people, func.on_key(less, age))` sorts by age. (Named `on_key` because bare `on` is a keyword.)

#### `fn constant(x: a) -> fn(b) -> a`

A function that ignores its argument and always returns `x`.

#### `fn first(p: (a, b)) -> a`

The first / second component of a pair — handy for the tuples `iter.zip` and `iter.enumerate` produce, without writing a `match` each time.

#### `fn second(p: (a, b)) -> b`

## `future`

std/future — cooperative single-threaded futures via CPS over closures.

A `Future(a)` is a thunk that, when polled, either completes (`Done`) or hands back the rest of the work (`More`). This is the substrate the `async`/`await` surface lowers onto: an `await` becomes an `and_then`, and the awaited value's live state is the captured values of the continuation closure. Those captures are owned values, never internal references — so, unlike Rust, there is nothing self-referential and no `Pin`. Pure structure (closures + sum types), so it runs byte-identically on both backends.

`More` is the cooperative yield point: an executor drives a future by polling it one step at a time, so several futures can interleave at their await points.

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

Sequence: run `f` to completion, then continue with `k` applied to its result. This is what `let y = await f` lowers to (`k` is the continuation).

#### `fn map(f: Future(a), g: fn(a) -> b) -> Future(b)`

Transform the result of a future.

#### `fn defer(thunk: fn() -> a) -> Future(a)`

Run `thunk` at POLL time and complete with its result. Unlike `ready`, which captures an already-computed value, `defer` delays the work (and any effects in it) until the executor polls — so effects from concurrent tasks interleave in scheduling order instead of all firing when the task is built.

#### `fn lazy(thunk: fn() -> Future(a)) -> Future(a)`

Build the future `thunk()` lazily: nothing runs until the first poll, and then exactly once (the executor advances to that future's own continuation, never back through `lazy`). This is what makes an `async fn` LAZY — calling it yields a future that does no work, not even its pre-`await` statements, until driven.

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

HTTP types and a small HTTP/1.1 *client* over the `Net` capability — the witchy answer to a slice of Go's net/http (and reqwest's shape). Pure transport built on the capability-gated socket primitives: a module handed a `Net` restricted to some hosts can reach only those, and one handed no `Net` can't reach the network at all. The `server` module builds on the shared `Request`/`Response` types here. Runs on both backends: the socket primitives compile to capability-gated host imports, so a compiled module's import list IS its network footprint.

#### `type Response`

A response: status code, headers (name lowercased for case-insensitive lookup), and body. Shared by the client and the `server` framework.

- `Response(Int, List((String, String)), String)`

#### `type Request`

A parsed request the server hands to a handler: method, raw path, the path params the router captured (`:id`), the parsed query string, the (lowercased) headers, and the body.

- `Request(String, String, List((String, String)), List((String, String)), List((String, String)), String)`

#### `type RequestBuilder`

An outgoing request being assembled: method, full URL, headers, and body. Build it up with the chainable methods and finish with `.send(net)`.

- `RequestBuilder(String, String, List((String, String)), String)`

#### `fn get(net: Net[Connect, Tcp], host: String, port: Int, path: String) -> Response`

Perform a GET request to `host:port` for `path`, returning the response.

#### `fn post(net: Net[Connect, Tcp], host: String, port: Int, path: String, body: String) -> Response`

Perform a POST request with `body` (e.g. a JSON document). The body's byte length is sent as Content-Length.

#### `fn put(net: Net[Connect, Tcp], host: String, port: Int, path: String, body: String) -> Response`

#### `fn delete(net: Net[Connect, Tcp], host: String, port: Int, path: String) -> Response`

#### `fn patch(net: Net[Connect, Tcp], host: String, port: Int, path: String, body: String) -> Response`

#### `fn head(net: Net[Connect, Tcp], host: String, port: Int, path: String) -> Response`

#### `fn get_url(net: Net[Connect, Tcp], raw: String) -> Result(Response, String)`

GET a full URL string (`http://host[:port]/path`), or an error if it doesn't parse. Saves splitting host/port/path by hand.

#### `fn request_with(net: Net[Connect, Tcp], secure: Bool, method: String, host: String, port: Int, path: String, headers: List((String, String)), body: String) -> Response`

Send a request with custom headers, returning the response. The generic form behind the method helpers — use it when you need to set headers (auth, content-type, ...). `secure` selects HTTPS (TLS). `Connection: close` ends `recv_all` after the body.

#### `fn try_request_with(net: Net[Connect, Tcp], secure: Bool, method: String, host: String, port: Int, path: String, headers: List((String, String)), body: String) -> Result(Response, String)`

Like `request_with`, but fallible: an unreachable upstream yields `Err("connect to host:port failed (unreachable)")` instead of trapping. Built on `try_connect` so a long-running server (a proxy, a health check) survives a down peer — the caller decides what to do (e.g. answer 502) instead of the VM aborting. A successful dial sends the request and parses the response as usual.

#### `fn try_get(net: Net[Connect, Tcp], host: String, port: Int, path: String) -> Result(Response, String)`

Fallible GET: `Ok(response)` on success, `Err(reason)` if the host is unreachable. The fallible counterpart of `get`.

#### `fn try_post(net: Net[Connect, Tcp], host: String, port: Int, path: String, body: String) -> Result(Response, String)`

Fallible POST: `Ok(response)` on success, `Err(reason)` if the host is unreachable. The fallible counterpart of `post`.

#### `fn build(method: String, url: String) -> RequestBuilder`

Start a request to `url` with `method` (e.g. "GET").

#### `fn get_request(url: String) -> RequestBuilder`

#### `fn post_request(url: String) -> RequestBuilder`

#### `fn put_request(url: String) -> RequestBuilder`

#### `fn delete_request(url: String) -> RequestBuilder`

#### `fn patch_request(url: String) -> RequestBuilder`

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

## `iter`

std/iter — lazy, pull-based iterators: the witchy take on Rust's Iterator, minus the part Rust most regrets. Because witchy values are "data" (no borrowing), there is no lending-iterator / GAT complexity: an `Iter(a)` is just a thunk that produces the next `Step`. Adapters (`map`/`filter`/ `take_while`/...) are lazy and compose without building intermediate lists; consumers (`collect`/`fold`/`find`/`count`) drive the pulling. Infinite iterators are fine (`count_from`, `repeat`) as long as something bounds them (`take`/`take_while`/`find`). Pure and capability-free; runs on both backends. (The planned `gen`/`yield` syntax will de-sugar to these constructors.)

#### `type Step`

One pull: either exhausted, or a value plus the rest of the iterator.

- `Empty`
- `Item(a, Iter(a))`

#### `type Iter`

An iterator is a thunk producing its next Step. Pull it with `next`.

- `Iter(fn() -> Step(a))`

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

#### `fn map(it: Iter(a), f: fn(a) -> b) -> Iter(b)`

Apply `f` to every element.

#### `fn filter(it: Iter(a), keep: fn(a) -> Bool) -> Iter(a)`

Keep only the elements for which `keep` holds.

#### `fn filter_map(it: Iter(a), f: fn(a) -> Option(b)) -> Iter(b)`

Apply `f` to each element, keeping every `Some(y)` and dropping every `None` — a `map` and `filter` fused into one pass (Rust's `Iterator::filter_map`).

#### `fn take(it: Iter(a), k: Int) -> Iter(a)`

The first `k` elements (fewer if the iterator is shorter).

#### `fn take_while(it: Iter(a), pred: fn(a) -> Bool) -> Iter(a)`

Elements up to (not including) the first one failing `pred`.

#### `fn drop_while(it: Iter(a), pred: fn(a) -> Bool) -> Iter(a)`

Skip the leading elements while `pred` holds, then yield the rest.

#### `fn drop(it: Iter(a), k: Int) -> Iter(a)`

Skip the first `k` elements.

#### `fn enumerate(it: Iter(a)) -> Iter((Int, a))`

Pair each element with its index: (0, x0), (1, x1), ...

#### `fn zip(a: Iter(x), b: Iter(y)) -> Iter((x, y))`

Zip two iterators into pairs, stopping at the shorter one.

#### `fn chain(first: Iter(a), second: Iter(a)) -> Iter(a)`

The elements of `first`, then the elements of `second`.

#### `fn flat_map(it: Iter(a), f: fn(a) -> Iter(b)) -> Iter(b)`

Map each element to an iterator and concatenate the results.

#### `fn for_each(it: Iter(a), f: fn(a) -> Nil)`

Call `f` on every element for its effect (drives to exhaustion). The right consumer for a generator when you don't need to early-exit — no list is built.

#### `fn collect(it: Iter(a)) -> c where c: FromIterator(a)`

Collect into any FromIterator type, chosen by the call site's expected type (drives the iterator to exhaustion — don't call on an unbounded one):     let xs: List(Int) = iter.collect(it)     let joined: String = iter.collect(pieces)     let s: Set(Int) = iter.collect(it)        # de-duplicates; needs `a: Eq`

#### `fn fold(it: Iter(a), init: b, f: fn(b, a) -> b) -> b`

Left fold over the elements.

#### `fn count(it: Iter(a)) -> Int`

Number of elements (drives to exhaustion).

#### `fn sum(it: Iter(Int)) -> Int`

Sum of an Int iterator.

#### `fn split_first(it: Iter(a)) -> Option((a, Iter(a)))`

Split an iterator into its first element and the rest, or None if it is empty. The building block for writing your own recursive iterator transforms (e.g. a prime sieve): pair it with `unfold`, which threads the "rest" as its seed.

#### `fn find(it: Iter(a), pred: fn(a) -> Bool) -> Option(a)`

The first element satisfying `pred`, or None (stops at the first match, so it is safe on an unbounded iterator if a match exists).

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

#### `fn encode(j: Json) -> String`

Serialize a Json value to its compact textual form.

#### `fn encode_pretty(j: Json) -> String`

Serialize with 2-space indentation, for human-readable output. Empty arrays and objects stay on one line (`[]` / `{}`).

#### `fn decode(s: String) -> Result(Json, String)`

Parse a complete JSON document, or return an error message. The whole input must be a single value: trailing content after it (other than whitespace) is rejected, so `decode("1 2")` is an Err rather than silently yielding `1`.

#### `fn get(j: Json, key: String) -> Option(Json)`

Look up a key in a JSON object.

#### `fn has_key(j: Json, key: String) -> Bool`

Whether a JSON object has `key` (false for non-objects).

#### `fn merge(a: Json, b: Json) -> Json`

A shallow merge of two JSON objects: every key of `b` overrides the same key in `a`, and `a`'s other keys are kept. If either value is not an object, `b` wins (so it works as "patch `a` with `b`"). Override is top-level only — nested objects are replaced, not deep-merged.

#### `fn index(j: Json, i: Int) -> Option(Json)`

The element at index `i` of a JSON array.

#### `fn as_int(j: Json) -> Option(Int)`

`j` as an integer, when it is one.

#### `fn as_string(j: Json) -> Option(String)`

`j` as a string, when it is one.

#### `fn as_bool(j: Json) -> Option(Bool)`

`j` as a bool, when it is one.

#### `fn as_array(j: Json) -> Option(List(Json))`

`j` as a list of elements, when it is an array.

#### `fn as_object(j: Json) -> Option(List((String, Json)))`

`j` as its key/value pairs, when it is an object — for iterating an object whose keys aren't known ahead of time.

#### `fn require(j: Json, key: String) -> Result(Json, String)`

--- Result-returning decoders (the backbone of `derive(Deserialize)`'s from_json) -----

#### `fn int_of(j: Json) -> Result(Int, String)`

Coerce a Json value to a scalar, or `Err` describing the expected shape.

#### `fn string_of(j: Json) -> Result(String, String)`

#### `fn bool_of(j: Json) -> Result(Bool, String)`

#### `fn float_of(j: Json) -> Result(Float, String)`

#### `fn array_of(j: Json) -> Result(List(Json), String)`

#### `fn optional(o: Option(Json), each: fn(Json) -> Result(a, String)) -> Result(Option(a), String)`

Decode an optional field: an absent key or an explicit `null` is `None`; otherwise the value is decoded via `each`. Used for `Option(_)` fields.

#### `fn object_sorted(pairs: List((String, Json))) -> Json`

Build a JSON object whose keys are sorted (matching a serialized BTreeMap), e.g. TUF `targets`. Use this only for dynamic key/value sets whose order must be deterministic for signing; records that derive(Json) keep their declared field order instead.

#### `fn get_string(j: Json, key: String) -> Option(String)`

--- typed field accessors (get a key, then coerce) -------------------------- `get` composed with each `as_*` — the common case of reading a typed field out of an object without spelling the two steps every time.

#### `fn get_int(j: Json, key: String) -> Option(Int)`

#### `fn get_bool(j: Json, key: String) -> Option(Bool)`

#### `fn get_strings(j: Json, key: String) -> List(String)`

The string array at `key` as a `List(String)`, dropping any non-string element; `[]` when the key is absent or not an array. Collapses the very common "decode an object's array-of-strings field" pattern into one call.

#### `fn strings(j: Json) -> List(String)`

A JSON array value as a `List(String)` (non-strings dropped).

#### `fn index_string(j: Json, i: Int) -> Option(String)`

The string at index `i` of a JSON array, when it is a string.

#### `fn get_path(j: Json, path: String) -> Option(Json)`

Follow a dotted path of object keys, e.g. `get_path(resp, "user.name")`. Any missing key (or a non-object along the way) yields `None`.

#### `fn from_option(o: Option(a), each: fn(a) -> Json) -> Json`

Encode an `Option` as payload-or-`null` — `Some(x)` through `each`, `None` as `JsonNull`. Keeps a derived `to_json`'s Option field a single-line call. (The param is `each`, not `encode`, so it doesn't shadow `json.encode`.)

#### `fn value_of(x: a) -> Json where a: Reflect`

--- reflective encoding (no derive) ----------------------------------------- `value_of(x)` encodes a value to `Json` by reflecting over its structure, so it works for any type with no derive. `stringify(x)` returns the encoded string.

#### `fn stringify(x: a) -> String where a: Reflect`

## `jwt`

jwt — verify a compact JWS / JWT (the OIDC identity-token shape), in PURE witchy over `crypto` (RS256/ES256), `encoding` (base64url), and `json`. Verification is computation, so this module has no host capability of its own — fetching the signing keys (JWKS discovery, over HTTPS) is a separate, network-bearing concern.

A compact JWT is `header.payload.signature`, each base64url. The signature covers the ASCII bytes of `header.payload`; for RS256 it is verified against the issuer's RSA public key (DER PKCS#1, hex). On success the decoded payload `claims` are returned for the caller to inspect (`sub`, `iss`, provider-specific owner fields).

#### `fn verify_rs256(token: String, rsa_pubkey_der_hex: String, audience: String, now: Int) -> Result(Json, String)`

Verify an RS256 (RSASSA-PKCS1-v1_5 / SHA-256) compact JWT against a DER PKCS#1 RSA public key (hex), checking the signature, `exp > now` (unix seconds), and `aud == audience`. Returns the decoded claims, or a reason string.

#### `fn verify_oidc(token: String, rsa_pubkey_der_hex: String, issuer: String, audience: String, now: Int) -> Result(Json, String)`

The full OIDC relying-party check: verify the RS256 signature AND that the token was minted by the expected `issuer` for the expected `audience`, and is valid now (`exp`/`nbf`). Returns the identity claims — `sub` plus provider-specific fields like GitHub's `repository` or Google's `email` — for the caller to authorize. This is the call a login or trusted-publishing flow makes once it holds the issuer's JWKS key (`rsa_key_from_jwk`). The issuer check is what binds a token to a TRUSTED provider: without it, anyone who can mint a JWT for the right audience would be admitted.

#### `fn header(token: String) -> Result(Json, String)`

The decoded JOSE header of a compact JWT (its first segment), or an error — so a verifier can read `alg`/`kid` to select the JWKS key before checking the signature.

#### `fn claims_unverified(token: String) -> Result(Json, String)`

The payload claims of a compact JWT WITHOUT verifying its signature — for reading the routing fields (`iss`, and `kid` via `header`) needed to SELECT the verification key before `verify_oidc`. DANGER: never authorize on these claims; verify the signature first and read the claims `verify_oidc` returns.

#### `fn rsa_key_from_jwk(n: String, e: String) -> String`

Build the DER PKCS#1 `RSAPublicKey` (as hex — the shape `verify_rs256` wants) from a JWK's base64url modulus `n` and exponent `e`, so an OIDC verifier can turn a JWKS entry (`{"kty":"RSA","n":…,"e":…}`) into a key. The result is the ASN.1 DER `SEQUENCE { INTEGER n, INTEGER e }`; an INTEGER gains a leading `00` when its top bit is set (DER integers are signed two's-complement, RSA values are unsigned magnitudes).

#### `fn kid(token: String) -> Option(String)`

The `kid` (key id) from a compact JWT's header — used to pick the right JWKS key when a provider publishes several (key rotation). `None` if the token or header is malformed.

#### `fn rsa_key_for_kid(jwks: Json, key_id: String) -> Result(String, String)`

Select the RSA public key for `kid` from a JWKS document (`{"keys":[{"kty":"RSA","kid": …,"n":…,"e":…}, …]}`) and return it as the DER PKCS#1 hex `verify_rs256`/`verify_oidc` want. This is how an OIDC verifier consumes a provider's published keys (Google, GitHub Actions): fetch the JWKS, read the token's `kid` (`jwt.kid`), then pick the key.

## `list`

The witchy standard list library. Every function here is pure: the module declares no capability parameters, so importing it grants no authority — it can only transform data. This is the capability model in miniature: a library you didn't hand a Console/Dir/Net to literally cannot reach them.

#### `fn length(xs: List(a)) -> Int`

The number of elements.

#### `fn at(xs: List(a), index: Int) -> a`

The element at `index` (0-based). Out of bounds is a runtime error on every backend.

#### `fn push(xs: List(a), x: a) -> List(a)`

A new list with `x` appended (lists are values; the original is unchanged).

#### `fn concat(xs: List(a), ys: List(a)) -> List(a)`

A new list that is `xs` followed by `ys`.

#### `fn range(n: Int) -> List(Int)`

#### `fn range_between(lo: Int, hi: Int) -> List(Int)`

The half-open span `lo..hi`: [lo, lo+1, ..., hi-1], empty when `lo >= hi`.

#### `fn range_step(start: Int, stop: Int, step: Int) -> List(Int)`

The span from `start` toward `stop` (exclusive) advancing by `step`. A positive `step` counts up while below `stop`, a negative `step` counts down while above `stop`, and a zero `step` yields [] rather than looping forever.

#### `fn map(xs: List(a), f: fn(a) -> b) -> List(b)`

Apply `f` to every element, collecting the results.

#### `fn filter(xs: List(a), keep: fn(a) -> Bool) -> List(a)`

Keep only the elements for which `keep` returns true.

#### `fn partition(xs: List(a), pred: fn(a) -> Bool) -> (List(a), List(a))`

Split `xs` into (matching, non-matching) by `pred`, each preserving the original order. A single pass — the dual of running `filter` twice.

#### `fn fold(xs: List(a), init: b, f: fn(b, a) -> b) -> b`

Reduce the list to a single value, left to right.

#### `fn reduce(xs: List(a), f: fn(a, a) -> a) -> Option(a)`

Combine the elements left to right using the first as the seed, as `Some`; `None` for the empty list. (A `fold` that needs no initial value — handy for max/min/sum over a non-empty list with a plain binary op.)

#### `fn sum(xs: List(Int)) -> Int`

The sum of a list of integers (0 for the empty list).

#### `fn sum_by(xs: List(a), f: fn(a) -> Int) -> Int`

The sum of `f` applied to each element (0 for the empty list) — e.g. a total over a record field: `sum_by(cart, fn(it): it.price * it.qty)`.

#### `fn product(xs: List(Int)) -> Int`

The product of a list of integers (1 for the empty list).

#### `fn scan(xs: List(a), init: b, f: fn(b, a) -> b) -> List(b)`

Like `fold`, but collect every intermediate accumulator left to right, starting from `init`: scan([1,2,3], 0, +) -> [0, 1, 3, 6].

#### `fn is_empty(xs: List(a)) -> Bool`

Whether the list has no elements.

#### `fn head_or(xs: List(a), default: a) -> a`

The first element, or `default` when the list is empty. (A total accessor in the style of `get_or`/`unwrap_or`, so it never indexes out of bounds.)

#### `fn last_or(xs: List(a), default: a) -> a`

The last element, or `default` when the list is empty.

#### `fn find_or(xs: List(a), pred: fn(a) -> Bool, default: a) -> a`

The first element satisfying `pred`, or `default` if none do.

#### `fn head(xs: List(a)) -> Option(a)`

The first element as `Some`, or `None` for the empty list.

#### `fn last(xs: List(a)) -> Option(a)`

The last element as `Some`, or `None` for the empty list.

#### `fn get(xs: List(a), i: Int) -> Option(a)`

The element at index `i` as `Some`, or `None` when `i` is out of range — a total, bounds-checked alternative to the `at` builtin.

#### `fn find(xs: List(a), pred: fn(a) -> Bool) -> Option(a)`

The first element satisfying `pred` as `Some`, or `None` if none do.

#### `fn find_map(xs: List(a), f: fn(a) -> Option(b)) -> Option(b)`

The first non-`None` result of applying `f` across the list (search and transform in one pass), or `None` if every result is `None`.

#### `fn min(xs: List(Int)) -> Option(Int)`

The smallest element as `Some`, or `None` for the empty list.

#### `fn max(xs: List(Int)) -> Option(Int)`

The largest element as `Some`, or `None` for the empty list.

#### `fn max_by(xs: List(a), less: fn(a, a) -> Bool) -> Option(a)`

The maximum element under a caller-supplied "is-less-than" comparator, as `Some` (the first of equal maxima), or `None` for the empty list. Generic, so it works for any type — e.g. max by a record field.

#### `fn min_by(xs: List(a), less: fn(a, a) -> Bool) -> Option(a)`

The minimum element under `less`, as `Some`; `None` for the empty list.

#### `fn position(xs: List(a), target: a) -> Option(Int)`

The index of the first element equal to `target` as `Some`, or `None` — the Option-returning companion to `index_of` (which uses a -1 sentinel).

#### `fn flatten(xss: List(List(a))) -> List(a)`

Concatenate a list of lists into one.

#### `fn flat_map(xs: List(a), f: fn(a) -> List(b)) -> List(b)`

Map each element to a list, then concatenate the results.

#### `fn transpose(xss: List(List(a))) -> List(List(a))`

Turn a list of rows into a list of columns. Rows are read only up to the length of the SHORTEST row, so a ragged tail is dropped and the result stays rectangular: `transpose([[1, 2, 3], [4, 5, 6]])` is `[[1, 4], [2, 5], [3, 6]]`.

#### `fn count(xs: List(a), pred: fn(a) -> Bool) -> Int`

How many elements satisfy `pred`.

#### `fn take_while(xs: List(a), pred: fn(a) -> Bool) -> List(a)`

The longest leading run of elements satisfying `pred`.

#### `fn drop_while(xs: List(a), pred: fn(a) -> Bool) -> List(a)`

Drop the longest leading run satisfying `pred`, keeping the rest.

#### `fn repeat(x: a, n: Int) -> List(a)`

A list of `n` copies of `x` (empty when `n <= 0`).

#### `fn find_index(xs: List(a), pred: fn(a) -> Bool) -> Int`

Index of the first element satisfying `pred`, or -1 if none do. (Like `index_of`, but matching by predicate rather than value equality.)

#### `fn zip_with(xs: List(a), ys: List(b), f: fn(a, b) -> c) -> List(c)`

Combine two lists element-wise with `f`, stopping at the shorter one.

#### `fn intersperse(xs: List(a), sep: a) -> List(a)`

Insert `sep` between adjacent elements: [a, b, c] -> [a, sep, b, sep, c].

#### `fn reverse(xs: List(a)) -> List(a)`

The list, reversed.

#### `fn sort_by(xs: List(a), less: fn(a, a) -> Bool) -> List(a)`

Sort using a caller-supplied "is-less-than" comparator — a stable merge sort (O(n log n)), so equal elements keep their original order. Generic over the element type.

#### `fn sort(xs: List(Int)) -> List(Int)`

Sort a list of integers ascending — the common default over `sort_by`.

#### `fn contains(xs: List(a), target: a) -> Bool`

Whether `target` appears in the list (by value equality).

#### `fn unique(xs: List(a)) -> List(a)`

The list with duplicates removed, keeping the first occurrence of each element (by value equality), in original order. `contains` here is this module's own list function — a same-module function shadows the like-named string builtin.

#### `fn any(xs: List(a), pred: fn(a) -> Bool) -> Bool`

Whether at least one element satisfies `pred`.

#### `fn all(xs: List(a), pred: fn(a) -> Bool) -> Bool`

Whether every element satisfies `pred` (true for the empty list).

#### `fn index_of(xs: List(a), target: a) -> Int`

Index of the first element equal to `target`, or -1 if absent.

#### `fn take(xs: List(a), n: Int) -> List(a)`

The first `n` elements (fewer if the list is shorter).

#### `fn split_at(xs: List(a), n: Int) -> (List(a), List(a))`

Split the list at index `n` into `(first n, the rest)`. `n` is clamped, so `split_at(xs, 0)` is `([], xs)` and an `n` past the end gives `(xs, [])`.

#### `fn drop(xs: List(a), n: Int) -> List(a)`

All but the first `n` elements.

#### `fn tail(xs: List(a)) -> List(a)`

All elements after the first; the empty list maps to the empty list.

#### `fn drop_last(xs: List(a)) -> List(a)`

All elements except the last; the empty list maps to the empty list.

#### `fn chunks(xs: List(a), n: Int) -> List(List(a))`

Split `xs` into consecutive sublists of length `n` (the final one may be shorter). `chunks([1,2,3,4,5], 2)` is `[[1,2],[3,4],[5]]`.

#### `fn slice(xs: List(a), start: Int, end: Int) -> List(a)`

The elements in the half-open index range [start, end), clamped to bounds. `slice(xs, 1, 3)` of [a,b,c,d] is [b,c].

#### `fn set_at(xs: List(a), index: Int, value: a) -> List(a)`

A copy of `xs` with the element at `index` replaced by `value`. An out-of- range index leaves the list unchanged. (Lists are immutable, so this returns a new list rather than mutating in place.)

#### `fn update_at(xs: List(a), index: Int, f: fn(a) -> a) -> List(a)`

A copy of `xs` with the function `f` applied to the element at `index`. An out-of-range index leaves the list unchanged.

#### `fn windows(xs: List(a), n: Int) -> List(List(a))`

All contiguous sublists of length `n` (a sliding window of step 1). Empty when `n < 1` or longer than the list. `windows([1,2,3,4], 2)` is `[[1,2],[2,3],[3,4]]`.

#### `fn zip(xs: List(a), ys: List(b)) -> List((a, b))`

Pair up two lists element-wise, stopping at the shorter one.

#### `fn unzip(xs: List((a, b))) -> (List(a), List(b))`

Split a list of pairs into a pair of lists — the inverse of `zip`.

#### `fn enumerate(xs: List(a)) -> List((Int, a))`

Pair each element with its index: `[a, b]` -> `[(0, a), (1, b)]`.

## `math`

The witchy standard math library: small integer helpers, pure and capability-free. (Comparison can't be generic without type classes, so these are Int-specific.) --- Native primitives (intercepted by both backends; self-recursive placeholder bodies give the type checker the signatures). ---

#### `fn to_float(n: Int) -> Float`

Int -> Float, exactly (within f64 precision).

#### `fn to_int(x: Float) -> Int`

Float -> Int, truncating toward zero; NaN -> 0; out-of-range saturates.

#### `fn sqrt(x: Float) -> Float`

The square root.

#### `fn min(a: Int, b: Int) -> Int`

#### `fn max(a: Int, b: Int) -> Int`

#### `fn abs(n: Int) -> Int`

#### `fn sign(n: Int) -> Int`

-1, 0, or 1 depending on the sign of `n`.

#### `fn clamp(x: Int, lo: Int, hi: Int) -> Int`

Constrain `x` to the inclusive range [lo, hi].

#### `fn pow(base: Int, exp: Int) -> Int`

`base` raised to a non-negative `exp` (exp <= 0 gives 1).

#### `fn ceil_div(a: Int, b: Int) -> Int`

Ceiling division: the smallest integer >= a / b (e.g. items split into pages of `b`). For a positive divisor `b`; `ceil_div(7, 3)` is 3, `ceil_div(6, 3)` is 2.

#### `fn round_div(a: Int, b: Int) -> Int`

Division rounded to the nearest integer (ties away from zero), for a positive `b`: `round_div(7, 2)` is 4, `round_div(5, 3)` is 2, `round_div(-7, 2)` is -4.

#### `fn gcd(a: Int, b: Int) -> Int`

Greatest common divisor (Euclid's algorithm).

#### `fn lcm(a: Int, b: Int) -> Int`

Least common multiple (0 if either argument is 0). Divides before multiplying to keep the intermediate value small.

#### `fn is_even(n: Int) -> Bool`

Whether `n` is even.

#### `fn is_odd(n: Int) -> Bool`

Whether `n` is odd.

#### `fn factorial(n: Int) -> Int`

`n!` — the product 1*2*...*n (1 for n <= 1). Watch the 32-bit range: factorial grows past it quickly (13! already overflows).

#### `fn is_prime(n: Int) -> Bool`

Whether `n` is prime (trial division up to math.sqrt(n); n < 2 is not prime).

#### `fn isqrt(n: Int) -> Int`

Integer square root: the largest `r` with `r*r <= n`. A negative `n` yields 0. Uses `mid <= n / mid` instead of `mid * mid <= n` so it never overflows.

#### `fn is_perfect_square(n: Int) -> Bool`

Whether `n` is a perfect square (0, 1, 4, 9, ...). Negative `n` is never one.

#### `fn to_base(n: Int, base: Int) -> String`

Render `n` in `base` (2..16) with lowercase digits; a negative `n` gets a leading "-". An out-of-range base yields "" (so callers can detect misuse).

#### `fn to_hex(n: Int) -> String`

`n` in hexadecimal (e.g. 255 -> "ff").

#### `fn to_binary(n: Int) -> String`

`n` in binary (e.g. 5 -> "101").

#### `fn float_min(a: Float, b: Float) -> Float`

--- Float versions (Int comparison can't be reused for Float) ---

#### `fn float_max(a: Float, b: Float) -> Float`

#### `fn float_abs(x: Float) -> Float`

#### `fn float_clamp(x: Float, lo: Float, hi: Float) -> Float`

#### `fn format_float(x: Float, decimals: Int) -> String`

Format `x` with `decimals` digits after the decimal point, rounded half-up: format_float(3.14159, 2) = "3.14", format_float(-0.5, 1) = "-0.5", format_float(2.0, 0) = "2". Built from float arithmetic, so unlike the `to_string` builtin it works on the compiled WASM backend too (which has no float formatting). Best for a fixed number of places; very large magnitudes lose precision to the Float itself.

## `meta`

Compile-time type introspection — the `typeInfo` half of witchy's comptime reflection (Zig's `@typeInfo`). A `comptime:` block can read the structure of every type in its module as ordinary data and generate code from it (e.g. a `to_json` specialized to a record's fields), with zero runtime cost. The compiler injects the type list as `module_types()`; these are the shapes it hands you.

This is COMPILE-TIME structure (field names + declared type names as strings), distinct from `std/reflect`'s runtime `Mirror` (a value's structure at runtime).

#### `type FieldInfo`

One field of a record: its name and its declared type rendered as a string (e.g. "Int", "List(String)", "Option(Point)").

- `FieldInfo { name: String, type_name: String }`

#### `type VariantInfo`

One constructor of a sum type: its name and its positional payload types.

- `VariantInfo { name: String, field_types: List(String) }`

#### `type TypeInfo`

A type's structure. `kind` is "record" (one constructor with named fields), "sum" (one or more positional constructors), or "unit". `fields` is populated for records, `variants` for sums.

- `TypeInfo { name: String, kind: String, params: List(String), fields: List(FieldInfo), variants: List(VariantInfo) }`

#### `fn derive_show(t: TypeInfo) -> String`

`derive(Show)` → structural rendering via the `__render` builtin.

#### `fn derive_eq(t: TypeInfo) -> String`

`derive(Eq)` → the total-equality marker. Refines `PartialEq` (derive both).

#### `fn derive_partial_eq(t: TypeInfo) -> String`

`derive(PartialEq)` → field-wise structural equality. The operators dispatch per field, so it is content-correct on both backends: a record compares each field with `==`; a sum type matches the variant and compares payloads.

#### `fn derive_reflect(t: TypeInfo) -> String`

`derive(Reflect)` → an `impl Reflect for T` building the value's `Mirror`: a record to `MRecord("T", [(field, reflect field)…])`, a sum type to a `match` over variants to `MVariant`. (The module must `import reflect`; caller checks.)

#### `fn derive_ord(t: TypeInfo) -> String`

`derive(Ord)` → lexicographic `compare` returning `Ordering` (records only; the caller validates the shape). Requires the `PartialEq`/`Eq`/`PartialOrd` impls too.

#### `fn derive_partial_ord(t: TypeInfo) -> String`

`derive(PartialOrd)` → lexicographic `partial_compare` (records only).

#### `fn derive_deserialize(t: TypeInfo) -> String`

`derive(Deserialize)` generates `from_json` for a record (the caller validates the shape). It decodes and coerces each field, returning on the first error. There is no matching `Serialize` derive, because reflection (`json.value_of`, `stringify`, `Into(Json)`) already encodes any value, so only this reconstruction is per-type. The generated code uses only json/result/list/option.

## `oauth`

oauth — the OAuth 2.0 Authorization Code flow (RFC 6749 §4.1), the basis of "Log in with GitHub / Google". Pure witchy over `std/http` (HTTPS) + `url`. A relying party:   1. redirects the user to `authorize_url(...)`;   2. receives a `code` (and the `state` it sent) at its registered callback;   3. exchanges the code for an access token with `exchange_code(...)`. Identity is then read from a provider endpoint (GitHub `/user`) or, for OIDC, the `id_token` (verify with `std/jwt`). `state` is an opaque anti-CSRF token the caller signs before the redirect and re-checks on the callback — bind it to the session.

#### `fn authorize_url(authorize_endpoint: String, client_id: String, redirect_uri: String, scope: String, state: String) -> String`

The provider authorization-endpoint URL to redirect the user to. After the user approves, the provider redirects to `redirect_uri?code=...&state=...`. `scope` is the provider's space-separated permission list (e.g. GitHub `read:user`, OIDC `openid email`).

#### `fn exchange_code(net: Net[Connect, Tcp], token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(String, String)`

Exchange an authorization `code` for an access token at the provider's token endpoint — an HTTPS POST with a form-encoded body and `Accept: application/json`. Returns the `access_token`, or a reason. Needs a `Net` that reaches the token host over TLS; the `client_secret` should come from a `Secret`, never a literal.

#### `fn exchange_code_id_token(net: Net[Connect, Tcp], token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(String, String)`

Like `exchange_code`, but returns the OIDC `id_token` (a JWT carrying the user's identity) instead of the access token — for "Log in with Google" and other OIDC providers. Verify the returned token with `std/jwt` (`kid` → `rsa_key_for_kid` over the provider's JWKS → `verify_oidc`).

#### `fn token_response(net: Net[Connect, Tcp], token_url: String, client_id: String, client_secret: String, code: String, redirect_uri: String) -> Result(Json, String)`

The raw token-endpoint response as JSON (the HTTPS POST exchanging the code) — the level beneath `exchange_code`; read `access_token` / `id_token` / `refresh_token` from it.

#### `fn bearer_get_json(net: Net[Connect, Tcp], url: String, token: String) -> Result(Json, String)`

GET `url` with a `Bearer` access token and parse the JSON body — the "read the signed-in user" step after `exchange_code` (GitHub `/user`, an OIDC userinfo endpoint). Sends a `User-Agent` (GitHub rejects requests without one). Returns the parsed JSON, or a reason. The caller reads identity fields it trusts (`login`, `id`, `email`).

## `option`

The witchy standard `Option` type and helpers. `import option` brings the type into scope (so the `?` operator works) and gives the usual combinators. Pure and capability-free.

#### `type Option`

- `Some(a)`
- `None`

#### `fn is_some(o: Option(a)) -> Bool`

#### `fn unwrap_or(o: Option(a), default: a) -> a`

The Some value, or `default` if it's None.

#### `fn map(o: Option(a), f: fn(a) -> b) -> Option(b)`

Transform the Some value, leaving None untouched.

#### `fn is_none(o: Option(a)) -> Bool`

True if the option holds no value.

#### `fn and_then(o: Option(a), f: fn(a) -> Option(b)) -> Option(b)`

Chain a fallible step: apply `f` (which itself yields an Option) to the Some value, or short-circuit on None.

#### `fn filter(o: Option(a), pred: fn(a) -> Bool) -> Option(a)`

Keep the Some value only if it satisfies `pred`; otherwise None.

#### `fn unwrap_or_else(o: Option(a), f: fn() -> a) -> a`

The Some value, or the result of calling `f` (a lazily-computed default).

#### `fn or(o: Option(a), alt: Option(a)) -> Option(a)`

The option if it is Some, otherwise the `alt` option.

#### `fn or_else(o: Option(a), f: fn() -> Option(a)) -> Option(a)`

The option if it is Some, otherwise the option produced by `f` (lazy).

#### `fn map_or(o: Option(a), default: b, f: fn(a) -> b) -> b`

Apply `f` to the Some value, or return `default` for None — `map` then `unwrap_or` in one step.

#### `fn ok_or(o: Option(a), err: e) -> Result(a, e)`

Turn an Option into a Result: `Some(v)` becomes `Ok(v)`, `None` becomes `Err(err)` — the inverse of `result.ok`.

#### `fn ok_or_else(o: Option(a), f: fn() -> e) -> Result(a, e)`

Like `ok_or`, but the error for `None` is produced lazily by `f`.

#### `fn flatten(oo: Option(Option(a))) -> Option(a)`

Collapse one layer of nesting: `Some(Some(v))` becomes `Some(v)`, and both `Some(None)` and `None` become `None`.

#### `fn zip(oa: Option(a), ob: Option(b)) -> Option((a, b))`

Combine two options into an option of a pair: `Some((x, y))` only when both are `Some`, otherwise `None`.

#### `fn all(xs: List(Option(a))) -> Option(List(a))`

Collect a list of Options into an Option of the list: `Some` of every value in order, or `None` if any element is `None`.

## `path`

path — pure manipulation of '/'-separated path strings.

This is string surgery only — it never touches the filesystem (that is the `Dir` capability, wrapped by `std/fs`). Splitting, joining, the base/dir/ext components, and a `normalize` that collapses `.` and `..`.

#### `fn is_abs(p: String) -> Bool`

Whether `p` is absolute (rooted at `/`).

#### `fn join(a: String, b: String) -> String`

Join two path pieces with a single `/`. An empty piece is ignored; an absolute `b` replaces `a`.

#### `fn base(p: String) -> String`

The final component: "a/b/c.txt" -> "c.txt", "a/b/" -> "b", "/" -> "/".

#### `fn dir(p: String) -> String`

Everything before the final component: "a/b/c" -> "a/b", "c" -> "", "/x" -> "/".

#### `fn ext(p: String) -> String`

The extension after the final `.` in the base name, WITHOUT the leading dot ("a/b.tar.gz" -> "gz"), or "" when there is none — matching Rust's `Path::extension` (not Node/Python, which keep the dot). A dotfile base (".bashrc") has no extension.

#### `fn stem(p: String) -> String`

The base name without its extension (".bashrc" -> ".bashrc"; "a.b.c" -> "a.b").

#### `fn normalize(p: String) -> String`

Collapse `.` and `..` segments and redundant slashes. A relative path that backs out past its start keeps the leading `..`s; an absolute one cannot escape its root. An empty result is "." (relative) or "/" (absolute).

## `random`

A small, deterministic pseudo-random generator: the Park-Miller "minimal standard" LCG, `state' = state * 16807 mod (2^31 - 1)`. The intermediate fits in i64 (no overflow), so it is content-correct on both backends. State is threaded explicitly — the same seed always replays the same sequence, which is what you want for tests, sampling, and games. NOT for cryptography. Pure and capability-free.

#### `type Rng`

- `Rng(Int)`

#### `fn seed(s: Int) -> Rng`

A generator from seed `s` (any Int is mapped into the valid range 1..modulus).

#### `fn next(r: Rng) -> (Int, Rng)`

Advance the generator: a pseudo-random Int in [1, 2^31-1) and the next state.

#### `fn next_below(r: Rng, bound: Int) -> (Int, Rng)`

A pseudo-random Int in [0, bound) (bound must be positive) and the next state.

#### `fn next_bool(r: Rng) -> (Bool, Rng)`

A pseudo-random Bool (true ~half the time) and the next state.

#### `fn choice(xs: List(a), r: Rng) -> (Option(a), Rng)`

A uniformly-chosen element of `xs` (None if empty) and the next state.

## `reflect`

Reflection: a value's structure as data, so one function can work over any type. `reflect(x)` returns a `Mirror` describing `x`: a record's named fields, a sum type's variant, a list's elements, or a scalar. Code that would otherwise need a per-type `derive` (JSON encoding, debug rendering, structural diffing) is written once against `Mirror`. Scalars and the built-in containers (`List`, `Option`, tuples, `Dict`) are reflectable out of the box; a user `type` becomes reflectable when you add `derive(Reflect)` to it (which needs `import reflect`), much like Zig's `@typeInfo` but opt-in per type — so `reflect(x)` / `json.stringify(x)` work without a per-type macro once the type derives it.

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

#### `fn reflect_one(x: a) -> Mirror where a: Reflect`

Reflect a single value through a free function. The generated `reflect` calls this rather than the trait method directly, because trait dispatch resolves on params and loop vars but not on `match` bindings (so an Option field's `Some(x)` arm could not call `reflect(x)`). Here `x` is a parameter, which always resolves.

#### `fn reflect_option(o: Option(a)) -> Mirror where a: Reflect`

Reflect an `Option` to a `Some`/`None` `MVariant`. The payload reflects through a loop over `opt_list` (0 or 1 element), so it calls the trait method `reflect(x)` on a loop var, which resolves under the generic `a` bound where a generic free function or a match binding would not. An empty payload gives None.

#### `fn reflect_list(xs: List(a)) -> Mirror where a: Reflect`

Reflect a list of `Reflect` elements. This is a free function rather than an `impl Reflect for List(a)` because method dispatch on a `List` receiver binds to the `list` module, so the generated `reflect` for a record's List field calls this.

#### `fn debug(x: a) -> String where a: Reflect`

--- a second consumer: structural debug rendering --------------------------- `debug(x)` renders any value from its reflection, using the same `reflect` that backs `json`.

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

The witchy standard `Result` type and helpers. `import result` brings the type into scope (so the `?` operator works) and gives the usual combinators. Pure and capability-free, like every std module.

#### `type Result`

- `Ok(a)`
- `Err(e)`

#### `fn is_ok(r: Result(a, e)) -> Bool`

#### `fn unwrap_or(r: Result(a, e), default: a) -> a`

The Ok value, or `default` if it's an Err.

#### `fn map_ok(r: Result(a, e), f: fn(a) -> b) -> Result(b, e)`

Transform the Ok value, leaving an Err untouched.

#### `fn is_err(r: Result(a, e)) -> Bool`

True if the result is an Err.

#### `fn and_then(r: Result(a, e), f: fn(a) -> Result(b, e)) -> Result(b, e)`

Chain a fallible step: apply `f` (which itself yields a Result) to the Ok value, or propagate the Err unchanged.

#### `fn map_err(r: Result(a, e), f: fn(e) -> g) -> Result(a, g)`

Transform the Err value, leaving an Ok untouched.

#### `fn unwrap_err(r: Result(a, e), default: e) -> e`

The Err value, or `default` if it's Ok.

#### `fn unwrap_or_else(r: Result(a, e), f: fn() -> a) -> a`

The Ok value, or the result of calling `f` (a lazily-computed default).

#### `fn or(r: Result(a, e), alt: Result(a, e)) -> Result(a, e)`

The result if it is Ok, otherwise the `alt` result.

#### `fn or_else(r: Result(a, e), f: fn(e) -> Result(a, e)) -> Result(a, e)`

The result if it is Ok, otherwise the result produced by applying `f` to the error — a lazy, error-aware recovery step.

#### `fn map_or(r: Result(a, e), default: b, f: fn(a) -> b) -> b`

Apply `f` to the Ok value, or return `default` for an Err — `map_ok` then `unwrap_or` in one step.

#### `fn flatten(rr: Result(Result(a, e), e)) -> Result(a, e)`

Collapse one layer of nesting: `Ok(Ok(v))` becomes `Ok(v)`; `Ok(Err(e))` and `Err(e)` become `Err(e)`. The Result counterpart of `option.flatten`.

#### `fn ok(r: Result(a, e)) -> Option(a)`

The Ok value as `Some`, or `None` for an Err — discards the error, turning a Result into an Option.

#### `fn err(r: Result(a, e)) -> Option(e)`

The Err value as `Some`, or `None` for an Ok.

#### `fn all(xs: List(Result(a, e))) -> Result(List(a), e)`

Collect a list of Results into a Result of the list: `Ok` of every value in order, or the first `Err` encountered (the "sequence" of fallible steps).

#### `fn partition(xs: List(Result(a, e))) -> (List(a), List(e))`

Split a list of Results into the Ok values and the Err values, each in order — for batch work that reports every failure, not just the first.

## `rights`

rights — rights-precise reasoning over capability footprints.

A capability is rendered the way the compiler's footprint prints it: "Console", "Dir[Read]", "Net[Connect, Tcp]". A *declared* capability covers a *demanded* one when they share a base kind AND the declared authority is at least as broad: a bare "Net" admits any rights of that kind, while "Net[Connect]" admits only a subset — so Net[Connect] does NOT cover full Net. This rights-precision is what the package manager's declared-vs-actual check and the block-on-widening gate both rely on, so it lives in one tested place.

#### `fn covers(declared: String, demanded: String) -> Bool`

Whether `declared` covers `demanded` (same kind, broad enough rights).

#### `fn covered(declared: List(String), demanded: String) -> Bool`

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

secretstore — read named secrets from the host-granted `SecretStore`. The secrets come from `--secret name=value` / `--secret-file name=path` (and `--signing-key` as sugar for `--secret-file signing=<path>`); their bytes stay host-side. `get` is intercepted by the runtime, since a `SecretStore` is a capability, not plain data. Each `Secret` then supports `crypto.sign` / `crypto.public_key` (as a hex Ed25519 seed) and `crypto.reveal` (value secrets).

#### `fn get(store: SecretStore, name: String) -> Option(Secret)`

Fetch the secret named `name`, or `None` if it was not granted.

#### `fn require(store: SecretStore, name: String) -> Secret`

Fetch a *required* secret named `name`, returning the `Secret` directly. Use this when absence is a configuration error (e.g. a server's root signing key): it fails loudly rather than handing back an `Option` to unwrap. The body is a placeholder — the runtime intercepts the call (interpreter) / lowers it to a host handle lookup (WASM); it is never executed.

## `semver`

semver — semantic versions and constraints, for dependency resolution.

Intentionally minimal (matching the package manager's needs): `major.minor.patch` versions and the `^`, `~`, exact, `>=`, and `*` constraints — enough for deterministic resolution without a full SemVer grammar. A missing component parses as 0 (`1.2` is `1.2.0`); a non-numeric component is an error.

#### `type Version`

- `Version(Int, Int, Int)`

#### `type Req`

A version constraint (requirement).

- `Caret(Version)`
- `Tilde(Version)`
- `Exact(Version)`
- `AtLeast(Version)`
- `Any`

#### `fn version(major: Int, minor: Int, patch: Int) -> Version`

--- constructors + accessors ------------------------------------------------

#### `fn major(v: Version) -> Int`

#### `fn minor(v: Version) -> Int`

#### `fn patch(v: Version) -> Int`

#### `fn parse(s: String) -> Result(Version, String)`

Parse `major.minor.patch` (missing trailing components default to 0). Errors on a non-numeric component or more than three components.

#### `fn format(v: Version) -> String`

#### `fn compare(a: Version, b: Version) -> Int`

-1 if a < b, 0 if equal, 1 if a > b — comparing major, then minor, then patch.

#### `fn equals(a: Version, b: Version) -> Bool`

#### `fn lt(a: Version, b: Version) -> Bool`

#### `fn lte(a: Version, b: Version) -> Bool`

#### `fn gt(a: Version, b: Version) -> Bool`

#### `fn gte(a: Version, b: Version) -> Bool`

#### `fn parse_req(s: String) -> Result(Req, String)`

--- constraints -------------------------------------------------------------

#### `fn matches(req: Req, v: Version) -> Bool`

Whether `v` satisfies the constraint `req`.

#### `fn best(versions: List(Version), req: Req) -> Option(Version)`

The highest version in `versions` that satisfies `req`, or None if none do.

## `server`

The witchy web framework — a slice of axum/tower over the `Net` capability, built on the shared `Request`/`Response` types in `http`.

A handler is a pure `fn(Request) -> Response`: it has NO capability parameters, so it is *structurally* unable to touch the network, filesystem, or console. To give a handler authority (a logger, a store, an outbound client), capture it in the closure — capture IS dependency injection. `serve` holds the `Net` to listen and never hands it to a handler, so even a mounted third-party handler can only compute over the request; it cannot phone home.

  let app = server.router()       .get("/", home)       .get("/users/:id", show)       .layer(logging(console))   server.serve(net, "127.0.0.1:8080", app)

#### `type Route`

One route: HTTP method, a path pattern (`:param` captures a segment, `*rest` captures the remainder), and the handler.

- `Route(String, String, fn(Request) -> Response)`

#### `type Router`

A router: its routes plus the middleware layers wrapping the whole dispatch.

- `Router(List(Route), List(fn(fn(Request) -> Response) -> fn(Request) -> Response))`

#### `fn method(req: Request) -> String`

---- Request accessors ----

#### `fn path(req: Request) -> String`

#### `fn param(req: Request, name: String) -> String`

A captured path parameter (`:name`), or "" if absent.

#### `fn query(req: Request, name: String) -> String`

A query-string parameter (`?name=...`), or "" if absent.

#### `fn request_header(req: Request, name: String) -> Option(String)`

A request header, looked up case-insensitively, or None.

#### `fn request_body(req: Request) -> String`

#### `fn json_body(req: Request) -> Result(Json, String)`

Decode the request body as JSON — the role of axum's `Json` extractor. Returns an Err (rather than panicking) on malformed input, so handlers stay total.

#### `fn form_body(req: Request) -> List((String, String))`

Parse an `application/x-www-form-urlencoded` body (`a=1&b=2`) into key/value pairs — for HTML form POSTs.

#### `fn form_field(req: Request, name: String) -> String`

A single form field, or "" if absent.

#### `fn text(code: Int, b: String) -> Response`

---- Response constructors (an axum `IntoResponse` in spirit) ----

#### `fn html(code: Int, b: String) -> Response`

#### `fn json(code: Int, b: String) -> Response`

`b` is an already-encoded JSON string (e.g. from `json.encode`).

#### `fn json_value(code: Int, j: Json) -> Response`

A JSON response from a `Json` value — encodes it for you.

#### `fn send(code: Int, value: a) -> Response where a: Reflect`

A JSON response from any reflectable value. Reflection serializes it, so a handler can return `server.send(200, .{names: names})`, or a record, without building `Json` by hand. Use `json` or `json_value` for pre-encoded bytes or a `Json` value.

#### `fn status_only(code: Int) -> Response`

#### `fn ok(b: String) -> Response`

---- Status-named constructors (axum `StatusCode` ergonomics) ----

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

Return `resp` with an extra header — for handlers and middleware that decorate a response (e.g. add a `set-cookie` or a tracing header).

#### `fn with_status(resp: Response, code: Int) -> Response`

Return `resp` with its status code replaced.

#### `fn router() -> Router`

#### `fn route(r: Router, m: String, p: String, h: fn(Request) -> Response) -> Router`

#### `fn serve(net: Net[Listen, Tcp], addr: String, app: Router)`

Serve `app` on `addr` forever. Needs the `Net` capability to listen; handlers never receive it.

#### `fn serve_n(net: Net[Listen, Tcp], addr: String, app: Router, n: Int)`

Serve exactly `n` requests then return — for tests and one-shot servers.

## `set`

Set(a) — an unordered collection of distinct values. Members are compared by value equality (a `where a: Eq` bound on every operation that compares), so sets of Ints, Strings, tuples, or your own `Eq` types all work. Build one with `set.new()` / `set.from_list(xs)`, test membership with `set.contains`, and reach for `union`/`intersection`/`difference` for the algebra. `set.show(s)` renders the members; `set.to_list(s)` returns them in insertion order.

#### `type Set`

- `Set { items: List(a) }`

#### `fn new() -> Set(a)`

The empty set.

#### `fn from_list(xs: List(a)) -> Set(a) where a: Eq`

A set of the distinct values in `xs` (duplicates collapse).

#### `fn insert(s: Set(a), x: a) -> Set(a) where a: Eq`

`s` with `x` added (a no-op if already present).

#### `fn remove(s: Set(a), x: a) -> Set(a) where a: Eq`

`s` with `x` removed (a no-op if absent).

#### `fn contains(s: Set(a), x: a) -> Bool where a: Eq`

Whether `x` is a member of `s`.

#### `fn size(s: Set(a)) -> Int`

The number of distinct members.

#### `fn is_empty(s: Set(a)) -> Bool`

Whether the set has no members.

#### `fn to_list(s: Set(a)) -> List(a)`

The members as a list, in insertion order.

#### `fn show(s: Set(a)) -> String`

Render the set as `{a, b, c}` (members in insertion order). Use this rather than interpolating a `Set` directly: `${s}` falls back to the structural form `Set([...])`, which the compiled backend can't render for a generic record.

#### `fn union(s: Set(a), t: Set(a)) -> Set(a) where a: Eq`

Every member of either set.

#### `fn intersection(s: Set(a), t: Set(a)) -> Set(a) where a: Eq`

The members in both sets.

#### `fn difference(s: Set(a), t: Set(a)) -> Set(a) where a: Eq`

The members of `s` that are not in `t`.

#### `fn symmetric_difference(s: Set(a), t: Set(a)) -> Set(a) where a: Eq`

The members in exactly one of the two sets.

#### `fn is_subset(s: Set(a), t: Set(a)) -> Bool where a: Eq`

Whether every member of `s` is also in `t`.

#### `fn is_disjoint(s: Set(a), t: Set(a)) -> Bool where a: Eq`

Whether the two sets share no members.

## `show`

The witchy standard `Show` trait: render a value as a `String`. Built-in impls cover the scalars — `Int`, `Float`, `Bool`, `String`, and `Duration` (which shows in its human form, `1m30s`, not raw milliseconds); implement `Show` for your own types to give them a *custom* readable form. (The built-in `to_string` already renders any value structurally — `Point(1, 2)`, `[Circle(2), Dot]` — on every backend; reach for `Show` when you want a different rendering than that default.) Pure except `say`, which takes the `Console` it prints to.

#### `fn show_list(xs: List(a)) -> String where a: Show`

Render a list as "[a, b, c]" using each element's Show impl — reach for this when your element type has a *custom* `Show` you want applied (the built-in `to_string` renders lists structurally on every backend already). Correct on both backends (the Show dispatch is monomorphized per element type).

#### `fn say(console: Console, x: impl Show)`

Print any `Show` value without converting it by hand — `say(console, 42)`, `say(console, point)`. The Show-accepting `print` you reach for instead of `print(console, "${n}")`. (A thin wrapper kept out of the `print` builtin so a builtin never depends on a std trait.)

## `string`

The witchy standard string library. Like `list`, it is pure: it declares no capability parameters, so importing it grants no authority. The primitive string operations (`split`, `replace`, `contains`, `to_upper`, ...) are builtins; these are the conveniences built on top of them.

#### `fn length(s: String) -> Int`

The string's length in BYTES (UTF-8). For user-perceived characters, see `char_count`.

#### `fn char_count(s: String) -> Int`

The number of Unicode scalar values.

#### `fn chars(s: String) -> List(String)`

The characters, each as a single-character String — one O(n) pass, so callers can index characters in O(1).

#### `fn from_code(code: Int) -> String`

The single character (as a String) for a Unicode scalar value — the inverse of reading a code point. An out-of-range or surrogate value yields U+FFFD (the replacement character), never an error. Powers the JSON `\u` decoder.

#### `fn split(s: String, sep: String) -> List(String)`

Split on every occurrence of `sep`.

#### `fn contains(s: String, needle: String) -> Bool`

Whether `needle` occurs in `s`.

#### `fn starts_with(s: String, prefix: String) -> Bool`

#### `fn ends_with(s: String, suffix: String) -> Bool`

#### `fn index_of(s: String, needle: String) -> Int`

The byte index of the first occurrence of `needle`, or -1.

#### `fn replace(s: String, from: String, to: String) -> String`

Replace every occurrence of `from` with `to`.

#### `fn substring(s: String, start: Int, end: Int) -> String`

The substring from byte `start` (inclusive) to `end` (exclusive).

#### `fn to_upper(s: String) -> String`

ASCII case mapping (the portable set both backends share).

#### `fn to_lower(s: String) -> String`

#### `fn trim(s: String) -> String`

Strip leading and trailing ASCII whitespace.

#### `fn to_int(s: String) -> Int`

Parse a decimal integer; junk, overflow, or an empty string ABORTS the program (a runtime error, not an `Err`) on every backend. For the total version that returns `Option(Int)`, see `parse_int`.

#### `fn join(parts: List(String), sep: String) -> String`

#### `fn repeat(s: String, n: Int) -> String`

Repeat a string `n` times.

#### `fn pad_left(s: String, width: Int, fill: String) -> String`

Left-pad `s` with copies of `fill` until it is `width` characters wide. The padding is trimmed to fit exactly, so any fill width yields a result of exactly `width` chars; `s` is returned unchanged when already that long.

#### `fn pad_right(s: String, width: Int, fill: String) -> String`

Right-pad `s` with copies of `fill` until it is `width` characters wide.

#### `fn center(s: String, width: Int, fill: String) -> String`

Center `s` in a field `width` characters wide, padding both sides with `fill`; an odd remainder goes on the right. `s` is returned unchanged when already at least that wide.

#### `fn strip_prefix(s: String, prefix: String) -> String`

Remove `prefix` from the front of `s` when present; otherwise return `s` unchanged. The complement of the `starts_with` builtin.

#### `fn strip_suffix(s: String, suffix: String) -> String`

Remove `suffix` from the end of `s` when present; otherwise return `s` unchanged. The complement of the `ends_with` builtin.

#### `fn char_at(s: String, i: Int) -> String`

The single character at character index `i` (counted by Unicode scalar), or "" when `i` is out of range. A readable shorthand over `substring`.

#### `fn is_empty(s: String) -> Bool`

Whether the string has no characters.

#### `fn reverse(s: String) -> String`

The string with its characters in reverse order. Counted by Unicode scalar (via `char_count`/`substring`), so multi-byte characters stay intact: `reverse("café")` is `"éfac"`.

#### `fn take(s: String, n: Int) -> String`

The first `n` characters (the whole string if it is shorter, "" if n <= 0).

#### `fn drop(s: String, n: Int) -> String`

All characters after the first `n` ("" if n covers the whole string).

#### `fn count(s: String, sub: String) -> Int`

The number of non-overlapping occurrences of `sub` in `s` (0 for an empty `sub`). After each match the search resumes past it.

#### `fn words(text: String) -> List(String)`

The whitespace-separated words of `text`: tabs, newlines, and carriage returns are treated as spaces, and empty pieces (from runs of whitespace) are dropped. `words("the  quick\tfox")` is `["the", "quick", "fox"]`.

#### `fn replace_first(s: String, from: String, to: String) -> String`

Replace only the first occurrence of `from` with `to`; return `s` unchanged when `from` is absent. (The `replace` builtin replaces every occurrence.)

#### `fn split_once(s: String, sep: String) -> (String, String)`

Split at the first occurrence of `sep` into `(before, after)`, with `sep` itself dropped. When `sep` is absent, returns `(s, "")`. Handy for parsing `key=value` or `host:port`. Counted by Unicode scalar.

#### `fn last_index_of(s: String, sep: String) -> Int`

The index of the LAST occurrence of `sep` in `s`, or -1 when absent (and -1 for an empty `sep`). The right-to-left companion of the `index_of` builtin.

#### `fn rsplit_once(s: String, sep: String) -> (String, String)`

Split on the LAST occurrence of `sep` (e.g. a file extension): `rsplit_once` of `"a.b.c"` on `"."` is `("a.b", "c")`. When `sep` is absent the whole string is the right part: `("", s)` — mirroring `split_once`'s `(s, "")`.

#### `fn parse_int(s: String) -> Option(Int)`

Safely parse a base-10 integer: an optional leading `-`/`+` then one or more digits. Returns None for empty, sign-only, non-digit, or out-of-range (beyond the i64 range) input — so it never traps the way the raw `string_to_int` builtin can.

#### `fn lines(text: String) -> List(String)`

Split text into its newline-separated lines.

#### `fn trim_start(s: String) -> String`

Remove leading whitespace.

#### `fn trim_end(s: String) -> String`

Remove trailing whitespace.

## `task`

std/task — the cooperative task substrate and its executor.

A `Task(m, a)` is a CPS-over-closures computation that, when stepped, either completes (`Done`) or yields an effect back to the executor: cooperate (`Yield`), `spawn` a child (`Fork`), `join` one (`Wait`), or a channel op (`Open`/`Push`/`Pull`/`PullAny`, produced by `std/chan`). `run` drives a task (and everything it spawns) to completion on a deterministic round-robin schedule, so a concurrent run is byte-identical on the interpreter and the compiled WebAssembly — no scheduler state in the runtime, no `Pin`.

This module is the scheduling core: the `Task` monad, `spawn`/`join`/ `yield_now`, and the executor. First-class channels are layered on top in `std/chan`; lightweight value-returning structured concurrency (`join_all`/ `select` over independent futures) lives in `std/future`.

Model: one message type `m` per program (a single `run`'s channels all carry `m`; union into a sum type if you need several shapes). Spawned tasks return `Nil`; a task reports a result by sending it on a channel, not by returning it (a typed `JoinHandle(T)` would force a native runtime and break the parity contract).

The `async`/`await` CPS transform lowers onto this substrate (`task.lazy`/ `and_then`/`done`/`run`), so `chan.recv(rx).await` / `chan.send(tx, x).await` work in async fns.

#### `type Step`

- `Done(a)`
- `Yield(Task(m, a))`
- `Fork(Task(m, Nil), fn(Int) -> Task(m, a))`
- `Open(Int, fn(Int) -> Task(m, a))`
- `Push(Int, m, fn(Nil) -> Task(m, a))`
- `Pull(Int, fn(Option(m)) -> Task(m, a))`
- `PullAny(List(Int), fn(Option((Int, m))) -> Task(m, a))`
- `Wait(Int, fn(Nil) -> Task(m, a))`

#### `type Task`

- `Task(fn() -> Step(m, a))`

#### `type Handle`

- `Handle(Int)`

#### `type Slot`

- `Active(Task(m, Nil))`
- `WaitRecv(Int, fn(Option(m)) -> Task(m, Nil))`
- `WaitSend(Int, m, fn(Nil) -> Task(m, Nil))`
- `WaitAny(List(Int), fn(Option((Int, m))) -> Task(m, Nil))`
- `WaitJoin(Int, fn(Nil) -> Task(m, Nil))`
- `Ended`

#### `fn poll(t: Task(m, a)) -> Step(m, a)`

#### `fn done(x: a) -> Task(m, a)`

A finished task.

#### `fn ready_unit() -> Task(m, Nil)`

An already-complete `Task(m, Nil)` — the async/await lowering target for a body that falls off its end.

#### `fn yield_now() -> Task(m, Nil)`

Hand control back to the executor once, then continue.

#### `fn and_then(t: Task(m, a), k: fn(a) -> Task(m, b)) -> Task(m, b)`

Sequence: run `t`, then continue with `k` applied to its result. This is what `await` lowers to — the continuation `k` is the rest of the body.

#### `fn map(t: Task(m, a), f: fn(a) -> b) -> Task(m, b)`

Transform a task's result.

#### `fn lazy(thunk: fn() -> Task(m, a)) -> Task(m, a)`

Build the task `thunk()` lazily: nothing runs until the first poll. This is what makes an `async fn` LAZY — calling it yields a task that does no work until driven (by `run`, or by being `spawn`ed, or `await`ed).

#### `fn for_each(xs: List(a), f: fn(a) -> Task(m, Nil)) -> Task(m, Nil)`

Run `f(x)` as a task for each `x` in `xs`, in order — the lowering target for an `await` inside a `for x in xs:` loop.

#### `fn spawn(child: Task(m, Nil)) -> Task(m, Handle)`

Start `child` as a concurrent task; the returned handle completes when it does.

#### `fn join(h: Handle) -> Task(m, Nil)`

Block until the spawned task behind `h` finishes.

#### `fn run(root: Task(m, Nil))`

Drive `root` (and everything it spawns) to completion on a deterministic round-robin schedule. An async `main` lowers to a single `run` of its body.

## `testing`

The witchy test support module. `witchy test <file>` discovers every zero-parameter function named `test_*`, runs each one, and reports it as passing unless it aborts — which these assertions do, with a message, via the `fail` primitive. Tests are pure functions: they take no capabilities, so a test suite provably has no effects.

  import testing

  fn test_addition():       testing.assert_eq("${2 + 2}", "4")

  fn test_truth():       testing.assert(1 < 2, "one is less than two")

#### `fn assert(cond: Bool, msg: String)`

Abort the test with `msg` unless `cond` holds.

#### `fn assert_eq(got: String, want: String)`

Abort unless the two strings are equal, showing both. Convert values with `to_string`/`int_to_string` at the call site — the message stays readable.

#### `fn assert_ne(got: String, other: String)`

Abort if the two strings ARE equal.

#### `fn assert_int_eq(got: Int, want: Int)`

Abort unless the two Ints are equal, showing both.

#### `fn fail_with(msg: String)`

Unconditional failure with a message (e.g. an unreachable branch).

## `time`

time — civil (UTC) date/time from a unix timestamp.

`std/duration` models *spans*; this module models *points* on the calendar. Given seconds since the unix epoch (1970-01-01T00:00:00Z), it computes the civil year/month/day/hour/minute/second and formats them. The conversions use the standard days<->civil algorithm (proleptic Gregorian), correct for any CE date and for negative timestamps (before 1970) via floor division.

#### `type DateTime`

- `DateTime(Int, Int, Int, Int, Int, Int)`

#### `fn year(d: DateTime) -> Int`

#### `fn month(d: DateTime) -> Int`

#### `fn day(d: DateTime) -> Int`

#### `fn hour(d: DateTime) -> Int`

#### `fn minute(d: DateTime) -> Int`

#### `fn second(d: DateTime) -> Int`

#### `fn from_millis(ms: Int) -> DateTime`

The civil UTC date/time at `ms` MILLISECONDS since the unix epoch — what `now(clock)` returns, so `time.from_millis(now(clock))` is the idiom for "the current date/time".

#### `fn from_unix(secs: Int) -> DateTime`

The civil UTC date/time at `secs` SECONDS since the unix epoch (a classic unix timestamp). `now(clock)` returns milliseconds — use `from_millis` for it, or this becomes the year 58000.

#### `fn to_unix(d: DateTime) -> Int`

The unix timestamp for a DateTime (its inverse).

#### `fn civil(y: Int, mo: Int, da: Int, h: Int, mi: Int, s: Int) -> Result(DateTime, String)`

A DateTime from civil UTC components, validated — `civil(2026, 2, 30, ...)` is an Err, not a rollover.

#### `fn days_in_month(y: Int, mo: Int) -> Int`

Days in a month, honoring leap February.

#### `fn parse_iso8601(text: String) -> Result(DateTime, String)`

Parse RFC 3339 / ISO 8601: `2026-06-08T22:30:00Z`, an offset like `+02:00` (normalized to UTC), fractional seconds (truncated), a space instead of the `T`, or a bare `YYYY-MM-DD` (midnight UTC).

#### `fn is_leap(y: Int) -> Bool`

--- calendar facts ----------------------------------------------------------

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

## `toml`

toml — a TOML reader written in pure witchy (no native code). Two ways in: `toml.decode(text)` parses a whole document into a structured `Toml` tree (the `json.decode` shape); or look individual values up by a `section.key` path with `toml.get`/`get_array`/`table`/... It supports top-level and `[section]` (and dotted `[a.b]`) tables, `key = value` for string/int/bool values, and `["a", "b"]` arrays. Comments (`#`) — whole-line and trailing — and blank lines are ignored. (Floats/dates decode as `TomlString`: witchy has no string->float primitive yet.)

#### `type Toml`

A decoded TOML value (`toml.decode`), the structured counterpart of the string-query API below. A document decodes to a `TomlTable`. Floats, dates, and other values witchy can't type are kept as `TomlString` (witchy has no string->float primitive yet), so a round-trip never loses data.

- `TomlString(String)`
- `TomlInt(Int)`
- `TomlBool(Bool)`
- `TomlArray(List(Toml))`
- `TomlTable(List((String, Toml)))`

#### `fn decode(text: String) -> Result(Toml, String)`

Parse a whole TOML document into a `Toml` tree (always a `TomlTable`). Supports top-level keys, `[section]` and dotted `[a.b]` tables, `#` comments, and `string`/`int`/`bool`/array values. Always succeeds — malformed lines are skipped — so the result is `Ok`; the `Result` shape mirrors `json.decode`.

#### `fn get(text: String, path: String) -> Option(String)`

The string value of `path` (e.g. "rune.name"), or None if absent. Surrounding double quotes are stripped.

#### `fn get_array(text: String, path: String) -> List(String)`

The string-array value of `path` (e.g. "capabilities.runtime"), or [] if absent. Each element is unquoted.

#### `fn table(text: String, section: String) -> List((String, String))`

Every `key = value` pair defined directly under `[section]`, in file order. Keys are unquoted (`"acme/money"` -> `acme/money`); values are raw (trimmed, still quoted/inline) — feed an inline-table value to `inline_get`. Use this to enumerate a table whose keys you don't know ahead of time, like `[dependencies]`.

#### `fn array_tables(text: String, name: String) -> List(String)`

Each `[[name]]` array-of-tables entry as its own block of body text — the lines after one `[[name]]` header up to the next header. Feed a block to `get` / `get_array` to read its fields. Use this to walk a `witchy.lock`'s `[[rune]]` entries, which `table`/`get` (single-table) cannot enumerate.

#### `fn keys(text: String, section: String) -> List(String)`

Just the keys of `[section]` (unquoted), in file order.

#### `fn inline_get(inline: String, key: String) -> Option(String)`

Read `key` from an inline table value like `{ path = "../money", version = "1" }`. Returns the unquoted value, or None if the key is absent.

## `url`

Minimal URL parsing — the witchy slice of Go's net/url. Pure and capability-free, so it compiles to WASM. Handles `scheme://host[:port][/path]`; the port defaults by scheme (443 for https, else 80) and the path to "/".

Structured parses return `Result(_, String)` with what went wrong (the same convention as `json.decode` and `semver.parse`); simple scalar parses like `string.parse_int` stay `Option`.

#### `type Url`

- `Url(String, String, Int, String)`

#### `fn parse(s: String) -> Result(Url, String)`

Parse a URL, or an error naming what is malformed.

#### `fn scheme(u: Url) -> String`

#### `fn host(u: Url) -> String`

#### `fn port(u: Url) -> Int`

#### `fn path(u: Url) -> String`

#### `fn format(u: Url) -> String`

Render a Url back to its string form — the inverse of `parse`. The port is shown only when it differs from the scheme default, so a parse/format round trip of `https://host/p` stays `https://host/p` rather than gaining `:443`.

#### `fn encode(s: String) -> String`

Percent-encode `s` for use as a query-string value (RFC 3986): the unreserved set (`A-Z a-z 0-9 - _ . ~`) passes through, every other byte becomes `%XX`. Used to build query strings safely — e.g. an OAuth `redirect_uri`, `scope`, or `state`.

## `webauthn`

webauthn — server-side verification of a WebAuthn *assertion* (the credential "get" / second-factor ceremony), in pure witchy. ES256 (P-256, COSE alg -7) only.

The browser hands every BINARY value to the server HEX-ENCODED (it holds them as ArrayBuffers, so this is free). The server then INDEPENDENTLY re-derives and checks everything that matters — it trusts none of the client's interpretation:   * clientDataJSON.type == "webauthn.get"   * clientDataJSON.challenge == the exact challenge the server issued (anti-replay)   * clientDataJSON.origin == the expected origin (anti-phishing)   * authenticatorData.rpIdHash == SHA-256(expected RP id) (wrong relying party)   * the user-presence (and, for 2FA, user-verification) flags are set   * the ECDSA-P256 signature over `authenticatorData || SHA256(clientDataJSON)`     verifies under the public key bound to this credential at registration. A forged or replayed assertion fails one of these and is rejected here.

#### `fn verify_assertion(stored_pubkey_hex: String, auth_data_hex: String, client_data_json: String, signature_hex: String, expected_challenge: String, expected_origin: String, expected_rp_id: String, require_uv: Bool) -> Result(Bool, String)`

Verify an assertion. All `*_hex` arguments are hex-encoded bytes; `client_data_json` is the exact clientDataJSON text the browser signed over (it must be re-hashed verbatim, never re-serialized). `require_uv` demands user verification — pass `true` for a genuine second-factor gate. Returns `Ok(true)` when every check passes, or `Err(reason)`.

