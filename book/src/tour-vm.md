# Multi-Core and Isolated Workers

The [`chan`/`task` model](tour-async.md) gives you *concurrency* — many tasks
interleaving on one core, cooperatively. The `vm` module gives you the other two
things a program eventually wants: **parallelism** (use every core) and **isolation**
(run code in a separate sandbox with exactly the authority you choose).

Everything here preserves witchy's prime directive — the interpreter and the compiled
backend produce identical output. That is not a happy accident; it *shaped* the
surface, as you'll see.

## `vm.par_map`: use every core

`vm.par_map(xs, f)` maps a function over a list with the elements processed in
parallel — on the compiled backend, across one worker VM per core, each its own
isolated WebAssembly instance.

```witchy
import vm
import list

fn square(n: Int) -> Int:
    n * n

fn main(console: Console):
    let squares = vm.par_map([1, 2, 3, 4], square)
    console.print("${list.sum(squares)}")
```

Why is this safe to run in parallel when so much of witchy's speed comes from "one
owner, no other observer"? Because the result is collected **by input index** and `f`
is a pure function: the parallel answer is *identical* to the sequential one. So the
interpreter runs it sequentially, the compiled backend runs it across cores, and they
agree. Parallelism changes how *fast* the map runs, not *what* it returns.

Two rules keep it sound, both checked at compile time (anything else runs the ordinary
sequential body):

- The element type must be **flat** — a scalar (`Int`/`Bool`/`Float`), a `String`, or
  `Bytes`. A flat value (`[length][bytes…]`, no internal pointers) copies to a worker
  by a plain byte copy. A pointer-bearing value like `List(String)` would carry
  addresses meaningless in another VM's memory, so it stays sequential.
- `f` must be a **top-level function** (capture-free): a worker has its own memory, so
  a captured parent-heap value would not be reachable there.

On the benchmark suite this lands within ~25% of Go goroutines on CPU-bound work — the
gap is the per-call cost of spinning up worker instances.

## `Bytes`: the binary payload

Crossing a VM boundary, or serializing anything structured, wants a flat byte buffer.
That's the `Bytes` type ([`std/bytes`](stdlib.md)) — a UTF-8-free sequence of bytes,
the thing `String` (always valid UTF-8) can't faithfully hold:

```witchy
import bytes

fn main(console: Console):
    let b = bytes.from_string("hi")
    console.print("${bytes.length(b)}")
    console.print("${bytes.at(b, 0)}")
```

Structured values cross a VM or wire boundary by choosing an explicit encoding into
`Bytes`. A `packed` record is a local layout/performance contract today, not a
worker-VM wire format; once a value leaves its VM, `Bytes` is the dependable boundary.

## `vm.with_dir`: a worker with exactly the authority you pass

`vm.with_dir(dir, f, input)` runs `f` in an isolated worker VM granted **exactly** the
one directory capability `dir` — and nothing else. The worker can read and write within
`dir` (with `dir`'s own rights) and reach no other host resource: every ungranted
capability simply traps. It is the sandbox for running partially-trusted code with
precisely scoped authority.

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

It is deliberately **lock-step** — the worker processes one request at a time, in
order, rather than racing the caller. That is the interesting part: a *freely-racing*
cross-VM channel would be nondeterministic (the interleaving depends on timing), and a
single-threaded interpreter could never reproduce it bit-for-bit. So a free-racing
channel is *incompatible* with witchy's parity guarantee. Lock-step serving is not a
weaker compromise — it is the correct shape for a language that promises two backends,
one meaning.

## A multi-core HTTP server, for free

The same prefork idea gives you a parallel web server with **no extra code**.
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

That's a capability-secure, multi-core HTTP server. The handlers are still pure
`fn(Request) -> Response` — they hold no `Net`, so a handler can't phone home — and
their state lives in their captured capabilities (a store `Dir` = the filesystem), so
the workers are interchangeable. You write the routes; the parallelism is automatic.
Reach for `server.serve_one` if you want a single-core loop (e.g. for per-process
in-memory state). witchy's own package registry, `coven`, runs exactly this way.

## When to reach for which

- **`vm.par_map`** — you have a list and a pure function and you want it *fast*.
- **`vm.with_dir`** — you want to run code with *less* authority than you hold.
- **`vm.serve`** — you want a stateful worker processing a stream, isolated.
- **`chan`/`task`** — you want cooperative concurrency (overlapped I/O, structured
  task groups) within one VM; see [the async chapter](tour-async.md).
