# Capabilities: the witchy security model

This is the user-facing guide to witchy's capability system - what it
guarantees, how to read and write capability-typed code, and how to audit and
confine a program. For the design rationale of rights parameters, see
[capability-rights.md](../rfcs/capability-rights.md); for how packages are gated on
their footprints, see [package-manager.md](../rfcs/package-manager.md).

## The one rule

**Authority enters a witchy program in exactly one place: the typed parameters
of `main`.**

```
fn main(console: Console, dir: Dir[Read], net: Net[Connect]):
    ...
```

The host mints these values when the program starts. Inside the program they
behave like ordinary values with three restrictions, all enforced by the
compiler:

1. **They cannot be created.** There is no constructor for `Console` or `Dir`.
   If a function wants to print, it must *receive* a `Console`.
2. **They propagate only as arguments.** No globals, no ambient lookup. A
   function's complete authority is visible in its signature.
3. **They only narrow.** A full `Dir` can be passed where `Dir[Read]` is
   expected (implicitly, at any call boundary) or narrowed explicitly with
   `as` - but a `Dir[Read]` can never become a `Dir[Write]`.

The consequence: **a function with no capability parameters provably has no
effects**, and a function with `Dir[Read]` provably cannot write a file. You
audit witchy code by reading signatures, not by tracing call graphs.

**Vocabulary.** Three words are used precisely throughout the docs: a
**capability** is the unforgeable value itself (`Console`, `Dir`, `Net`); a
**right** is an authority parameter *within* one (`Dir[Read]` grants the
`Read` right); a **verb** is an operation checked against rights (`read`,
`write`, `connect`). So `Dir[Read]` is a capability carrying one right, and
`dir.read(path)` is a verb call that requires it.

## The capability types

| Type | Grants | Operations |
|---|---|---|
| `Console`, `Console[Read]`, `Console[Write]` | read stdin and write stdout | `console.read_line() -> String`, `console.print(s)` |
| `Clock` | read the wall clock | `clock.now() -> Int` (epoch ms) |
| `Env` | read environment variables | `env.get_env(name) -> Option(String)` |
| `Rand` | draw cryptographically-seeded random values from the host | `rand.u64() -> Int`, `rand.below(n) -> Int`, `rand.bool() -> Bool`, `rand.hex(nbytes) -> String` (std `rand`) |
| `Dir`, `Dir[Read]`, `Dir[Write]` | a directory **subtree** | `read`, `write`, `append`, `exists`, `is_dir`, `list`, `make_dir`, `subtree`, `read_file`/`write_file` (→ `File`) |
| `File`, `File[Read]`, `File[Write]` | authority to **one file** (the leaf) | `f.read() -> String`, `f.write(data)` (a `Dir` mints one with `read_file`/`write_file`) |
| `Exec` | spawn a confined native subprocess | `exec.run(e, dir, path, args, stdin) -> (Int, String)` (std `exec`) |
| `Net`, `Net[Connect]`, `Net[Listen]` (+ `Tcp`/`Udp`/`Uds` transport markers) | the network | `connect`, `listen`, `accept`, `send_line`, `recv_line`, `recv_all`, `only`, `deny`, … |
| `SecretStore` | named secrets provisioned by the host (`--secret`/`--secret-file`/`--signing-key`) | `require(store, name) -> Secret`, `get(store, name) -> Option(Secret)` |
| `Secret` | opaque host-held secret material obtained from a `SecretStore` | `crypto.sign`, `crypto.public_key` (Ed25519 signing keys); `server.serve_tls`/`serve_tls_n` consume a TLS private key by opaque reference; `crypto.reveal` (revealable value secrets only - signing keys and use-only secrets are not revealable) |

A `Dir` is not "the filesystem" - it is one subtree. `dir.read(path)` resolves
`path` relative to the capability and rejects `..`, absolute paths, and
symlinks that point outside the subtree. `dir.subtree("sub")` mints a new,
smaller capability - handing a callee `dir.subtree("uploads")` gives it that
folder and nothing else. A `Dir` also carries an **entry policy** ([RFC-0011](../rfcs/0011-capability-refinement.md)) in two dimensions:
name-suffix (`dir.only(Dir.ext(".txt"))` - only `.txt` entries) and entry-kind
(`dir.only(Dir.files())` - only file access, no sub-directory open/create;
`Dir.dirs()` - the mirror). They AND-compose: `dir.only(Dir.files()).only(Dir.ext(".txt"))`
touches only `.txt` files. A non-admitted entry is denied at the access check -
including opening a sub-directory, so `Dir.files()` genuinely confines traversal - and a
subtree inherits the policy. An `ext`-only policy still traverses freely (ext gates
file names, not directories), so `kind` is additive. This is the filesystem analog of
`net.only`/`net.deny`; like `Net`, the raw-string form is a `--net`/config grant, not
a language builtin.

