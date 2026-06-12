# Narrowing and Attenuation

Holding a capability doesn't mean you must hand out all of it. witchy lets you
pass on a *weaker* version — fewer verbs, or a smaller slice of the world. This
is **attenuation**, and it's how you give a function exactly the power it needs
and no more.

## Rights: fewer verbs

`Dir` and `Net` are parameterized by *rights* — the operations they permit. A
bare `Dir` allows everything; `Dir[Read]` allows only reading; `Dir[Write]` only
writing. The right is part of the type, so it's checked:

```witchy
// This loader provably cannot write. The `Dir[Read]` it receives has no `write`.
fn load(dir: Dir[Read], name: String) -> String:
    read(dir, name)

fn main(console: Console, dir: Dir):
    print(console, load(dir, "config.txt"))
```

`load` takes `Dir[Read]`, but `main` holds a full `Dir`. Passing the full `Dir`
where `Dir[Read]` is expected is allowed — **more authority stands in for less**,
narrowing automatically at the call boundary. The reverse never type-checks: you
cannot pass a `Dir[Read]` where a `Dir[Write]` is wanted, because that would be
*widening*, and authority only ever shrinks.

`Net` works the same way along two axes: a verb (`Connect` to dial out vs
`Listen` to accept connections) and a transport (`Tcp`/`Udp`/`Uds`). A
`Net[Connect, Tcp]` is a TCP client that structurally cannot listen:

```witchy
// A fetcher that can dial out over TCP but cannot open a listening socket.
fn fetch(net: Net[Connect, Tcp], addr: String) -> String:
    let sock = connect(net, addr)
    recv_all(sock)

fn main(console: Console, net: Net[Connect, Tcp]):
    print(console, fetch(net, "example.test:80"))
```

If `fetch` tried to call `listen(net, ...)`, it wouldn't compile — `Net[Connect]`
has no `listen`.

## Naming a narrowed handle: `as`

Implicit narrowing happens at calls. When you want to *name* a weaker handle —
to keep using it locally, or to make the attenuation obvious — ascribe it with
`as`:

```witchy
fn main(console: Console, dir: Dir):
    let ro = dir as Dir[Read]      // a read-only view of the same subtree
    print(console, read(ro, "log.txt"))
    // `ro` cannot write; `write(ro, ...)` would be a compile error.
```

You can only ever drop rights with `as`, never add them. Authority can't be
laundered back up.

## Subtrees: a smaller world

Rights restrict the *verbs*; `subdir` restricts the *scope*. A `Dir` is not "the
filesystem" — it is one directory subtree, and `subdir(dir, "uploads")` mints a
new capability confined to that child:

```witchy
// `handle_upload` gets ONLY the uploads/ folder. It cannot see the rest of the
// program's directory, even though its caller can.
fn handle_upload(uploads: Dir, name: String, body: String):
    write(uploads, name, body)

fn main(console: Console, dir: Dir):
    let uploads = subdir(dir, "uploads")
    handle_upload(uploads, "avatar.png", "...")
    print(console, "stored")
```

Combine the two — `subdir(dir, "uploads") as Dir[Write]` — and you've handed a
function write access to one folder and nothing else, in a way the type system
guarantees and a reviewer can read at a glance.

## Brands: encoding policy

You can go further and wrap a capability in your own type to attach *policy*. A
`Backup` type whose only constructor checks an invariant becomes a capability
that can only be obtained the blessed way — and the footprint analyzer still
sees the `Dir` inside it, so the wrapping adds discipline without hiding
authority. The `examples/branded_caps.witchy` program in the repository walks
through this; the key idea is that capabilities compose with your domain types.

## Block firewalls: `retain` and `without`

Everything above attenuates along *calls* — you weaken a handle as you pass it
on. Sometimes you want to weaken authority along *scope* instead: to carve out a
region of a function where some capability simply isn't available, regardless of
what the surrounding code holds. That is what `retain` and `without` do.

A `without` block drops the named capabilities for the length of the block:

```witchy
fn main(console: Console, clock: Clock):
    without clock:
        // `clock` is walled off in here — `now(clock)` would not compile.
        print(console, "this section provably does not read the clock")
    print(console, to_string(now(clock)))   // outside, clock is back
```

A `retain` block is the mirror image: it keeps *only* what you name and drops
everything else. Writing `retain:` with no names at all seals the block
completely — no capability survives, so the region is pure computation:

```witchy
fn main(console: Console, clock: Clock):
    retain console:
        // Only `console` survives; `clock` is gone, even though `main` holds it.
        print(console, "this section can print and nothing else")
    retain:
        // Fully sealed: no authority in scope, so this does provably no I/O.
        let sum = 2 + 2
    print(console, to_string(now(clock)))
```

### Why this is more than a comment

The firewall is enforced by the type checker, and — crucially — it is sealed
against the *future*. If someone later adds a `Net` parameter to `main`, the code
inside a `retain console:` block still cannot touch the network: the network was
never named, so it is never let in. A block's authority is fixed by what it asks
for, not by whatever its enclosing scope happens to accumulate over time. That is
a local, durable guarantee that a slice of a function does no more than the
handful of things you allowed — the same "exactly this power and no more" promise
as parameter-level attenuation, but for a region of code rather than a call.

Re-binding a dropped name inside the block is still allowed: `without console:`
followed by `let console = ...` legitimately shadows the firewall, because a
fresh value is not the forbidden capability — and inside the block you have no
way to *name* the dropped one to smuggle it back.

## The supply-chain payoff

Because rights are part of the footprint, `witchy caps` reports them precisely:

```text
fetch   Net[Connect, Tcp]
load    Dir[Read]
serve   Net[Listen, Tcp]
main    Console, Dir, Net
total   Console, Dir, Net
```

And `witchy caps-diff old.witchy new.witchy` understands them. A change from
`Net[Connect]` to `Net[Connect, Listen]` is a *widening* — "this code now
*listens*" — and fails the gate, even though both are "uses the network." The
supply-chain signal is verb-precise, not just kind-precise.

So far a capability is something you *have* and pass along, perhaps narrowed.
But authority can also be *conditional* — held only sometimes, or in one of
several shapes. That's next.
