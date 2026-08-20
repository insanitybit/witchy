# Concurrency with Async and Channels

Most concurrency bugs are memory bugs wearing a hat: two threads reach the same
value and the schedule decides who wins. witchy's tasks share no memory, so
there's nothing to race over, and the schedule is deterministic, so a test that
passes once passes every time.

Concurrency here is **cooperative async tasks** and **channels**. An
`async` function may `await`; calling it produces a *task* that the executor
drives. Start one with `chan.spawn`; create channels with `chan.channel` and pass
them to the tasks that communicate. Tasks have separate memory, and the
single-threaded deterministic schedule produces the same output on the
interpreter and compiled WebAssembly backends.

## `async` and `await`

An `async fn` is a function that can suspend at an `await`. Awaiting another async
call runs it and yields its value. An `async fn main` is the program's entry into
the executor - it's driven to completion automatically.

```witchy
async fn double(n: Int) -> Int:
    n + n

async fn main(console: Console):
    let a = double(21).await
    console.print("doubled: ${a}")
```

## Spawning tasks

`chan.spawn` starts a task running concurrently and returns a handle; `chan.join`
waits for it while the executor can make progress. No channel is involved -
spawning is just concurrency. Each task yields control at `chan.yield_now().await`,
so the others get a turn - that's what interleaves their output. If every live
task parks with no progress, the executor runs its quiescence close pass; a
parked join resumes then, even if the joined task has a continuation that will
run afterward.

```witchy
import chan

async fn ticker(console: Console, name: String, n: Int):
    if n <= 0:
        console.print("${name} done")
    else:
        console.print("${name} ${n}")
        chan.yield_now().await
        ticker(console, name, n - 1).await

async fn main(console: Console):
    let a = chan.spawn(ticker(console, "A", 2)).await
    let b = chan.spawn(ticker(console, "B", 2)).await
    chan.join(a).await
    chan.join(b).await
```

## Channels: sending and receiving

`chan.channel(cap)` creates a channel and returns a `(Sender, Receiver)` pair -
two ends of the same conduit, which you pass to whichever tasks need them. A
bounded channel blocks the sender when it's full while some task can make
progress (backpressure); pass `0`, or use `chan.unbounded()`, for no limit. If
every live task parks with no progress, the executor runs a quiescence close pass:
parked receives resume as `None`, parked sends are released, and parked joins
resume. That is the close condition `chan.recv(rx).await` and `chan.consume` see;
witchy doesn't refcount sender values, so "closed" doesn't mean no `Sender`
value can ever be used again.

```witchy
from chan import Sender, Receiver

async fn source(tx: Sender(String)):
    chan.send(tx, "first").await
    chan.send(tx, "second").await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let source_handle = chan.spawn(source(tx)).await
    chan.consume(rx, fn(msg): chan.done(console.print("got: ${msg}"))).await
    chan.join(source_handle).await
```

The promoted bounded workflow in `examples/channels` uses a capacity of one,
so its four sends cannot all queue at once: the producer parks and the consumer
must drain the channel before it can continue. The installed bounded-channel
smoke test recreates that project under a fresh temporary home, places only a
copied public `witchy` binary on `PATH`, scrubs the inherited environment, and
checks the exact output. The release archive runs the same smoke against its
extracted binary. This is the workflow available after installation; it does
not import compiler sources, standard-library files, package state, or cache
state from a Witchy checkout.

## Request, reply, and stateful servers

A server is a task that owns a `Receiver` and loops on it. `chan.serve` writes that
loop and threads a piece of state through every message. A request that needs an
answer carries a reply `Sender` the client made - the reply comes back on a channel
the caller chose, with no shared addresses.