A **`File`** is the *leaf* of the same hierarchy ([RFC-0012](../rfcs/0012-file-capability.md)): authority to one
file, right-typed like `Dir` (`File[Read]`/`File[Write]`). A `Dir` navigates to
one with `dir.read_file("x.txt") -> File[Read]` (must exist) or `dir.write_file("x.txt")
-> File[Write]` (need not), both rejecting `..`/absolute escape exactly as `read`
does; then `f.read() -> String` / `f.write(data)` operate on the leaf with no path
argument. `File[Read]` expresses "this one file, read-only" - the least-authority
form for a single-file need, instead of handing over a whole `Dir`. `main` can
also receive a `File` **directly**: each `--file <path>` grant fills `main`'s
`File` parameters positionally (the i-th `File` param ← the i-th `--file`), so
`main(config: File[Read])` audits as `Console, File[Read]` with no `Dir` at all.

A `Net` likewise carries an **address-set**, narrowed with typed policy values
built on the capability itself: `Net.tcp(host, port)`, `Net.any_port(host)`, `Net.cidr(block,
port)`, `Net.cidr_any(block)`, and `Net.union(a, b)` for a multi-endpoint set.
`net.only(policy)` intersects the carried set with `policy` (each endpoint must
already be admitted); `net.deny(policy)` subtracts one (set difference). Both are
**monotone** - refinement can only ever shrink the set - and enforced **at the
syscall by the runtime** on both backends, so a narrowed `Net` cannot dial
elsewhere. Policy patterns are **scheme-agnostic `host:port`**: HTTPS is not a
right and not an allowlist scheme but a *connect-time* `tls:` choice on the
address you dial (`net.connect("tls:github.com:443")`), terminated on the host -
see [RFC-0009](../rfcs/0009-https-tls-client.md).

**Resolve-once-and-pin.** `connect` resolves a hostname a *single* time: the IP set
checked against the allowlist is the same set dialed - no code path re-resolves
between the check and the connection, so a name cannot rebind to a different
address underneath the check (DNS-rebinding / SSRF). CIDR and IP allowlist entries
are matched against the **resolved IP** (rebinding-proof); a bare-hostname entry is
matched by name and dials all of the name's resolved addresses (an ergonomic
convenience, *not* rebinding-proof). For the common "deny the internal ranges"
guard, `net.deny(Net.private())` excludes loopback, RFC-1918, link-local
(including the `169.254.169.254` cloud-metadata IP), CGNAT, and "this host" -
enforced on the resolved IP, so a name that rebinds to an internal address is
denied at connect time.

Portable HTTP uses the narrower `Fetch` root rather than exposing DNS or
sockets. A Fetch value carries an origin allowlist and can only be narrowed:
`fetch.only(origin)` rejects any origin outside its current authority.
`std/http` accepts Fetch directly, so the same program runs under native and
browser providers. Native code that already holds `Net[Connect, Tcp]` can derive
Fetch with `net.fetch(origin)`; the Net allowlist remains a permanent floor after
DNS resolution, and the provider resolves once before dialing.

```sh
let safe = net.deny(Net.private())
let target = "https://example.com/status"
let fetch = safe.fetch(http.origin(target))
http.try_get(fetch, target)
```

Code which genuinely needs address inspection or raw protocols may still use
`net.resolve` and `connect_pinned`, but those are no longer part of the portable
HTTP surface. See [RFC-0020](../rfcs/0020-rebinding-resistant-http.md) for the
historical socket-level design and
[RFC-0102](../rfcs/0102-portable-roots-and-the-fetch-capability.md) for Fetch.

