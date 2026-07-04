# Introduction

## The problem witchy is about

Most of the common programming languages you use grant "ambient authority" - any function can do just about any action. If I call `foo()` in Python, I have
no idea if `foo()` scoops up my env vars, reads `~/.ssh/`, sends it off to a
remote server, etc. This extends into the package management systems for these languages as well - installation scripts execute with arbitrary user permissions and often it's "all or nothing" grants.

Witchy aims at solving this area of problems while providing an overall nice language for development. Witchy programs aim to be best in class with regards to safety, but also ergonomics.

When writing or executing a witchy program you are always able to reason about what your code *can* do. The entry point of every program declares its maximum capabilities via its parameter list:

```witchy
// This function can read a file. You can see that. It cannot write, connect to
// the network, or read the clock — there is no parameter that would let it.
fn first_line(dir: Dir[Read], name: String) -> String:
    let contents = read(dir, name)
    string.split_once(contents, "\n").0

fn main(console: Console, dir: Dir[Read]):
    print(console, first_line(dir, "notes.txt"))
```

This program can write to the console and read whatever directory was provided to it.

The `Console` capability is implicitly provided to programs but filesystem access must be granted explicitly.

`witchy sandbox --dir . main.witchy`

This sort of explicit capability approach is present throughout the witchy language. Consumers of witchy binaries grant rights to that execution, developers of witchy programs grant rights to their dependencies, etc.

**You can audit by reading signatures.** `witchy caps program.witchy` walks the
program and reports its complete capability footprint, computed from the source,
broken down per right (`Dir[Read]` vs `Dir[Write]`, `Net[Connect]` vs
`Net[Listen]`). It is never self-asserted metadata that could drift or lie.

**You can gate on growth.** `witchy caps-diff old new` fails when authority
widened. Put it in CI and a dependency cannot quietly start listening on a
socket between versions. The package manager applies the same gate to the runes
(packages) you depend on.

**You can enforce at runtime.** `witchy sandbox program.witchy` compiles to
WebAssembly and runs it in a VM that has been handed *exactly* the host
functions its footprint calls for — and nothing else physically exists for it
to call.

## A taste of the language

Capabilities are the point, but the rest of the language is meant to be a pleasure
to write. Here is a tiny in-process server: an `async` task that owns a channel,
folds the messages it receives into running state, and answers a request — the
request/reply shape you'd normally reach for a socket, here in pure, deterministic
witchy that needs nothing but `Console`.

```witchy
import chan
import json

// This program's channels all carry one message type; a program may also use
// channels of several different types.
type Msg:
    Reading(Int)
    Report(Sender(Msg))
    Summary(Int, Int)

// A tiny stateful server: it folds incoming readings into a running
// (count, total) and answers a Report by sending the totals back.
async fn server(inbox: Receiver(Msg)) -> Nil:
    chan.serve(inbox, (0, 0), fn(state: (Int, Int), m):
        match m:
            Reading(v) -> chan.done((state.0 + 1, state.1 + v))
            Report(reply) -> chan.and_then(chan.send(reply, Summary(state.0, state.1)), fn(_u): chan.done(state))
            Summary(_c, _t) -> chan.done(state)).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(8).await
    let srv = chan.spawn(server(rx)).await
    for v in [(n * 31 + 7) % 100 for n in 0..5]:
        chan.send(tx, Reading(v)).await
    let (reply_tx, reply_rx) = chan.channel(1).await
    chan.send(tx, Report(reply_tx)).await
    let r = chan.recv(reply_rx).await
    match r:
        Some(Summary(count, total)) -> print(console, json.stringify(.{count: count, total: total}))
        _ -> print(console, "no reply")
    chan.join(srv).await
```

```text
{"count":5,"total":145}
```

A lot is on display in those few lines: `async`/`await` and first-class channels
(`Sender`/`Receiver`), one message `type` matched exhaustively, a list
comprehension to feed it, and an **anonymous record** (`.{count, total}`) turned
straight into JSON by reflection — with no response type to declare. Tasks share
no memory and the scheduler is deterministic, so this prints the same thing on the
interpreter and the compiled-to-WebAssembly backend, every run.

Laziness is just as light. A `gen fn` writes a sequence as an ordinary loop and
`yield`s each value, producing an `Iter` that computes only what is demanded — so
an infinite generator is fine when something bounds it:

```witchy
import iter
import json

// A generator yields a lazy, possibly-infinite sequence; the caller bounds it.
gen fn squares() -> Iter(Int):
    var n = 1
    while true:
        yield n * n
        n = n + 1

fn main(console: Console):
    let first5: List(Int) = iter.collect(iter.take(squares(), 5))
    print(console, json.stringify(.{squares: first5}))
```

```text
{"squares":[1,4,9,16,25]}
```

The chapters ahead build these up one at a time — values and functions, your own
types, errors, generics and traits, iterators, compile-time code, and the
capability system in depth.
