# Narrowing and Attenuation

A capability can be attenuated before it is passed on: remove rights, or restrict
the part of the world it can reach. This is **attenuation**.

## Rights: fewer verbs

`Dir` and `Net` are parameterized by *rights* - the operations they permit. A
bare `Dir` allows everything; `Dir[Read]` allows only reading; `Dir[Write]` only
writing. The right is part of the type, so it's checked:

```witchy
// This loader provably cannot write. The `Dir[Read]` it receives has no `write`.
fn load(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn main(console: Console, dir: Dir):
    console.print(load(dir, "config.txt"))
```

`load` takes `Dir[Read]`, but `main` holds a full `Dir`. Passing the full `Dir`
where `Dir[Read]` is expected is allowed - **more authority stands in for less**,
narrowing automatically at the call boundary. The reverse never type-checks: you
cannot pass a `Dir[Read]` where a `Dir[Write]` is wanted, because that would be
*widening*, and authority only ever shrinks.

The full `Dir` verb set, and the right each one demands:

| Verb | Needs | Semantics |
|---|---|---|
| `d.read(path)` | `Read` | file contents; error if missing or outside the subtree |
| `d.exists(path)` / `d.is_dir(path)` | `Read` | total - a path outside the subtree just reads as `false` |
| `d.list()` | `Read` | entry names in the directory |
| `d.subtree(name)` | `Read` | mint a capability confined to a child, keeping the rights (see below) |
| `d.write(path, contents)` | `Write` | **replace** the whole file, creating it if absent |
| `d.append(path, contents)` | `Write` | add to the end, creating the file if absent |
| `d.make_dir(name)` | `Write` | create a subdirectory (idempotent) |

Note `write` *overwrites* - for a log you keep adding to, use `append`.

`Net` works the same way along two axes: a verb (`Connect` to dial out vs
`Listen` to accept connections) and a transport (`Tcp`/`Udp`/`Uds`). A
`Net[Connect, Tcp]` is a TCP client that structurally cannot listen:

```witchy
// A fetcher that can dial out over TCP but cannot open a listening socket.
fn fetch(net: Net[Connect, Tcp], addr: String) -> String:
    let sock = net.connect(addr)
    sock.recv_all()
```

If `fetch` tried to call `net.listen(...)`, it wouldn't compile - `Net[Connect]`
has no `listen`. This is deliberately a library fragment rather than an
executable browser example: raw sockets have no honest browser provider.

## Naming a narrowed handle: `as`

Implicit narrowing happens at calls. When you want to *name* a weaker handle -
to keep using it locally, or to make the attenuation obvious - ascribe it with
`as`:

```witchy
fn main(console: Console, dir: Dir):
    // A read-only view of the same subtree.
    let ro = dir as Dir[Read]
    console.print(ro.read("log.txt"))

    // `ro` cannot write; `ro.write(...)` would be a compile error.
```

You can only ever drop rights with `as`, never add them. Authority can't be
laundered back up.

## Subtrees: a smaller world

Rights restrict the *verbs*; `dir.subtree(...)` restricts the *scope*. A `Dir` is
not "the filesystem" - it is one directory subtree, and `dir.subtree("uploads")`
mints a new capability confined to that child. It is the host-primitive method
form, the filesystem counterpart of `net.only(...)`:

```witchy
// `handle_upload` gets ONLY the uploads/ folder. It cannot see the rest of the
// program's directory, even though its caller can.
fn handle_upload(uploads: Dir, name: String, body: String):
    uploads.write(name, body)

fn main(console: Console, dir: Dir):
    let uploads = dir.subtree("uploads")
    handle_upload(uploads, "avatar.png", "...")
    console.print("stored")
```

Combine the two - `dir.subtree("uploads") as Dir[Write]` - to give a function
write access to one folder. Narrowing chains and stays confined:
`dir.subtree("a").subtree("b")` reaches `a/b`, and `..` cannot escape.