`Exec` is the right to spawn a native subprocess - the runtime analog of the
build-time `BuildExec`. It is right-less, and its opaque host value carries an
optional executable-name allowlist. A bare host-minted `Exec` is unrestricted;
`exec.only(List(String))` returns the intersection of its receiver's policy and
the requested names. Grant-document `[exec]` entries establish same-named root
bindings. The executable is **named through a `Dir[Read]` argument**, resolved with the same
confinement as `read`, so **you can only execute a file you can read**. The std
`exec` module wraps the low-level primitive as
`exec.run(e, dir, path, args: List(String), stdin) -> (Int, String)`, returning
the child's `(exit_code, stdout-then-stderr)`. `Exec` is the most dangerous
capability - it escapes the WASM sandbox by running native code - so it is
footprinted and gated like any other, the `Dir[Read]` confinement and
argv-only (no shell string) call shape are load-bearing, and almost nothing
should hold it. It exists chiefly so the self-hosted `witchy` package manager
can drive the compiler - the same binary's `compile`/`build-step` verbs - as a
confined subprocess; see [RFC-0004](../rfcs/0004-self-hosted-cli.md).

## Narrowing patterns

```witchy
// Implicit narrowing at a call: more authority stands in for less.
fn load(dir: Dir[Read], name: String) -> String:
    dir.read(name)

fn main(console: Console, dir: Dir):
    console.print(load(dir, "notes.txt")) // full Dir -> Dir[Read] parameter

    // Explicit narrowing when you want to NAME the smaller handle:
    let ro = dir as Dir[Read]
    console.print(ro.read("config.txt"))

    // Subtree attenuation: a smaller world, not just fewer verbs.
    let uploads = dir.subtree("uploads")
    console.print(uploads.read("latest.bin"))
```

`Net` narrows the same way, with typed policy values on the `Net` type itself instead of strings:

```witchy
fn main(console: Console, net: Net):
    // Address-set attenuation: confine Net to one endpoint (scheme-agnostic).
    let db = net.only(Net.tcp("10.0.0.5", 6379))

    // `deny` subtracts a block; refinement only ever shrinks. Chains, too.
    let safe = net.deny(Net.cidr_any("10.0.0.0/8")).only(Net.tcp("192.168.1.1", 80))

    console.print("net confined")
```

Brands go further: wrap a capability in your own type to encode *policy*
(e.g. a `Backup` that only a checked constructor can produce). The footprint
analyzer sees through wrappers, so brands add discipline without hiding
authority. See `examples/branded_caps/src/branded_caps.witchy`.

A `capability` may also carry **state beside** the authority it wraps - a sealed
record mixing host capabilities with ordinary policy data:

```witchy
capability Postgres:
    net: Net[Connect, Tcp]
    table: String
```

`Postgres` holds a `Net` confined to one host (hard, audited authority) plus a
`table` filter the library enforces in its own queries (a soft policy). Because it
is `capability`, it is **sealed**: only its module can mint, refine, or destructure
one, and its fields are private - reachable with `match`, never `.field`, so an
alias can never leak the underlying `Net` past the policy. The footprint analyzer
sums the record's capability fields, so it still audits as exactly `Net` - carried
state, no authority hidden. This is the host-primitive `Net`/`Dir` confinement (hard,
runtime-enforced) plus a library-defined policy tier (soft, sealed-but-correctness-
dependent) in one value. See `examples/carried_state/src/carried_state.witchy`.

## Grantable capabilities - a library's own root authority

A library can define its **own** capability that `main` receives at the root,
without it being a built-in host capability. Mark a sealed capability `grantable`:

```witchy
grantable capability UiRoot:
    policy: String

fn policy_of(u: UiRoot) -> String:
    match u:
        UiRoot(p) -> p
```

A `grantable` capability may appear as a parameter of `main`, and the host mints it
from a `[user_caps]` section of a grant document - the same reviewed-launch
mechanism as `[dirs]`/`[files]`:

```
fn main(console: Console, ui: UiRoot):
    console.print(policy_of(ui))
```

```
# app.grants.toml
[user_caps]
ui = { type = "UiRoot", policy = "coven-web" }
```

The one rule: a root-grantable capability must be **bare** - carrying *zero*
transitive host authority. A `grantable` cap that reaches a `Net`/`Dir`/`Secret`
through any field is a compile error. This is what keeps it honest: granting
`UiRoot` can never be a disguised `Net` grant, and a later version that adds a
host-cap field cannot silently widen root authority behind an unchanged `main`
signature. A capability that legitimately wraps host authority (`Postgres` above)
stays non-grantable - you construct it *inside* the program from an explicit `Net`
root, so the `Net` is visible in the signature.

