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
| `subtree(d, name)` | `Read` | mint a capability confined to a child, keeping the rights (see below) |
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

Rights restrict the *verbs*; `dir.subtree(...)` restricts the *scope*. A `Dir` is
not "the filesystem" — it is one directory subtree, and `dir.subtree("uploads")`
mints a new capability confined to that child. It is the host-primitive method
form, the filesystem counterpart of `net.only(...)`:

```witchy
// `handle_upload` gets ONLY the uploads/ folder. It cannot see the rest of the
// program's directory, even though its caller can.
fn handle_upload(uploads: Dir, name: String, body: String):
    write(uploads, name, body)

fn main(console: Console, dir: Dir):
    let uploads = dir.subtree("uploads")
    handle_upload(uploads, "avatar.png", "...")
    print(console, "stored")
```

Combine the two — `dir.subtree("uploads") as Dir[Write]` — and you've handed a
function write access to one folder and nothing else, in a way the type system
guarantees and a reviewer can read at a glance. The narrowing also chains and
stays confined: `dir.subtree("a").subtree("b")` reaches `a/b`, and `..` still
cannot escape.

## Files: the leaf

A `Dir` is authority over a *subtree*; a **`File`** is the leaf — authority over
exactly *one* file. A function that only needs to read one config file shouldn't be
handed a whole directory, so a `Dir` navigates down to a single file:

```witchy
// `read_config` provably touches one file — `witchy caps` reports it as
// `File[Read]`, never `Dir`. It cannot see any other file in the tree.
fn read_config(f: File[Read]) -> String:
    read(f)

fn main(console: Console, dir: Dir):
    let cfg = dir.read_file("config.toml")     // File[Read] — needs Dir[Read], must exist
    print(console, read_config(cfg))

    let log = dir.write_file("run.log")        // File[Write] — needs Dir[Write]
    write(log, "started")                       // a File op takes no path — it IS the file
```

The **name states the conferred right**, and it's all checked statically:
`dir.read_file` needs `Dir[Read]` and yields `File[Read]`; `dir.write_file` needs
`Dir[Write]` and yields `File[Write]`. So a `Dir[Read]` can only ever produce a
`File[Read]` (calling `write_file` on it is a compile error), and `write` on a
`File[Read]` is a compile error too — the read-only chain is provable end to end.
Navigation keeps the same `..`/absolute/symlink confinement as `read`, and a `File`
can also be handed straight to `main` (`main(config: File[Read])`, granted with
`--file`) — the least authority for a single-file program, with no `Dir` at all.
A `File` is read/write only; there is no exec-on-a-`File` (spawning a process is
the separate `Exec` capability, below).

## Net: a smaller slice of the network

Rights narrow the *verbs* a `Net` permits (`Connect` vs `Listen`); to narrow its
*reach* — which hosts it may dial — confine its **address-set**. This is the
network counterpart of `dir.subtree`, and it uses typed policy values from
`std/confine` rather than ad-hoc strings:

```witchy
import confine

// `talk_to_db` is handed a Net that can reach exactly one server. Even though
// `main` holds the whole network, the dependency cannot dial anywhere else.
fn talk_to_db(db: Net[Connect, Tcp]):
    let sock = connect(db, "10.0.0.5:6379")
    send_line(sock, "PING")

fn main(console: Console, net: Net):
    let db = net.only(confine.tcp("10.0.0.5", 6379))   // intersect down to one endpoint
    talk_to_db(db)
    print(console, "done")
```

`net.only(policy)` *intersects* the carried address-set with `policy`; an endpoint
survives only if it was already admitted, so refinement can only ever shrink the
set. `net.deny(policy)` does the opposite — subtracts a slice — and the two chain:
`net.deny(confine.cidr_any("10.0.0.0/8")).only(confine.tcp("192.168.1.1", 80))`
removes a private block, then keeps a single host. The policy constructors are
`confine.tcp(host, port)`, `any_port(host)`, `cidr(block, port)`, `cidr_any(block)`,
and `union(a, b)` for a multi-endpoint set. The host enforces the set **at the
syscall** on both backends, so a narrowed `Net` structurally cannot reach
elsewhere. (HTTPS isn't a separate right: ask for TLS at connect time with a
`tls:` prefix on the address you dial — `connect(net, "tls:example.com:443")`.)