A `Dir` also carries an **entry policy** that narrows *which entries* it may
touch, the third axis alongside rights (verbs) and subtree (scope).
`dir.only(Dir.ext(".log"))` confines a `Dir` so `read`/`write`/`open` only
admit matching files - a non-matching name is refused at the access check, and a
subtree inherits the policy. It is the `Dir` analog of `net.only` below:

```witchy
// `read_logs` is handed a Dir that can only touch `.log` files — even though its
// caller holds the whole directory, it cannot read a `.key` or a `.env`.
fn read_logs(logs: Dir[Read], name: String) -> String:
    logs.read(name)

fn main(console: Console, dir: Dir):
    // Entry policy: only `.log` files.
    let logs = dir.only(Dir.ext(".log"))
    console.print(read_logs(logs, "app.log"))
```

## Files: the leaf

A `Dir` is authority over a *subtree*; a **`File`** is the leaf - authority over
exactly *one* file. A function that only needs to read one config file shouldn't be
handed a whole directory, so a `Dir` navigates down to a single file:

```witchy
// `read_config` provably touches one file — `witchy caps` reports it as
// `File[Read]`, never `Dir`. It cannot see any other file in the tree.
fn read_config(f: File[Read]) -> String:
    f.read()

fn main(console: Console, dir: Dir):
    // File[Read]: needs Dir[Read], must exist.
    let cfg = dir.read_file("config.toml")
    console.print(read_config(cfg))

    // File[Write]: needs Dir[Write].
    let log = dir.write_file("run.log")
    // A File op takes no path; it IS the file.
    log.write("started")
```

The **name states the conferred right**, and it's all checked statically:
`dir.read_file` needs `Dir[Read]` and yields `File[Read]`; `dir.write_file` needs
`Dir[Write]` and yields `File[Write]`. So a `Dir[Read]` can only ever produce a
`File[Read]` (calling `write_file` on it is a compile error), and `write` on a
`File[Read]` is a compile error too - the read-only chain is provable end to end.
Navigation keeps the same `..`/absolute/symlink confinement as `read`, and a
`File` can also be handed straight to `main` (`main(config: File[Read])`, granted
with `--file`) - the least authority for a single-file program, with no `Dir` at
all. A `File` is read/write only; there is no exec-on-a-`File` (spawning a process
is the separate `Exec` capability, below).

Native directory and file capabilities are open handles, not checked path
strings. A `File` retains its already-open parent plus one fixed leaf; a subtree
retains the opened child directory. Renaming or replacing any original path
component therefore cannot redirect a later operation, and write/append refuse
a symlink leaf. Build input/output roots use the same implementation.

## Net: a smaller slice of the network

Rights narrow the *verbs* a `Net` permits (`Connect` vs `Listen`); to narrow its
*reach* - which hosts it may dial - confine its **address-set**. This is the
network counterpart of `dir.subtree`, and it uses typed policy values built on
the capability itself (`Net.tcp(…)`) rather than ad-hoc strings:

```witchy
// `talk_to_db` is handed a Net that can reach exactly one server. Even though
// `main` holds the whole network, the dependency cannot dial anywhere else.
fn talk_to_db(db: Net[Connect, Tcp]):
    let sock = db.connect("10.0.0.5:6379")
    sock.send_line("PING")
```

`net.only(policy)` *intersects* the carried address-set with `policy`; an endpoint
survives only if it was already admitted, so refinement can only ever shrink the
set. `net.deny(policy)` does the opposite - subtracts a slice - and the two chain:
`net.deny(Net.cidr_any("10.0.0.0/8")).only(Net.tcp("192.168.1.1", 80))`
removes a private block, then keeps a single host. The policy constructors are
`Net.tcp(host, port)`, `Net.any_port(host)`, `Net.cidr(block, port)`,
`Net.cidr_any(block)`, and `Net.union(a, b)` for a multi-endpoint set. The host enforces the set **at the
syscall** on both backends, so a narrowed `Net` structurally cannot reach
elsewhere. (HTTPS isn't a separate right: ask for TLS at connect time with a
`tls:` prefix on the address you dial - `net.connect("tls:example.com:443")`.)

