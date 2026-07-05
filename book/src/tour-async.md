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
    let a = double(21).await
    print(console, "doubled: " + "${a}")
```

## Spawning tasks

`chan.spawn` starts a task running concurrently and returns a handle; `chan.join`
waits for it to finish. No channel is involved — spawning is just concurrency.
Each task yields control at `chan.yield_now().await`, so the others get a turn —
that is what interleaves their output.

```witchy
import chan

async fn ticker(console: Console, name: String, n: Int) -> Nil:
    if n <= 0:
        print(console, name + " done")
    else:
        print(console, "${name} ${n}")
        chan.yield_now().await
        ticker(console, name, n - 1).await

async fn main(console: Console):
    let a = chan.spawn(ticker(console, "A", 2)).await
    let b = chan.spawn(ticker(console, "B", 2)).await
    chan.join(a).await
    chan.join(b).await
```

## Channels: sending and receiving

`chan.channel(cap)` creates a channel and returns a `(Sender, Receiver)` pair —
two ends of the same conduit, which you pass to whichever tasks need them. A
bounded channel blocks the sender when it is full (backpressure); pass `0`, or use
`chan.unbounded()`, for no limit. `chan.recv(rx).await` yields the next message, or
`None` once the channel is closed — which happens automatically when no task can
send to it anymore. `chan.consume` writes that receive-until-closed loop for you.

```witchy
import chan
from chan import Sender, Receiver

async fn source(tx: Sender(String)) -> Nil:
    chan.send(tx, "first").await
    chan.send(tx, "second").await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.spawn(source(tx)).await
    chan.consume(rx, fn(msg): chan.done(print(console, "got: " + msg))).await
```

## Request, reply, and stateful servers

A server is a task that owns a `Receiver` and loops on it. `chan.serve` writes that
loop and threads a piece of state through every message. A request that needs an
answer carries a reply `Sender` the client made — the reply comes back on a channel
the caller chose, with no shared addresses.

```witchy
import chan
from chan import Sender, Receiver

type Msg:
    Add(Int)
    Get(Sender(Msg))
    Total(Int)

async fn accumulator(inbox: Receiver(Msg)) -> Nil:
    chan.serve(inbox, 0, fn(sum, m):
        match m:
            Add(n) -> chan.done(sum + n)
            Get(reply) -> chan.and_then(chan.send(reply, Total(sum)), fn(_u): chan.done(sum))
            Total(_t) -> chan.done(sum)
    ).await

async fn client(console: Console, srv: Sender(Msg)) -> Nil:
    chan.send(srv, Add(5)).await
    chan.send(srv, Add(2)).await
    let (reply_tx, reply_rx) = chan.channel(1).await
    chan.send(srv, Get(reply_tx)).await
    let r = chan.recv(reply_rx).await
    match r:
        Some(Total(t)) -> print(console, "total is " + "${t}")
        Some(Add(_n)) -> print(console, "(unreachable)")
        Some(Get(_s)) -> print(console, "(unreachable)")
        None -> print(console, "(no reply)")

async fn main(console: Console):
    let (srv_tx, srv_rx) = chan.channel(8).await
    let h = chan.spawn(accumulator(srv_rx)).await
    client(console, srv_tx).await
    chan.join(h).await
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
from chan import Sender, Receiver

async fn worker(jobs: Receiver(Int), out: Sender(Int)) -> Nil:
    chan.consume(jobs, fn(n): chan.send(out, n * n)).await

async fn main(console: Console):
    let (jobs_tx, jobs_rx) = chan.channel(2).await
    let (out_tx, out_rx) = chan.channel(2).await
    chan.spawn(worker(jobs_rx, out_tx)).await
    chan.spawn(worker(jobs_rx, out_tx)).await
    for n in [3, 4, 5]:
        chan.send(jobs_tx, n).await
    chan.consume(out_rx, fn(r): chan.done(print(console, "sq ${r}"))).await
```

## Iterating with `await`

A `for` loop may `await` in its body. Each iteration runs to completion before the
next begins, so a batch of asynchronous steps reads as an ordinary loop — that is
how `source` above sends several messages with `for n in [...]`.

`for await x in rx:` is the receiver form: it loops over a channel, binding each
message in turn and stopping when the channel closes — and its body may `await`
too, so a stage can receive, transform, and forward in a few plain lines:

```witchy
import chan
from chan import Sender, Receiver

async fn producer(tx: Sender(Int)) -> Nil:
    for n in [1, 2, 3]:
        chan.send(tx, n).await

async fn squares(rx: Receiver(Int), out: Sender(Int)) -> Nil:
    for await n in rx:
        chan.send(out, n * n).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let (out_tx, out_rx) = chan.channel(4).await
    chan.spawn(producer(tx)).await
    chan.spawn(squares(rx, out_tx)).await
    chan.consume(out_rx, fn(v): chan.done(print(console, "got ${v}"))).await
```

The list form lowers to `task.for_each`, the receiver form to `chan.consume`. A
`while` loop cannot `await` (it would need mutable state carried across the point, which captured-by-value closures can't express) — for an open-ended loop,
recurse with an async fn, or use `for await`. The same rule explains a subtler
limit: the code *after* an `await` becomes a captured-by-value continuation, so a
`var` declared before an `await` can't be mutated after it. Carry evolving state by
recursing with an `async fn`, or thread it through a channel (`chan.serve`), rather
than in a `var`.

## Async methods

An `async fn` can also be a **method** in an inherent `impl` block. Calling it
returns a task like any other async call, and the caller `await`s it; the body
reads the receiver's fields through `self` and may itself `await`:

```witchy
type Doubler:
    base: Int

async fn step(n: Int) -> Int:
    n + n

impl Doubler:
    async fn scaled(self, x: Int) -> Int:
        let doubled = step(x).await
        self.base + doubled

async fn main(console: Console):
    let d = Doubler(100)
    let r = d.scaled(5).await
    print(console, "${r}")
```

```text
110
```

One restriction: an `async fn` may not be a *trait* method (neither declared in a
`trait` nor implementing one in an `impl Trait for T`) — the compiler rejects it
at parse time. A trait that wants an asynchronous operation declares a plain
`fn … -> Task(m, a)`; the implementing method leaves its own return type to
inference and delegates to an inherent async method.

## Why this stays deterministic

The executor is ordinary witchy code (see `std/chan`): it owns the channel buffers
and polls tasks in a fixed round-robin order. No scheduler state lives in the
runtime, no operating-system threads are involved, and nothing is shared mutably —
so the interleaving is identical on both backends. Each channel carries its own
message type (the executor moves messages erased and every endpoint recovers its
type), and a spawned task returns `Nil` (it reports results by sending them),
which is what keeps the whole executor expressible in pure witchy.
