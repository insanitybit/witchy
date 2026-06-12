# Concurrency with Actors

witchy's concurrency model is **actors**: independent units that own their state,
never share memory, and communicate only by sending each other messages. There
are no locks and no data races to reason about, because there is nothing shared
to race over — each actor processes one message at a time, in order.

## Declaring an actor

An `actor` declares the state it holds. A separate `impl` block gives it message
handlers, each written `on Message(args):`. A handler is the only code that can
touch the actor's state, and it runs to completion before the next message.

```witchy
actor Counter:
    console: Console
    var n: Int = 0

impl Counter:
    on Inc():
        n = n + 1
        print(console, "count is " <> to_string(n))

fn main(console: Console):
    let counter = spawn Counter(console)
    send(counter, Inc)
    send(counter, Inc)
    send(counter, Inc)
```

```text
count is 1
count is 2
count is 3
```

Three things are happening here:

- **`spawn Counter(console)`** creates a new actor, initializing its state. The
  fields with no default (`console`) are supplied as arguments, in order; fields
  with a default (`var n: Int = 0`) may be omitted. `spawn` returns a `Subject` —
  a typed handle to the actor's mailbox, not the actor itself.
- **`send(counter, Inc)`** drops an `Inc` message (a nullary message is written bare, like any nullary constructor) into that mailbox and returns
  immediately. The handler runs later, on its own.
- **The state is private.** Nothing outside the actor can read or write `n`. The
  only way to affect a `Counter` is to send it a message it has a handler for.

## State is reached only through messages

Because `n` lives inside the actor and handlers run one at a time, there is no
way for two pieces of code to mutate it at once. That is the whole safety story:
concurrency without shared mutable state. You don't protect `n` with a lock — you
simply can't reach it except by sending a message, and messages are serialized.

## Actors talking to actors

A `Subject` can be held as another actor's state, so actors form networks. Here a
`Worker` does a computation and reports the result to its `Boss` by sending *it*
a message — the worker never prints anything itself.

```witchy
actor Worker:
    boss: Subject

impl Worker:
    on Task(label: String, work: Int):
        send(boss, Done(label, work * work))

actor Boss:
    console: Console

impl Boss:
    on Done(label: String, result: Int):
        print(console, label <> " -> " <> to_string(result))

fn main(console: Console):
    let boss = spawn Boss(console)
    let worker = spawn Worker(boss)
    send(worker, Task("square 5", 5))
    send(worker, Task("square 9", 9))
```

```text
square 5 -> 25
square 9 -> 81
```

The `Boss` holds a `Console`; the `Worker` holds only a `Subject` pointing at the
boss. Which leads to the part that makes actors *witchy* actors.

## Actors and capabilities

An actor can only do what its **state** lets it do. A `Worker` that holds no
`Console`, no `Dir`, and no `Net` provably cannot print, read a file, or touch the
network — it can only compute and send messages. Authority enters an actor exactly
once, as a constructor argument at `spawn`, and you can attenuate it on the way in
just like any other capability:

```witchy
actor Auditor:
    log: Dir[Write]

impl Auditor:
    on Record(line: String):
        write(log, "audit.txt", line)

fn main(console: Console, dir: Dir):
    // The auditor gets write access to one subtree and nothing else — not the
    // console, not the rest of the filesystem.
    let auditor = spawn Auditor(subdir(dir, "audit") as Dir[Write])
    send(auditor, Record("started"))
    print(console, "spawned the auditor")
```

The `Auditor` cannot print and cannot read — its entire authority is "append to
files under `audit/`", and that is visible at the `spawn` site and checked by the
type system. Concurrency and capability-security are the same idea here: an actor
is a boundary, and you decide exactly what crosses it.

## Actors in the sandbox

When actors compile, the boundary becomes physical: each actor runs in **its own
WebAssembly VM** with its own linear memory, linked with **only the host
imports its capability fields entitle it to**. The `Worker` above has no
`print` import in its instance — not denied, absent. The `Auditor`'s VM links
the directory *write* family and nothing else (`Dir[Write]` is a per-right
gate), and its `Dir` is an opaque handle whose path lives host-side: at
`spawn` the host translates the spawner's handle into the new VM's own table,
so the attenuated `subdir(dir, "audit")` grant stays attenuated across the VM
boundary. A message crosses by value — `Int`, `Float`, and `Subject` fields
are copied, and a `String`'s bytes are read out of the sender and
re-allocated in the receiver — so no actor ever sees another's memory.
(Capabilities travel at `spawn`, never in messages.)

Passing a `Subject` in a message is capability delegation: the receiver gains
the authority to message that actor, and nothing else. An actor that was never
introduced to the printer cannot reach it — there is no global registry to look
one up in, only the references you were explicitly handed.

Next: putting it all together in a real project.
