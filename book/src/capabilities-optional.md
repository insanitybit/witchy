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
            d.append(name, line)
            true
        None -> false

fn main(console: Console, dir: Dir[Write]):
    let wrote = record(Some(dir), "log.txt", "started")
    console.print("${wrote}")
    let skipped = record(None, "log.txt", "goes nowhere")
    console.print("${skipped}")
```

You construct the values with `Some(dir)` and `None` like any other option - a
The capability is an ordinary value passed as an argument.

The important part: **the auditor sees through `Option`.** `witchy caps` reports

```text
record  Dir[Write]
```
 -
because the code *can* write (it matches `Some` and calls `write`). Wrapping a
capability in `Option` doesn't hide it from the footprint; the analysis is
static and conservative, counting what the code is able to do regardless of
whether the value is `None` at runtime. Optionality changes the control flow, not
the authority on paper.

## One of several rights: keep the capability direct

Sometimes the question isn't "do I have it?" but "which rights do I want to use?"
Keep the capability direct and branch into functions whose parameter types name
the authority they need:

```witchy
fn read_only(d: Dir[Read], name: String) -> String:
    d.read(name)

fn touch(d: Dir[Write], name: String) -> String:
    d.write(name, "touched")
    "wrote ${name}"

fn handle(dir: Dir, writable: Bool, name: String) -> String:
    if writable:
        touch(dir as Dir[Write], name)
    else:
        read_only(dir as Dir[Read], name)

fn main(console: Console, dir: Dir):
    console.print(handle(dir, false, "notes.txt"))
    console.print(handle(dir, true, "out.txt"))
```

`handle` can read or write, so its footprint is the union: `Dir`. The narrower
helpers still report only what they use (`Dir[Read]` and `Dir[Write]`).

A capability-carrying **aggregate** compiles to a typed wasm GC struct when its
layout is concrete. This includes a direct structural tuple such as
`(Dir[Read], String)`, plus each closed instance of a generic or non-generic
`type` with named fields, positional payloads, or multiple variants, sealed
`capability` declaration or plain `type` alike. The one exception is a
transparent single-field capability brand, which stays a direct externref
instead of allocating a wrapper struct. Concrete tuples and nominal instances
may nest while keeping the capability an unforgeable reference; nominal types
may also recurse. Tuple construction, numeric projection, parameter and return
passing, and `let`/`match` destructuring use that typed representation on both
backends.
Concrete and generic type aliases are expanded before representation selection,
so an alias for one of these tuples has exactly the same GC shape and behavior.

Closures may capture capabilities: the compiled backend keeps each boxed
lambda's captures in a typed GC environment, so the capability remains an
unforgeable reference through aliases and indirect calls. Function and
capability values may also live in closed tuples, nominal instances,
`Option`/`Result`, and nested `List` values. A reference-bearing `List(T)` uses
a typed GC array and supports literals, persistent updates, indexed access,
list patterns, `pop`, and iteration. `?` may propagate between `Option` or
`Result` values whose success payloads have different representations; the
compiled backend rebuilds the enclosing function's failure value.

`Dict` keys or values that carry references, open generic function calls
instantiated with capability-bearing values, `region:` copy-out carrying
capabilities, and isolated-worker callback crossings remain intentionally
rejected until those boundaries have concrete typed layouts.
Capability aggregates also can't be rendered or compared for equality:
authority isn't printable data or identity. No capability is silently boxed
into an ordinary heap slot.

## At the entry point

One restriction: `main` itself may only take *bare* host capabilities (or
`List(String)`), not `Option(Dir)` or a capability enum. The root grant is always
concrete - the host either hands `main` a real `Dir` or that parameter doesn't
exist. So optionality lives *inside* the program, not at the boundary.

To make a capability genuinely conditional on, say, an environment variable,
grant it to `main` and decide there whether to pass it onward:

```witchy
fn run(console: Console, out: Option(Dir[Write])):
    match out:
        Some(d) ->
            d.write("out.txt", "result")
            console.print("wrote out.txt")
        None -> console.print("dry run; nothing written")

fn main(console: Console, dir: Dir[Write], env: Env):
    let enabled = match env.get_env("WRITE"):
        Some(_) -> Some(dir)
        None -> None

    run(console, enabled)
```

`main` declares the upper bound of its authority (it *may* write), and the rest
of the program decides, value by value, whether that authority is actually
exercised. Everything below `run` that holds `None` provably can't write.

The type checker establishes this condition statically; the sandbox enforces it
at runtime.
