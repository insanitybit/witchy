# Multi-Core and Isolated Workers

The [`chan`/`task` model](tour-async.md) provides cooperative concurrency within
one VM. The `vm` module provides **parallelism** across cores and **isolation** in
a separate sandbox with explicitly chosen authority.

These APIs preserve backend parity by fixing result order and rejecting callback
shapes that cannot cross an isolated VM boundary.

> The online book runs these examples in fresh, zero-authority WebAssembly
> instances, driven sequentially. Native compiled programs fan eligible maps
> across cores; the browser preserves the same ordered result and isolation
> without claiming parallel speedup.

## `vm.par_map`: use every core

`vm.par_map(xs, f)` maps a function over a list with the elements processed in
parallel on the native compiled backend, across one worker VM per core. The
browser creates isolated WebAssembly instances too, but drives them sequentially.

```witchy
import vm
import list

fn square(n: Int) -> Int:
    n * n

fn main(console: Console):
    let squares = vm.par_map([1, 2, 3, 4], square)
    console.print("${list.sum(squares)}")
```

The result is collected **by input index** and `f` is pure. The
interpreter runs it sequentially, the compiled backend runs it across cores, and they
produce the same ordered result.

Two rules select the parallel fast path. Anything else runs the ordinary sequential
body with the same ordered-map semantics:

- The element type must be **flat** — a scalar (`Int`/`Bool`/`Float`), a `String`, or
  `Bytes`. A flat value (`[length][bytes…]`, no internal pointers) copies to a worker
  by a plain byte copy. A pointer-bearing value like `List(String)` would carry
  addresses meaningless in another VM's memory, so it stays sequential.
- `f` must be named directly as a **top-level function** (capture-free): a worker has
  its own memory, so a captured parent-heap value would not be reachable there. A
  local function value or lambda remains valid, but runs sequentially.

Worker startup adds a per-call cost, so native `vm.par_map` is intended for
substantial CPU-bound elements rather than tiny callbacks. Browser execution is
the portable semantic path, not a performance claim.

## `Bytes`: the binary payload

Crossing a VM boundary, or serializing anything structured, wants a flat byte buffer.
The `Bytes` type ([`std/bytes`](appendix-stdlib.md)) is a UTF-8-free sequence of bytes,
the thing `String` (always valid UTF-8) can't faithfully hold:

```witchy
import bytes

fn main(console: Console):
    let b = bytes.from_string("hi")
    console.print("${b.length()}")
    console.print("${b.at(0)}")
```

Structured values cross a VM or wire boundary by choosing an explicit encoding into
`Bytes`. A `packed` record is a local layout/performance contract today, not a
worker-VM wire format; once a value leaves its VM, `Bytes` is the dependable boundary.

## `vm.with_dir`: a worker with exactly the authority you pass

`vm.with_dir(dir, f, input)` runs `f` in an isolated worker VM granted **exactly** the
one directory capability `dir` — and nothing else. The worker can read and write within
`dir` (with `dir`'s own rights) and reach no other host resource. An attempt to use
ungranted authority traps. This is the sandbox for running partially-trusted code
with precisely scoped authority.

```sh
fn untrusted(d: Dir, name: Bytes) -> Bytes:
    bytes.from_string(d.read(bytes.to_string(name)))

fn main(dir: Dir):
    # `untrusted` runs in its own VM that can ONLY touch `dir`.
    let contents = vm.with_dir(dir, untrusted, bytes.from_string("config.txt"))
    ...
```

Because the output is a deterministic function of the directory's contents and the
input, the isolation is a security property invisible to the *result* — so the two
backends still agree.

The callback must be a bare top-level function name. A closure or local function
alias is rejected at compile time: `vm.with_dir` never silently substitutes a direct
parent-VM call for its promised isolation boundary.

## `vm.serve`: a cross-VM channel as a stateful service

The last shape is a worker that stays alive and processes a *stream* of messages while
keeping state — a service, an actor. `vm.serve(init, requests, handler)` runs that
service on one long-lived isolated worker VM: it processes `requests` in order,
threading an accumulator through `handler(state, request) -> new_state`, and emits each
new state as that request's response.

```witchy
import vm
import bytes

fn step(state: Bytes, req: Bytes) -> Bytes:
    bytes.concat(state, req)

fn main(console: Console):
    let reqs = [bytes.from_string("a"), bytes.from_string("b")]
    let outs = vm.serve(bytes.from_string(""), reqs, step)
    for o in outs:
        console.print(bytes.to_string(o))
```

This prints `a` then `ab`: the state accumulates across the stream.

As with `vm.with_dir`, the handler must be a bare top-level function name. Closures
and local aliases are rejected, so this API always means an isolated worker on the
compiled backend rather than a shape-dependent parent-VM fallback.

It is deliberately **lock-step**: the worker processes one request at a time, in
order. A freely racing cross-VM channel would make interleaving timing-dependent,
which the single-threaded interpreter could not reproduce. Lock-step serving keeps
the result identical across both backends.

## A multi-core HTTP server

The same prefork model provides a parallel web server.
`server.serve(net, addr, app)` spawns one worker VM per core, each re-running your
program to build the same routes with the same capabilities, all accepting from one
shared listener — the kernel load-balances connections across them.

```sh
import server

fn home(req: Request) -> Response:
    server.text(200, "hello, witchy")

fn main(net: Net, console: Console):
    let app = server.router().get("/", home)
    server.serve(net, "127.0.0.1:8080", app)   # uses every core
```

The handlers remain pure `fn(Request) -> Response` values. Their state lives in
captured capabilities (a store `Dir` represents the filesystem), so the workers
are interchangeable. The routes determine the application; the server supplies
the parallel workers.
Reach for `server.serve_one` if you want a single-core loop (e.g. for per-process
in-memory state). witchy's own package registry, `coven`, runs exactly this way.

Use `vm.par_map` for a list and a pure function; use `vm.with_dir` to run code
with less authority than the caller; use `vm.serve` for an isolated stateful
worker. Use `chan`/`task` for cooperative concurrency within one VM; see [the
async chapter](tour-async.md).
