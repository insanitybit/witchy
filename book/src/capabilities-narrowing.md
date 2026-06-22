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

The full `Dir` verb set, and the right each one demands:

| Verb | Needs | Semantics |
|---|---|---|
| `read(d, path)` | `Read` | file contents; error if missing or outside the subtree |
| `exists(d, path)` / `is_dir(d, path)` | `Read` | total — a path outside the subtree just reads as `false` |
| `list(d)` | `Read` | entry names in the directory |
| `subdir(d, name)` | `Read` | mint a capability confined to a child, keeping the rights (see below) |
| `write(d, path, contents)` | `Write` | **replace** the whole file, creating it if absent |
| `append(d, path, contents)` | `Write` | add to the end, creating the file if absent |
| `make_dir(d, name)` | `Write` | create a subdirectory (idempotent) |

Note `write` *overwrites* — for a log you keep adding to, use `append`.

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

## User-defined capabilities: `capability X from U`

A library can define its own capability by refining one of the host's, with
`capability X from U`. The new type `X` wraps the underlying capability `U`, and
it is sealed: only the module that declares `X` may construct or destructure it.
No other module can forge an `X` or pull the `U` back out.

```witchy
capability ConfigDir from Dir[Read]

// The ONLY way to get a `ConfigDir` — a checked smart constructor, in the same
// module. Outside this module nobody can write `ConfigDir(...)`.
pub fn config_dir(root: Dir[Read]) -> Option(ConfigDir):
    if exists(root, "config.toml"):
        Some(ConfigDir(root))
    else:
        None

// A consumer that takes `ConfigDir` *statically* demands the brand, not just any
// `Dir` — and can never reach the raw `Dir` back out.
fn load(c: ConfigDir, name: String) -> String:
    match c:
        ConfigDir(dir) -> read(dir, name)
```

`witchy caps` still reports `load` as `Dir[Read] (refined: ConfigDir)`. The brand
records intent and tightens the API, but it never hides authority from the
footprint. A library can ship a `Redis` over `Net`, a database handle that
exposes read and write as separate facets, or a mailer over `Net` and a `Secret`.

A plain `type` brand (`type ConfigDir: ConfigDir(Dir)`) is convention only: any
module can construct one. Use `capability` when the brand has to hold as a
guarantee. See `examples/branded_caps` and `examples/redis_capability`.

For the network, `restrict(net, "host:port")` returns a `Net` confined to a set
of addresses: an exact `host:port`, `host:*`, or an IPv4 CIDR. The host enforces
it on `connect` and `listen`, the same way `subdir` confines a `Dir` to a
subtree. Pass a dependency `restrict(net, "10.0.0.5:6379")` and it reaches that
one server.

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
    print(console, "${now(clock)}")
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
    print(console, "${now(clock)}")
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
  fetch  Net[Connect, Tcp]
  load   Dir[Read]
  serve  Net[Listen, Tcp]
  main   Console, Dir, Net
  total  Console, Dir, Net
```

(Private helpers appear too — the rows are *every* function whose signature
carries a capability; `total` is the union over the entry points.)

And `witchy caps-diff old.witchy new.witchy` understands them. A change from
`Net[Connect]` to `Net[Connect, Listen]` is a *widening* — "this code now
*listens*" — and fails the gate, even though both are "uses the network." The
supply-chain signal is verb-precise, not just kind-precise.

So far a capability is something you *have* and pass along, perhaps narrowed.
But authority can also be *conditional* — held only sometimes, or in one of
several shapes. That's next.