```witchy
from chan import Sender, Receiver

type Msg:
    Add(Int)
    Get(Sender(Msg))
    Total(Int)

async fn accumulator(inbox: Receiver(Msg)):
    chan.serve(inbox, 0, fn(sum, m):
        match m:
            Add(n) -> chan.done(sum + n)
            Get(reply) -> chan.and_then(chan.send(reply, Total(sum)), fn(own _u): chan.done(sum))
            Total(_t) -> chan.done(sum)
    ).await

async fn client(console: Console, srv: Sender(Msg)):
    chan.send(srv, Add(5)).await
    chan.send(srv, Add(2)).await
    let (reply_tx, reply_rx) = chan.channel(1).await
    chan.send(srv, Get(reply_tx)).await
    let r = chan.recv(reply_rx).await
    match r:
        Some(Total(t)) -> console.print("total is ${t}")
        Some(Add(_n)) -> console.print("(unreachable)")
        Some(Get(_s)) -> console.print("(unreachable)")
        None -> console.print("(no reply)")

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
worker pool (many receivers on one channel) - something a per-task mailbox can't
express. Results flow back on a second channel.

```witchy
from chan import Sender, Receiver

async fn worker(jobs: Receiver(Int), out: Sender(Int)):
    chan.consume(jobs, fn(n): chan.send(out, n * n)).await

async fn main(console: Console):
    let (jobs_tx, jobs_rx) = chan.channel(2).await
    let (out_tx, out_rx) = chan.channel(2).await
    let first_worker = chan.spawn(worker(jobs_rx, out_tx)).await
    let second_worker = chan.spawn(worker(jobs_rx, out_tx)).await
    for n in [3, 4, 5]:
        chan.send(jobs_tx, n).await
    chan.consume(out_rx, fn(r): chan.done(console.print("sq ${r}"))).await
    chan.join_all([move first_worker, move second_worker]).await
```

## Structured concurrency: the form to reach for first

A bare `chan.spawn` hands you a must-consume task handle. The checker requires
every path to `join` it, `cancel` it, return it, or transfer it into another
`own` operation; forgetting it or returning early is a compile-time error.
The `chan` module gives you a *structured* layer on top, where the tasks are
never visible and the join is guaranteed. Prefer these over a raw `spawn`:

- `chan.gather(tasks)` fans out result-producing tasks and returns every result
  once they all finish.
- `chan.par_map(items, f)` maps a task over a list concurrently and returns the
  results **in input order**, regardless of which finished first.
- `chan.par_reduce(items, f, init, combine)` folds those results.
- `chan.race(a, b)` returns the first result and cancels the loser.
- `chan.scope(tasks)` runs a batch of side-effecting tasks and joins them all.

No task handle escapes any of these, so nothing leaks. And because the executor
schedules deterministically - `par_map` reorders results back to input order, a
`race` tie favours the first racer - the run is byte-identical on both backends.

```witchy
import list
import option
from chan import Sender

async fn square(n: Int) -> Int:
    n * n

async fn announce(tx: Sender(Int), n: Int):
    chan.send(tx, n).await

async fn main(console: Console):
    let squares = chan.gather([square(1), square(2), square(3)]).await
    console.print("gathered: ${squares}")

    let mapped = chan.par_map([1, 2, 3, 4], fn(n): square(n)).await
    console.print("par_map: ${mapped}")

    let winner = chan.race(square(7), square(8)).await
    console.print("race: ${option.unwrap_or(winner, 0)}")

    let (tx, rx) = chan.channel(0).await
    chan.scope([announce(tx, 10), announce(tx, 20)]).await
    let a = chan.recv(rx).await
    let b = chan.recv(rx).await
    console.print("scoped: ${[a, b].length()} workers ran")
```

```text
gathered: [1, 4, 9]
par_map: [1, 4, 9, 16]
race: 49
scoped: 2 workers ran
```

Reach for a bare `spawn`/`join` (or the worker-pool pattern above) only when you
need a long-lived task whose lifetime genuinely outlives a single scope - a
server loop, say. For fan-out-and-collect, the structured combinators are the
idiom.

## Iterating with `await`

A `for` loop may `await` in its body. Each iteration runs to completion before the
next begins, so a batch of asynchronous steps reads as an ordinary loop - that's
how `source` above sends several messages with `for n in [...]`.

`for await x in rx:` is the receiver form: it loops over a channel, binding each
message in turn and stopping when the channel closes - and its body may `await`
too, so a stage can receive, transform, and forward in a few plain lines:

```witchy
from chan import Sender, Receiver

async fn producer(tx: Sender(Int)):
    for n in [1, 2, 3]:
        chan.send(tx, n).await

