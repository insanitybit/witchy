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
        print(console, "count is " + "${n}")

fn main(console: Console):
    let counter = spawn Counter(console)
    counter.Inc()
    counter.Inc()
    counter.Inc()
```

```text
count is 1
count is 2
count is 3
```

Three things are happening here:

- **`spawn Counter(console)`** creates a new actor, initializing its state. The
  fields with no default (`console`) are supplied as arguments, in order; fields
  with a default (`var n: Int = 0`) may be omitted. Data fields work the same
  way — `id: Int` with no default is filled from its spawn argument, giving
  each instance its own value. `spawn Counter(...)` returns a `Subject(Counter)`
  — a *typed* handle to the actor's mailbox, not the actor itself. The type
  records which actor it talks to, so sending a message `Counter` has no handler
  for is a compile error, not a runtime surprise (more on that below).
- **`counter.Inc()`** sends the actor an `Inc` message — method-call syntax on
  the subject, where the message name is the (capitalized) handler. It drops the
  message into the mailbox and returns immediately; the handler runs later, on
  its own. A message with arguments is `counter.Add(10)`.
- **The state is private.** Nothing outside the actor can read or write `n`. The
  only way to affect a `Counter` is to send it a message it has a handler for.

## State is reached only through messages

Because `n` lives inside the actor and handlers run one at a time, there is no
way for two pieces of code to mutate it at once. That is the whole safety story:
concurrency without shared mutable state. You don't protect `n` with a lock — you
simply can't reach it except by sending a message, and messages are serialized.

## An actor's own address: `self`

A handler that needs to reach itself declares `self` as its first parameter —
`on Tick(self, n: Int)`. That `self` is the actor's own `Subject`, the authority
to send itself a message; a handler that doesn't declare it can't name it. (It is
not a message argument — the message `Tick` still carries only `n`.) This lets an
actor drive its own work without anyone handing it its address:

```witchy
actor Countdown:
    console: Console

impl Countdown:
    on Tick(self, n: Int):
        if n > 0:
            print(console, "tick ${n}")
            self.Tick(n - 1)
        else:
            print(console, "liftoff")

fn main(console: Console):
    let c = spawn Countdown(console)
    c.Tick(3)
```

```text
tick 3
tick 2
tick 1
liftoff
```

`self` is also how an actor introduces itself: put `self` in a message and the
receiver gains the authority to message you back.

## Actors talking to actors

A `Subject` can be held as another actor's state, so actors form networks. Here a
`Worker` does a computation and reports the result to its `Boss` by sending *it*
a message — the worker never prints anything itself.

```witchy
actor Worker:
    boss: Subject(Boss)

impl Worker:
    on Task(label: String, work: Int):
        boss.Done(label, work * work)

actor Boss:
    console: Console

impl Boss:
    on Done(label: String, result: Int):
        print(console, label + " -> " + "${result}")

fn main(console: Console):
    let boss = spawn Boss(console)
    let worker = spawn Worker(boss)
    worker.Task("square 5", 5)
    worker.Task("square 9", 9)
```

```text
square 5 -> 25
square 9 -> 81
```

The `Boss` holds a `Console`; the `Worker` holds only a `Subject(Boss)` pointing
at the boss. That annotation is worth pausing on: a `Subject(Boss)` may only be
sent messages `Boss` declares a handler for, checked when you compile. Write
`boss.Whoops(...)` and the compiler names the actor and the missing
handler rather than letting a misrouted message vanish at runtime. (A bare
`Subject`, with no actor named, is the untyped escape hatch — it accepts any
declared message, validated against the program's handlers as a whole.)

## Asking for a result: `ask` and `reply`

A `.Msg()` send is one-way: it drops a message in the mailbox and returns
immediately, and the handler runs later. Often that is exactly right. But
sometimes you want an *answer back* — and that is what `ask` is for.
`ask(subject, Msg(...))` runs the target's handler right away and returns the
value the handler hands back with `reply(...)`:

```witchy
actor Squarer:

impl Squarer:
    on Square(n: Int):
        reply(n * n)

fn main(console: Console):
    let w = spawn Squarer()
    var sum = 0
    for i in [3, 5, 7]:
        sum = sum + ask(w, Square(i))
    print(console, "sum of squares: ${sum}")
```

```text
sum of squares: 83
```

This is the shape a one-way send alone can't express: `main` spawns a worker,
asks it to compute, and gathers each result to summarize. A handler answers an
`ask` by calling `reply(v)`, and `ask` returns that `v`. (A handler reached by an
ordinary `.Msg()` send has no one waiting, so a `reply` there is simply ignored.)
The reply is an `Int` — a count, a sum, an id, the usual things you ask an actor
for.

Two things to keep straight:

- **`.Msg()` is asynchronous; `ask` is synchronous.** A `.Msg()` send is queued
  and its handler runs later; an `ask` runs the handler *now* and waits for the
  reply. So if you send an actor several messages and then `ask` it, the ask sees
  the state from *before* those queued sends are processed.
- **An `ask` cannot re-enter a busy actor.** While a handler runs, its actor is
  mid-delivery; asking that same actor — directly, or around a cycle — is an
  error, because it would have to answer a new question while still answering the
  first. (This is the same on both backends, down to the error.)

That covers how actors talk. Before the part that makes them *witchy* actors,
one practical question: when do all these handlers actually run, and when does it
all stop?

## Lifetimes: when actors run, and when they stop

witchy's scheduler is deterministic — the same program produces the same ordering
every time, on both backends — and that determinism shapes the lifecycle in a way
worth being precise about.

**`main` runs first, as a setup phase.** `spawn` creates an actor; a `.Msg()` send
*enqueues* a message and returns immediately — the handler does **not** run yet.
`main` runs straight through to its last line with messages piling up behind it.

**Then the queue drains.** Once `main` returns, the runtime processes the queued
messages one at a time, each handler running to completion before the next begins.
There is no preemption: an actor is never interrupted mid-handler, and two handlers
never overlap. A handler may itself send messages — those go to the back of the
queue. That is exactly why the self-driving `Countdown` above works: each `Tick`
handler enqueues the next `Tick`, and the drain keeps pulling them off.

**The program ends at quiescence — an empty queue.** There is no separate "stop"
for an actor, and an actor never "goes out of scope." It doesn't terminate; it
simply becomes *inert* once nothing sends it a message. A `Subject` is only an
address: dropping your last reference to an actor does not stop it (the runtime
still holds it), and holding a reference does not keep it busy. The system is done
precisely when the last message has been handled and the queue is empty. An actor
that *always* re-sends to itself never reaches quiescence; rather than hang, the
scheduler stops it at a step budget with a loud error.

**So how does `main` "exit" while actors are live?** It doesn't have to wait:
by the time `main` returns, *no handler has run yet* — they all run during the
drain that follows. `main` therefore cannot observe a half-finished actor; its job
is to build the actor graph and hand out authority, and the work happens afterward.
The lone exception is `ask`, which runs a target's handler **synchronously, inline,
during `main`** — which is the whole reason `ask` exists: it is the only way `main`
can see a result before the drain.

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
    auditor.Record("started")
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
(Capabilities travel at `spawn`, never in messages.) An `ask` is the same
boundary run synchronously: the host invokes the target VM's handler to
completion and copies its `reply` back to the caller — no VM ever pauses
mid-handler, so a question-and-answer behaves identically compiled or
interpreted.

Passing a `Subject` in a message is capability delegation: the receiver gains
the authority to message that actor, and nothing else. An actor that was never
introduced to the printer cannot reach it — there is no global registry to look
one up in, only the references you were explicitly handed.

Next: putting it all together in a real project.