Because bare grantable caps carry no host authority, they are invisible to the
host-capability footprint - so they get their own axis. `witchy caps` reports a
`user caps` line, and the footprint diff treats a newly-required grantable cap as a
**widening**: it is new library-defined authority, and it puts the declaring
package in the policy trust base, both of which a review (or the coven gate) must
see. This lets a domain like a UI framework define its own reviewable authority
vocabulary (which fetches, which secret inputs, which host ports a component may
*request*) while the language core stays small and real platform authority stays
in the host shell.

A grantable capability enters through `main` (a CLI root, staged from a
`[user_caps]` grant document) **or through an exported root entrypoint** - a
`pub fn export_*(cap: UiRoot, input: String) -> String`. That is how a *browser*
app receives its authority: it has no `main` and no `--grants` launch - the host
shell drives it by calling that pure step function once per event - so the host
mints the bare cap *into the export* each call (the browser mirror of a
`[user_caps]` grant). The rune stays pure (it receives only the minted policy
record and still emits inert effect descriptions), so a UI framework's authority
enters at its true root without the rune ever holding a real platform capability.

## Framework effect authority: capability-safe UI (Glamour)

The grantable mechanism turns "which effects a component may *request*" into typed,
reviewable authority. Glamour - the MVU view framework - is the reference realization.

Glamour's root token is a bare grantable `UiRoot`. The framework alone narrows it into
per-concern child tokens (it declares them, so sealing means only it can mint them):

```
UiFetch          construct an HTTP request   (scope / methods / path prefix)
UiRoute          navigate                    (base path)
UiTimer          arm a timer                 (no shorter than min_ms)
CredentialPort   invoke ONE named host port  (login, a passkey ceremony, promote)
SecretInput      render a host-owned secret field  (form / field)
SecretRef        an opaque handle to a host-held secret value
```

An app composes the object-capability graph by handing each child token to the component
that needs it: a read-only view gets a `UiFetch`; a promote button gets a `CredentialPort`
named `promote` and nothing else.

**Effects are token-gated at construction.** Each sensitive effect description takes its
token as the leading argument, so an unauthorized effect is *unrepresentable* rather than
merely denied at runtime:

```
Cmd.Http(UiFetch, method, url, body, on_done)
Cmd.Nav(UiRoute, path)
Cmd.After(UiTimer, ms, msg)
Cmd.Port(CredentialPort, arg, on_done)               # the port NAME rides the token
Cmd.SubmitSecret(SecretRef, CredentialPort, on_done)
VNode.SecretField(SecretInput, on_ready)
```

A component without a `UiFetch` cannot build an HTTP command; a component holding a
`promote` `CredentialPort` can only ever emit a promote - the name is read out of the
token, never passed as a free string, so authority cannot be widened at the call site.

**Tokens gate construction; the shell still enforces.** A token is compile-time authority
and is *not* serialized - `cmd_to_json` drops it. The capability-holding host shell performs
the effect and re-checks the token's policy (scope/methods/prefix/name) before acting: the
static token and the dynamic shell are defense in depth, not redundancy.

**Secrets stay in host custody.** `secret_input` renders a password field whose typed value
never leaves the host - the shell holds the bytes, and the rune receives only a
non-sensitive status (`Empty`/`NonEmpty`) and an opaque `SecretRef`. `submit_secret` hands
the host-held value to a credential port; the rune sees only the result. Because
`SecretInput`/`SecretRef` are sealed to the framework, a sibling component may hold a
`SecretRef` but cannot unwrap it - so it can never read another component's password, by
construction rather than convention.

Every one of these tokens is a bare grantable cap, so the effect authority a rune
actually uses surfaces on the `user caps` footprint axis, and Coven's block-on-widening
gate covers a dependency that broadens it - the package-manager footprint gate, applied to
UI effects.

## Withholding authority by structure

The patterns above narrow along *calls*. To deny a capability to a region of
code outright, give that work its own function and don't pass the capability: a
function or closure that never receives a capability cannot use it, alias it, or
forge it. This is capture-as-dependency-injection - the strongest firewall witchy
has, because there is no name to reach and no value to smuggle.