For the common SSRF / DNS-rebinding guard, `net.deny(Net.private())` excludes
the internal ranges in one line - loopback, RFC-1918, link-local (including the
`169.254.169.254` cloud-metadata address), CGNAT, and "this host". It is matched
against the **resolved** IP, so a hostname that rebinds to an internal address is
refused at connect time, not just at a check beforehand.

For portable HTTP clients, narrow the host-granted `Fetch` root to one origin.
Unlike the raw-socket fragments above, this complete program runs unchanged in
the browser and on native hosts:

```witchy
import http

fn main(console: Console, fetch: Fetch):
    let target = "https://example.com/status"
    let api = fetch.only(http.origin(target))
    match http.try_get(api, target):
        Ok(response) -> console.print("status ${http.status(response)}")
        Err(error) -> console.print("request failed: ${error}")
```

## Spawning processes: the `Exec` capability

`Exec` is authority to run a *native subprocess* - and it is the single most
dangerous capability, because a spawned process runs with full OS authority
**outside** witchy's sandbox. witchy cannot confine what it spawns, so `Exec` is
kept conspicuous and granted on its own line, never folded into `File`. Two things
keep it honest: the binary is **named through a `Dir[Read]`** (you can only execute
a file you can *read*, opened through the same handle-anchored confinement as
`read`), and the call takes an argv list, never a shell string. The host executes
the already-open file (or a private immutable snapshot on platforms without
descriptor execution), so a concurrent path swap cannot change which program
starts:

```witchy
import exec

// This helper can execute only `git` through `bin`: no shell and no other
// readable executable.
fn run_git(e: Exec, bin: Dir[Read]) -> Int:
    let git = e.only(["git"])
    let (code, _out) = exec.run(git, bin, "git", ["--version"], "")
    code
```

Almost nothing should hold `Exec`; it exists chiefly so a self-hosted tool (like
the package manager driving the compiler) can run a confined subprocess. `witchy
caps` surfaces it like any other authority, and the supply-chain gate treats newly
wanting `Exec` as a serious widening. A grant document's `[exec].runner` entry
sets the root allowlist for a same-named `runner: Exec` parameter. `only`
intersects that policy with the requested names, so a narrower capability can
never regain an executable excluded by its source.

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
    if root.exists("config.toml"):
        Some(ConfigDir(root))
    else:
        None

// A consumer that takes `ConfigDir` *statically* demands the brand, not just any
// `Dir` — and can never reach the raw `Dir` back out.
fn load(c: ConfigDir, name: String) -> String:
    match c:
        ConfigDir(dir) -> dir.read(name)

fn main(console: Console, root: Dir[Read]):
    match config_dir(root):
        Some(c) -> console.print(load(c, "config.toml"))
        None -> console.print("no config.toml here")
```

`witchy caps` still reports `load` as `Dir[Read] (refined: ConfigDir)`. The brand
records intent and tightens the API, but it never hides authority from the
footprint. A library can ship a `Redis` over `Net`, a database handle that
exposes read and write as separate facets, or a mailer over `Net` and a `Secret`.

A plain `type` brand (`type ConfigDir: ConfigDir(Dir)`) is convention only: any
module can construct one. Use `capability` when the brand has to hold as a
guarantee. See [`examples/branded_caps`](https://github.com/insanitybit/witchy/tree/master/examples/branded_caps)
and [`examples/redis_capability`](https://github.com/insanitybit/witchy/tree/master/examples/redis_capability).

## Capabilities that carry policy

A `capability` can carry **state beside** the authority it wraps - a sealed
*record* mixing a host capability with ordinary policy data. A database handle, say,
is a `Net` confined to one server *plus* the table it is scoped to:

```witchy
// A confined `Net` (the hard, audited authority) + a `table` it is scoped to (a
// soft policy the library enforces). Sealed: only this module can mint, refine, or
// destructure one, and its fields are private — reached with `match`, never
// `.field` — so the underlying `Net` can never leak past the policy.
capability Table:
    fetch: Fetch
    name: String

// The only way to make one (sealed constructor).
pub fn open_table(fetch: Fetch, name: String) -> Table:
    Table(fetch, name)

