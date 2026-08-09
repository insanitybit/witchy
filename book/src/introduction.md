# Introduction

## The problem witchy is about

Most general-purpose languages give code ambient authority. A Python function
can read environment variables, inspect `~/.ssh`, or open a network connection
without declaring any of those effects in its signature. Package installation
and build scripts often inherit the invoking user's full permissions as well.

witchy makes authority explicit while retaining a small, general-purpose
language. A program's entry point declares its maximum authority in its
parameter list:

```witchy
// This function can read a file. You can see that. It cannot write, connect to
// the network, or read the clock — there is no parameter that would let it.
fn first_line(dir: Dir[Read], name: String) -> String:
    let contents = dir.read(name)
    contents.split_once("\n").0

fn main(console: Console, dir: Dir[Read]):
    console.print(first_line(dir, "notes.txt"))
```

This program can write to the console and read within the directory supplied by
the host.

The normal launch host supplies `Console`; filesystem access must be granted
explicitly.

```sh
witchy sandbox --dir . main.witchy
```

The same rule applies at other boundaries. A user grants resources when launching
a program, and a project reviews capability growth when adding or updating a
dependency.

**You can audit by reading signatures.** `witchy caps program.witchy` walks the
program and reports its complete capability footprint, computed from the source,
broken down per right (`Dir[Read]` vs `Dir[Write]`, `Net[Connect]` vs
`Net[Listen]`). It's never self-asserted metadata that could drift or lie.

**You can gate on growth.** `witchy caps-diff old new` fails when authority
widened. Put it in CI and a dependency can't quietly start listening on a
socket between versions. The package manager applies the same gate to the runes
(packages) you depend on.

**You can enforce at runtime.** `witchy sandbox program.witchy` compiles to
WebAssembly and runs it in a VM that has been handed *exactly* the host
functions its footprint calls for - and nothing else physically exists for it
to call.

## A taste of the language

The rest of the language supports ordinary application code. This small
in-process server is an `async` task that owns a channel,
folds the messages it receives into running state, and answers a request - the
request/reply shape you'd normally reach for a socket, here in pure, deterministic
witchy that needs nothing but `Console`.

```witchy
import json
// This program's channels all carry one message type; a program may also use
// channels of several different types.

// A tiny stateful server: it folds incoming readings into a running
// (count, total) and answers a Report by sending the totals back.
import reflect
from chan import Sender, Receiver

type Msg:
    Reading(Int)
    Report(Sender(Msg))
    Summary(Int, Int)

async fn server(inbox: Receiver(Msg)):
    chan.serve(inbox, (0, 0), fn(state: (Int, Int), m):
        match m:
            Reading(v) -> chan.done((state.0 + 1, state.1 + v))
            Report(reply) -> chan.and_then(chan.send(reply, Summary(state.0, state.1)), fn(_u): chan.done(state))
            Summary(_c, _t) -> chan.done(state)
    ).await

async fn main(console: Console):
    let (tx, rx) = chan.channel(8).await
    let srv = chan.spawn(server(rx)).await
    for v in [(n * 31 + 7) % 100 for n in 0..5]:
        chan.send(tx, Reading(v)).await
    let (reply_tx, reply_rx) = chan.channel(1).await
    chan.send(tx, Report(reply_tx)).await
    let r = chan.recv(reply_rx).await
    match r:
        Some(Summary(count, total)) -> console.print(json.stringify(.{count: count, total: total}))
        _ -> console.print("no reply")

    chan.join(srv).await
```

```text
{"count":5,"total":145}
```

A lot is on display in those few lines: `async`/`await` and first-class channels
(`Sender`/`Receiver`), one message `type` matched exhaustively, a list
comprehension to feed it, and an **anonymous record** (`.{count, total}`) turned
straight into JSON by reflection - with no response type to declare. Tasks share
no memory and the scheduler is deterministic, so this prints the same thing on the
interpreter and the compiled-to-WebAssembly backend, every run.

Laziness is just as light. A `gen fn` writes a sequence as an ordinary loop and
`yield`s each value, producing an `Iter` that computes only what is demanded - so
an infinite generator is fine when something bounds it:

```witchy
import iter
import json
// A generator yields a lazy, possibly-infinite sequence; the caller bounds it.
import reflect

gen fn squares() -> Iter(Int):
    var n = 1
    while true:
        yield n * n
        n = n + 1

fn main(console: Console):
    let first5: List(Int) = iter.collect(squares().take(5))
    console.print(json.stringify(.{squares: first5}))
```

```text
{"squares":[1,4,9,16,25]}
```
