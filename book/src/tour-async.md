# Concurrency with Async and Channels

witchy's concurrency is **cooperative async tasks** that communicate over
**channels**. A function marked `async` may `await`; calling it produces a *task*
that does nothing until an executor drives it. Tasks share no memory — they pass
messages — so there are no locks and no data races. The schedule is deterministic
and single-threaded, so a concurrent program produces the same output on the
interpreter and the compiled WebAssembly, every run.

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

`await` may appear as the whole right-hand side of a `let`, as a statement, or in
tail position. (Awaiting inside a loop or nested in a larger expression is not yet
supported.)

## Running tasks together

`chan.run` takes a list of tasks and drives them concurrently with a deterministic
round-robin schedule. Each task yields control at `await chan.yield_now()`, so the
others get a turn — that is what interleaves their output.

```witchy
import chan

async fn ticker(console: Console, name: String, n: Int) -> Nil:
    if n <= 0:
        print(console, name + " done")
    else:
        print(console, name + " " + "${n}")
        await chan.yield_now()
        await ticker(console, name, n - 1)

fn main(console: Console):
    chan.run([ticker(console, "A", 2), ticker(console, "B", 2)])
```

## Channels: sending and receiving

Every task in a `chan.run` has its own **inbox**. `chan.send(target, msg)` routes a
message to the inbox of task `#target` (its index in the list); `chan.recv()` reads
the current task's own inbox, suspending until a message arrives. Messages are of
any single type you choose.

A task that loops on `recv` is an **actor** — its state lives in a recursive
parameter, so nothing outside can touch it:

```witchy
import chan

async fn printer(console: Console) -> Nil:
    let msg = await chan.recv()
    print(console, "got: " + msg)
    await printer(console)

async fn source() -> Nil:
    await chan.send(0, "first")
    await chan.send(0, "second")

fn main(console: Console):
    chan.run([printer(console), source()])
```

The `printer` (task `#0`) receives the two messages the `source` (task `#1`) sends
to it, and prints them in order.

## Request and reply

There is no special `ask` operation: a request that needs an answer is just a
message, and the reply is a message sent back to the asker's inbox. Use a sum type
to carry both directions.

```witchy
import chan

type Msg:
    Get
    Total(Int)

async fn accumulator(sum: Int) -> Nil:
    let m = await chan.recv()
    match m:
        Get ->
            await chan.send(1, Total(sum))
            await accumulator(sum)
        Total(_t) -> await accumulator(sum)

async fn client(console: Console) -> Nil:
    await chan.send(0, Get)
    let r = await chan.recv()
    match r:
        Total(t) -> print(console, "total is " + "${t}")
        Get -> print(console, "(unreachable)")

fn main(console: Console):
    chan.run([accumulator(7), client(console)])
```

The `client` (`#1`) asks the `accumulator` (`#0`) for its running total; the
accumulator replies on the client's inbox, and the client prints it.

## The actor loop as a combinator: `chan.serve`

Every actor above ends each branch with the same `await loop(next_state)` call —
the recursion that carries state to the next message. `chan.serve` captures exactly
that shape: give it the initial state and a handler, and it receives a message,
runs the handler to get the next state, and repeats. The handler returns the next
state *as a task*, so it can also send replies before carrying state forward.

```witchy
import chan

type Msg:
    Add(Int)
    Get
    Total(Int)

async fn accumulator() -> Nil:
    await chan.serve(0, fn(sum, m):
        match m:
            Add(n) -> chan.done(sum + n)
            Get -> chan.and_then(chan.send(1, Total(sum)), fn(_u): chan.done(sum))
            Total(_t) -> chan.done(sum)
    )

async fn client(console: Console) -> Nil:
    await chan.send(0, Add(5))
    await chan.send(0, Add(2))
    await chan.send(0, Get)
    let r = await chan.recv()
    match r:
        Total(t) -> print(console, "total is " + "${t}")
        Add(_n) -> print(console, "(unreachable)")
        Get -> print(console, "(unreachable)")

fn main(console: Console):
    chan.run([accumulator(), client(console)])
```

This is the same accumulator as the previous section, but the state recursion lives
once inside `serve` instead of in every match arm. Like any actor, it runs until
quiescence — when nothing more arrives in its inbox, the task goes inert.

## Iterating with `await`

A `for` loop may `await` in its body. Each iteration runs to completion before the
next begins, so a sequence of asynchronous steps reads as an ordinary loop — no
hand-written recursion:

```witchy
import chan

async fn sender() -> Nil:
    for x in [1, 2, 3]:
        await chan.send(1, x)
    await chan.send(1, 0)

async fn receiver(console: Console) -> Nil:
    for _i in 0..4:
        let v = await chan.recv()
        print(console, "got ${v}")

fn main(console: Console):
    chan.run([sender(), receiver(console)])
```

The loop lowers to `chan.for_each`. A `while` loop cannot `await`: it would need
mutable state carried across the await point, which captured-by-value closures
can't express — for an open-ended loop, recurse with an async fn (as the actors
above do), or iterate a list with `for`.

## Why this stays deterministic

The executor is ordinary witchy code (see `std/chan` and `std/future`): it owns the
inboxes and polls tasks in a fixed order. No scheduler state lives in the runtime,
no operating-system threads are involved, and nothing is shared mutably — so the
interleaving is identical on both backends. When real parallelism is added, it will
be opt-in and built on the same message-passing model, so single-threaded runs stay
reproducible for testing.
