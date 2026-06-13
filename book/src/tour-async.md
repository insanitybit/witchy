# Concurrency with Async and Channels

witchy's concurrency is **cooperative async tasks** that communicate over
**channels**. A function marked `async` may `await`; calling it produces a *task*
that does nothing until it is driven. You start a task with `chan.spawn`, and a
channel is an ordinary value you create with `chan.channel` and hand to whichever
tasks share it — spawning and channels are *independent* concerns. Tasks share no
memory, so there are no locks and no data races, and the schedule is deterministic
and single-threaded, so a program produces the same output on the interpreter and
the compiled WebAssembly, every run.

## `async` and `await`

An `async fn` is a function that can suspend at an `await`. Awaiting another async
call runs it and yields its value. An `async fn main` is the program's entry into
the executor — it is driven to completion automatically.

```witchy
async fn double(n: Int) -> Int:
    n + n

async fn main(console: Console):
    let a = await double(21)
    print(console, "doubled: " + "${a}")
```

## Spawning tasks

`chan.spawn` starts a task running concurrently and returns a handle; `chan.join`
waits for it to finish. No channel is involved — spawning is just concurrency.
Each task yields control at `await chan.yield_now()`, so the others get a turn —
that is what interleaves their output.

```witchy
import chan

async fn ticker(console: Console, name: String, n: Int) -> Nil:
    if n <= 0:
        print(console, name + " done")
    else:
        print(console, "${name} ${n}")
        await chan.yield_now()
        await ticker(console, name, n - 1)

async fn main(console: Console):
    let a = await chan.spawn(ticker(console, "A", 2))
    let b = await chan.spawn(ticker(console, "B", 2))
    await chan.join(a)
    await chan.join(b)
```

## Channels: sending and receiving

`chan.channel(cap)` creates a channel and returns a `(Sender, Receiver)` pair —
two ends of the same conduit, which you pass to whichever tasks need them. A
bounded channel blocks the sender when it is full (backpressure); pass `0`, or use
`chan.unbounded()`, for no limit. `await chan.recv(rx)` yields the next message, or
`None` once the channel is closed — which happens automatically when no task can
send to it anymore. `chan.consume` writes that receive-until-closed loop for you.

```witchy
import chan

async fn source(tx: Sender(String)) -> Nil:
    await chan.send(tx, "first")
    await chan.send(tx, "second")

async fn main(console: Console):
    let (tx, rx) = await chan.channel(4)
    await chan.spawn(source(tx))
    await chan.consume(rx, fn(msg): chan.done(print(console, "got: " + msg)))
```

## Request, reply, and stateful servers

A server is a task that owns a `Receiver` and loops on it. `chan.serve` writes that
loop and threads a piece of state through every message. A request that needs an
answer carries a reply `Sender` the client made — the reply comes back on a channel
the caller chose, with no shared addresses.

```witchy
import chan

type Msg:
    Add(Int)
    Get(Sender(Msg))
    Total(Int)

async fn accumulator(inbox: Receiver(Msg)) -> Nil:
    await chan.serve(inbox, 0, fn(sum, m):
        match m:
            Add(n) -> chan.done(sum + n)
            Get(reply) -> chan.and_then(chan.send(reply, Total(sum)), fn(_u): chan.done(sum))
            Total(_t) -> chan.done(sum)
    )

async fn client(console: Console, srv: Sender(Msg)) -> Nil:
    await chan.send(srv, Add(5))
    await chan.send(srv, Add(2))
    let (reply_tx, reply_rx) = await chan.channel(1)
    await chan.send(srv, Get(reply_tx))
    let r = await chan.recv(reply_rx)
    match r:
        Some(Total(t)) -> print(console, "total is " + "${t}")
        Some(Add(_n)) -> print(console, "(unreachable)")
        Some(Get(_s)) -> print(console, "(unreachable)")
        None -> print(console, "(no reply)")

async fn main(console: Console):
    let (srv_tx, srv_rx) = await chan.channel(8)
    let h = await chan.spawn(accumulator(srv_rx))
    await client(console, srv_tx)
    await chan.join(h)
```

The handler returns the next state *as a task*, so the `Get` arm can send a reply
before carrying `sum` forward. The server runs until its channel closes, then the
`join` completes.

## A worker pool

Because a `Receiver` is an ordinary value, you can hand the *same* one to several
tasks: they share a queue, and whoever is free takes the next message. That is a
worker pool (many receivers on one channel) — something a per-task mailbox cannot
express. Results flow back on a second channel.

```witchy
import chan

async fn worker(jobs: Receiver(Int), out: Sender(Int)) -> Nil:
    await chan.consume(jobs, fn(n): chan.send(out, n * n))

async fn main(console: Console):
    let (jobs_tx, jobs_rx) = await chan.channel(2)
    let (out_tx, out_rx) = await chan.channel(2)
    await chan.spawn(worker(jobs_rx, out_tx))
    await chan.spawn(worker(jobs_rx, out_tx))
    for n in [3, 4, 5]:
        await chan.send(jobs_tx, n)
    await chan.consume(out_rx, fn(r): chan.done(print(console, "sq ${r}")))
```

## Iterating with `await`

A `for` loop may `await` in its body. Each iteration runs to completion before the
next begins, so a batch of asynchronous steps reads as an ordinary loop — that is
how `source`/`worker` above send several messages. The loop lowers to
`chan.for_each`. A `while` loop cannot `await` (it would need mutable state carried
across the await point, which captured-by-value closures can't express) — for an
open-ended loop, recurse with an async fn or loop on a receiver with
`chan.consume`/`chan.serve`.

## Why this stays deterministic

The executor is ordinary witchy code (see `std/chan`): it owns the channel buffers
and polls tasks in a fixed round-robin order. No scheduler state lives in the
runtime, no operating-system threads are involved, and nothing is shared mutably —
so the interleaving is identical on both backends. One message type flows through a
program's channels, and a spawned task returns `Nil` (it reports results by sending
them), which is what keeps the whole executor expressible in pure witchy.