async fn squares(rx: Receiver(Int), out: Sender(Int)):
    for await n in rx:
        chan.send(out, n * n).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let (out_tx, out_rx) = chan.channel(4).await
    let producer_handle = chan.spawn(producer(tx)).await
    let squares_handle = chan.spawn(squares(rx, out_tx)).await
    chan.consume(out_rx, fn(v): chan.done(console.print("got ${v}"))).await
    chan.join(producer_handle).await
    chan.join(squares_handle).await
```

A `while` loop may `await` in its body too, and a `var` local may cross an
`await` - the compiler lowers each `async fn` to a resumable state machine, so
evolving state rides the machine rather than a captured-by-value closure. That
also lets a `for await` loop **fold**: mutate an accumulator each message and read
the total once the channel closes.

```witchy
from chan import Sender, Receiver

async fn counter(tx: Sender(Int), n: Int):
    var i = 0
    while i < n:
        chan.send(tx, i).await
        i = i + 1

async fn total(console: Console, rx: Receiver(Int)):
    var sum = 0
    for await v in rx:
        sum = sum + v
    console.print("sum is ${sum}")

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let counter_handle = chan.spawn(counter(tx, 5)).await
    total(console, rx).await
    chan.join(counter_handle).await
```

```text
sum is 10
```

`counter` drives its `while` with a `var i` that it mutates after each `await`,
and `total` folds the stream into `var sum` (0 + 1 + 2 + 3 + 4 = 10).

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
    console.print("${r}")
```

```text
110
```

One restriction: an `async fn` may not be a *trait* method (neither declared in a
`trait` nor implementing one in an `impl Trait for T`) - the compiler rejects it
at parse time. A trait that wants an asynchronous operation declares a plain
`fn … -> Task(a)`; the implementing method inherits the trait's return type
(no need to repeat the annotation) and delegates to an inherent async method.

```witchy
from task import Task

trait Fetcher:
    fn fetch(self, url: String) -> Task(String)

type FakeFetcher:
    prefix: String

impl FakeFetcher:
    async fn get(self, url: String) -> String:
        "${self.prefix}: ${url}"

impl Fetcher for FakeFetcher:
    fn fetch(self, url: String):
        self.get(url)

async fn main(console: Console):
    let f = FakeFetcher("fetched")
    let body = f.fetch("/docs").await
    console.print(body)
```

```text
fetched: /docs
```

Calling `f.fetch(...)` returns the `Task(String)` unstarted; the caller decides
when to `await` it. The inherent `async fn get` is where the suspension actually
lives - the trait method is just a synchronous function that happens to return a
task.

## The bounded-memory contract

Channels provide deterministic scheduling and backpressure. `unbounded`
describes a channel's logical capacity; it does not promise infinite process
memory.

For a qualifying scalar producer/consumer loop, the compiled optimizer
synthesizes an owning state machine with fixed task, channel, and message slots.
Resuming the loop retags those slots in place instead of allocating a new
continuation. The RFC acceptance gate executes one million messages, checks the
result, rejects per-resume linear allocation, and enforces the documented
latency ceiling. The general GC-backed scheduler likewise reuses task slots and
eligible direct-list cells; its high-water probe checks that Wasmtime's backing
capacity does not grow with the message count.

The supported-preview promise covers the bounded workflows and promoted
compiled shapes exercised by that matrix. Payloads or control flow that fall
back to a more general representation keep the same deterministic semantics,
but they do not silently inherit a flat-memory or latency claim. Use a bounded
channel when the application needs an explicit storage bound, and measure the
actual payload shape before treating a channel as an indefinitely running
service queue.

## Deterministic scheduling

The executor is ordinary witchy code (see `std/task`): it owns the channel buffers
and polls tasks in a fixed round-robin order. `std/chan` is the ergonomic channel
surface over that task executor. No scheduler state lives in the
runtime, no operating-system threads are involved, and nothing is shared mutably -
so the interleaving is identical on both backends. Each channel carries its own
message type. The executor moves one opaque message envelope, and the checked
endpoint type recovers the envelope's i32, i64, f64, host-reference, or direct
GC-reference field; references are never bit-cast through an integer slot. A
spawned task returns `()` (it reports results by sending them),
which is what keeps the whole executor expressible in pure witchy.