## Spawning processes: the `Exec` capability

`Exec` is authority to run a *native subprocess* — and it is the single most
dangerous capability, because a spawned process runs with full OS authority
**outside** witchy's sandbox. witchy cannot confine what it spawns, so `Exec` is
kept conspicuous and granted on its own line, never folded into `File`. Two things
keep it honest: the binary is **named through a `Dir[Read]`** (you can only execute
a file you can *read*, resolved with the same confinement as `read`), and the call
takes an argv list, never a shell string:

```witchy
import exec

// `run_tool` can execute exactly the binaries reachable through `bin` — nothing
// it cannot already read, and no shell to inject into.
fn run_tool(e: Exec, bin: Dir[Read], name: String) -> Int:
    let (code, _out) = exec.run(e, bin, name, ["--version"], "")
    code
```

Almost nothing should hold `Exec`; it exists chiefly so a self-hosted tool (like
the package manager driving the compiler) can run a confined subprocess. `witchy
caps` surfaces it like any other authority, and the supply-chain gate treats newly
wanting `Exec` as a serious widening.

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

## Capabilities that carry policy

A `capability` can carry **state beside** the authority it wraps — a sealed
*record* mixing a host capability with ordinary policy data. A database handle, say,
is a `Net` confined to one server *plus* the table it is scoped to:

```witchy
// A confined `Net` (the hard, audited authority) + a `table` it is scoped to (a
// soft policy the library enforces). Sealed: only this module can mint, refine, or
// destructure one, and its fields are private — reached with `match`, never
// `.field` — so the underlying `Net` can never leak past the policy.
capability Table:
    net: Net[Connect, Tcp]
    name: String

// The only way to make one (sealed constructor).
pub fn open_table(net: Net[Connect, Tcp], name: String) -> Table:
    Table(net, name)

// A query refuses any table but the one the handle carries — the policy lives in
// this one reviewable place.
pub fn count(t: Table, requested: String) -> String:
    match t:
        Table(_, name) ->
            if requested == name: "ok: " + requested
            else: "denied: " + requested

fn main(console: Console, net: Net):
    let users = open_table(net, "users")
    print(console, count(users, "users"))      // ok: users
    print(console, count(users, "secrets"))    // denied: secrets
```

`witchy caps` sees straight through the record: `Table` audits as exactly `Net`,
because the footprint sums its capability-typed fields and the `String` carries no
authority. So you get carried policy with **nothing hidden** — the hard tier (the
`Net` is host-confined to one server) plus a soft, library-enforced tier (the
`table` filter), in one unforgeable value. See `examples/carried_state`.

## Withholding authority by structure

Everything above attenuates along *calls* — you weaken a handle as you pass it
on. The same mechanism gives you the strongest possible way to *deny* authority
to a stretch of code: don't pass it. A function or closure that never receives a
capability cannot use it — there is no name to reach, no value to alias, nothing
to forge.

So when a region of work must not touch the network (or the clock, or the disk),
lift it into a function that simply isn't given that capability:

```witchy
fn audit_log(console: Console, body: String):
    // Never receives `clock`, so it structurally cannot read the wall clock.
    print(console, "audit: ${body}")

fn main(console: Console, clock: Clock):
    let body = "request handled"
    audit_log(console, body)
    print(console, "logged at ${now(clock)}")
```

`audit_log` cannot read the clock in *any* execution, under *any* later refactor:
the authority was never handed to it. This is **capture-as-dependency-injection**
— authority comes from *holding* a capability, so the un-bypassable way to deny
it is to not pass the reference. The boundary is sealed against the future, too:
if someone later adds a `Net` parameter to `main`, the code inside `audit_log`
still cannot dial, because its authority is fixed by its own signature, not by
whatever its callers accumulate over time.

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