// A query refuses any table but the one the handle carries — the policy lives in
// this one reviewable place.
pub fn count(t: Table, requested: String) -> String:
    match t:
        Table(_, name) ->
            if requested == name:
                "ok: ${requested}"
            else:
                "denied: ${requested}"

fn main(console: Console, fetch: Fetch):
    let users = open_table(fetch.only("https://example.com"), "users")
    // ok: users
    console.print(count(users, "users"))
    // denied: secrets
    console.print(count(users, "secrets"))
```

`witchy caps` sees straight through the record: `Table` audits as exactly `Fetch`,
because the footprint sums its capability-typed fields and the `String` carries no
authority. So you get carried policy with **nothing hidden** - the hard tier (the
`Fetch` is host-confined to one origin) plus a soft, library-enforced tier (the
`table` filter), in one unforgeable value. See
[`examples/carried_state`](https://github.com/insanitybit/witchy/tree/master/examples/carried_state).

## Grantable capabilities: a library's own root authority

The capabilities above all *wrap* a host capability. Sometimes a library wants a
capability that is its own kind of authority - a UI framework's "permission to
request a fetch", say - that `main` receives at the root without it being a
built-in like `Net` or `Dir`. Mark a sealed capability `grantable`:

```witchy
grantable capability UiRoot:
    policy: String

fn policy_of(u: UiRoot) -> String:
    match u:
        UiRoot(p) -> p
```

Now `main` can take a `UiRoot`, and the host mints it from a `[user_caps]` block of
a grant document - the same reviewed launch as `[dirs]`/`[files]`:

```
fn main(console: Console, ui: UiRoot):
    console.print(policy_of(ui))
```

```
# app.grants.toml — witchy sandbox --grants app.grants.toml app.witchy
[user_caps]
ui = { type = "UiRoot", policy = "coven-web" }
```

The rule that keeps this safe: a grantable capability must be **bare** - it may
carry policy data, but *zero* host authority, directly or through any field. A
`grantable` cap that reaches a `Net`/`Dir`/`Secret` is a compile error. So granting
`UiRoot` can never be a disguised `Net` grant, and a version bump that slips a
host-cap field into it cannot widen your root authority behind an unchanged `main`
signature. (A capability that legitimately wraps host authority, like `Table`
above, stays non-grantable: you build it *inside* the program from an explicit
`Net`, where the `Net` shows in the signature.)

Bare grantable caps carry no host authority, so they get their own footprint axis:
`witchy caps` prints a `user caps` line, and requiring a new one counts as a
**widening** - new library-defined authority, and a new package in your trust base,
both of which review (and the coven gate) will flag.

## Withholding authority by structure

Everything above attenuates along *calls* - you weaken a handle as you pass it
on. The same mechanism gives you the strongest possible way to *deny* authority
to a stretch of code: don't pass it. A function or closure that never receives a
capability cannot use it - there is no name to reach, no value to alias, nothing
to forge.

When a region of work must not touch the network (or the clock, or the disk),
lift it into a function that is not given that capability:

```witchy
fn audit_log(console: Console, body: String):
    // Never receives `clock`, so it structurally cannot read the wall clock.
    console.print("audit: ${body}")

fn main(console: Console, clock: Clock):
    let body = "request handled"
    audit_log(console, body)
    console.print("logged at ${clock.now()}")
```

`audit_log` cannot read the clock in *any* execution, under *any* later refactor:
the authority was never handed to it. This is **capture-as-dependency-injection** -
authority comes from *holding* a capability, so the un-bypassable way to deny
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

(Private helpers appear too - the rows are *every* function whose signature
carries a capability; `total` is the union over the entry points.)

And `witchy caps-diff old.witchy new.witchy` understands them. A change from
`Net[Connect]` to `Net[Connect, Listen]` is a *widening* - "this code now
*listens*" - and fails the gate, even though both are "uses the network." The
supply-chain signal is verb-precise, not just kind-precise.

Capabilities can also be *conditional* - held only sometimes, or in one of
several shapes.
