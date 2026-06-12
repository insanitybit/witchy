# Optional and Conditional Capabilities

A capability is an ordinary value once you hold it, so it composes with the rest
of the type system. Two consequences are worth calling out, because they answer a
question people ask early: *what if a function only sometimes needs authority?*

## Optional: `Option(Dir)`

Wrap a capability in `Option` to say "maybe a `Dir`, maybe not." The function
matches on it and uses the capability only in the `Some` branch:

```witchy
// Append a line if we were given somewhere to write; otherwise do nothing.
// (`append` creates the file if absent and adds to it; `write` would
// overwrite the whole file each call.)
fn record(out: Option(Dir[Write]), name: String, line: String) -> Bool:
    match out:
        Some(d) ->
            append(d, name, line)
            true
        None -> false

fn main(console: Console, dir: Dir[Write]):
    let wrote = record(Some(dir), "log.txt", "started")
    print(console, "${wrote}")
    let skipped = record(None, "log.txt", "goes nowhere")
    print(console, "${skipped}")
```

You construct the values with `Some(dir)` and `None` like any other option — a
capability isn't special here, it's just a value flowing as an argument.

The important part: **the auditor sees through `Option`.** `witchy caps` reports

```text
record  Dir[Write]
```

— because the code *can* write (it matches `Some` and calls `write`). Wrapping a
capability in `Option` does not hide it from the footprint; the analysis is
static and conservative, counting what the code is able to do regardless of
whether the value is `None` at runtime. Optionality changes the control flow, not
the authority on paper.

## One of several shapes: a capability enum

Sometimes the question isn't "do I have it?" but "which form do I have?" — say a
`Dir[Read]` *or* a read-write `Dir`. That's just a sum type, and it needs no
special support:

```witchy
type Access:
    ReadOnly(Dir[Read])
    Writable(Dir[Write])

fn handle(a: Access, name: String) -> String:
    match a:
        ReadOnly(d) -> read(d, name)
        Writable(d) ->
            write(d, name, "touched")
            "wrote " + name

fn main(console: Console, dir: Dir):
    print(console, handle(ReadOnly(dir as Dir[Read]), "notes.txt"))
    print(console, handle(Writable(dir as Dir[Write]), "out.txt"))
```

`handle` accepts either variant and does the right thing for each. And again the
footprint tells the truth — `witchy caps` reports `handle` as needing `Dir`
(read **and** write), the *union* of what the variants permit, because a caller
could hand it either one. The auditor sees straight through your enum, exactly as
it sees through `Option` and through [branded capabilities](capabilities-narrowing.md).

## At the entry point

One restriction: `main` itself may only take *bare* host capabilities (or
`List(String)`), not `Option(Dir)` or a capability enum. The root grant is always
concrete — the host either hands `main` a real `Dir` or that parameter doesn't
exist. So optionality lives *inside* the program, not at the boundary.

To make a capability genuinely conditional on, say, an environment variable,
grant it to `main` and decide there whether to pass it onward:

```witchy
fn run(console: Console, out: Option(Dir[Write])):
    match out:
        Some(d) ->
            write(d, "out.txt", "result")
            print(console, "wrote out.txt")
        None -> print(console, "dry run; nothing written")

fn main(console: Console, dir: Dir[Write], env: Env):
    let enabled = match get_env(env, "WRITE"):
        Some(_) -> Some(dir)
        None -> None
    run(console, enabled)
```

`main` declares the upper bound of its authority (it *may* write), and the rest
of the program decides, value by value, whether that authority is actually
exercised. Everything below `run` that holds `None` provably cannot write.

So far this is all static — the type checker keeping your code honest. The next
chapter turns it into a runtime guarantee strong enough to run code you don't
trust.