```witchy
fn audit_log(console: Console, body: String):
    // No `clock` parameter — structurally cannot read the wall clock.
    console.print("audit: ${body}")

fn main(console: Console, clock: Clock):
    audit_log(console, "request handled")
    console.print("at ${clock.now()}")
```

`audit_log`'s authority is fixed by its signature, not by what its callers hold:
if `main` later gains a `Net`, `audit_log` still cannot dial, because `net` was
never a parameter. The absence of a parameter *is* the boundary.

## Auditing - and what it actually defends

First, be precise about where the *enforcement* lives. At **runtime**, the type
system is the defense, and it is complete on its own: a dependency cannot use a
capability you don't pass it, and it cannot change what it demands without
changing its signatures - which breaks your compile. A malicious version bump
either fails to type-check or sits there unable to act.

```sh
witchy caps program.witchy
```

recomputes the capability footprint **from source**, on two axes - the runtime
authority each public entry point demands, and the **build footprint** (what the
rune's `build` step may do). It is not declared metadata; it cannot drift or lie.
For the runtime axis this is *reporting and governance* over what the types
already guarantee: a one-shot answer to "what would I have to grant this code?"

The **build axis is where the audit is the defense**. A build step runs while a
rune is *built* - outside your type-checked call graph - so a version that newly
wants to `exec` a tool or reach the network at build time is precisely what the
gate exists to catch:

```sh
witchy caps-diff old.witchy new.witchy   # exit 2 if either axis widened
```

A runtime widening prints `WIDENING`; a build-axis widening prints
`BUILD WIDENING` and cannot run until you explicitly grant the new capability.
The package manager applies the same gate to dependencies: `witchy add`/`update`
block on a widening until you approve. Build steps themselves run under confined,
safe-by-default grants - try one with
`witchy build-step <file> [--out <dir>] [--read <dir>] [--env K]... [--exec tool]...`.

## Enforcement: the sandbox

Static checking keeps *your* code honest. To run code you don't trust, compile
it to WebAssembly and let the VM boundary enforce the grant:

```sh
witchy sandbox [--dir <root>] [--net <host:port>]... program.witchy
```

The sandbox computes the program's footprint, shows it, and instantiates the
module with **exactly those host functions linked - nothing else exists to
call**. The enforcement is structural, not a runtime permission check:

- A module that was not granted `Clock` has no `now` import to call;
  instantiation fails if it asks for one.
- The Dir operations are linked *per right*: a module that imports the write
  operation cannot instantiate under a `Dir[Read]` grant.
- A guest `Dir` value is an opaque host reference carrying its confined root,
  rights, and entry policy; the paths never enter guest memory, so a module
  cannot forge or widen one, and every resolution runs the same
  `..`/absolute/symlink confinement as the interpreter.
- Memory is capped; a scheduler can preempt a runaway guest at loop back-edges.
- Concurrency stays *inside* the single VM: `async`/`await`, `spawn`, and
  channels lower to a cooperative executor written in witchy (`std/task`, with
  `std/chan` as the channel-facing surface), so concurrent tasks share that VM's
  one linear memory and one capability grant. There is no per-task sandbox
  boundary - isolating untrusted code means running it as its own sandboxed
  program, not as a task.

A bare `witchy program.witchy` (and `witchy run`) compiles the program to
WebAssembly and executes it under the same wasmtime runtime, granting the
footprint its `main` demands from the ambient environment - a convenience run
for code you trust, not a boundary for code you don't. `witchy sandbox` is the
explicit untrusted-code boundary: it grants nothing by default and links only the
host functions named on its command line. The tree-walking interpreter is not a
user run path - it is the parity oracle, the `comptime` evaluator, the
in-language test runner, and the build-step executor; it confines `Dir` paths
through the same checks so its oracle runs match.

## What capabilities do NOT defend against

Honest limits, so you can reason about the system:

- **The compiler and runtime are trusted.** A bug in the type checker, the
  code generator, or wasmtime is a bug in the security boundary.
- **Granted authority is granted.** If you hand a program `Dir[Write]` on your
  home directory, it can write your home directory. Capabilities make
  authority *visible and minimal*, not magically safe.
- **Covert channels.** Timing and other microarchitectural side channels
  (e.g. Spectre-class) are out of scope.
