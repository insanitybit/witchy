# Capabilities: the witchy security model

This is the user-facing guide to witchy's capability system — what it
guarantees, how to read and write capability-typed code, and how to audit and
confine a program. For the design rationale of rights parameters, see
[capability-rights.md](capability-rights.md); for how packages are gated on
their footprints, see [package-manager.md](package-manager.md).

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
   `as` — but a `Dir[Read]` can never become a `Dir[Write]`.

The consequence: **a function with no capability parameters provably has no
effects**, and a function with `Dir[Read]` provably cannot write a file. You
audit witchy code by reading signatures, not by tracing call graphs.

## The capability types

| Type | Grants | Operations |
|---|---|---|
| `Console` | write to stdout | `print(console, s)` |
| `Clock` | read the wall clock | `now(clock) -> Int` (epoch ms) |
| `Env` | read environment variables | `get_env(env, name) -> Option(String)` |
| `Dir`, `Dir[Read]`, `Dir[Write]` | a directory **subtree** | `read`, `write`, `exists`, `is_dir`, `list`, `make_dir`, `subdir` |
| `Net`, `Net[Connect]`, `Net[Listen]` (+ `Tcp`/`Udp`/`Uds` transport markers) | the network | `connect`, `listen`, `accept`, `send_line`, `recv_line`, `recv_all`, … |
| `SigningKey` | an Ed25519 private key | `crypto.sign`, `crypto.public_key` |

A `Dir` is not "the filesystem" — it is one subtree. `read(dir, path)` resolves
`path` relative to the capability and rejects `..`, absolute paths, and
symlinks that point outside the subtree. `subdir(dir, "sub")` mints a new,
smaller capability — handing a callee `subdir(dir, "uploads")` gives it that
folder and nothing else.

## Attenuation patterns

```witchy
// Implicit narrowing at a call: more authority stands in for less.
fn load(dir: Dir[Read], name: String) -> String:
    read(dir, name)

fn main(console: Console, dir: Dir):
    print(console, load(dir, "notes.txt"))   // full Dir -> Dir[Read] parameter

    // Explicit narrowing when you want to NAME the smaller handle:
    let ro = dir as Dir[Read]
    print(console, read(ro, "config.txt"))

    // Subtree attenuation: a smaller world, not just fewer verbs.
    let uploads = subdir(dir, "uploads")
    print(console, read(uploads, "latest.bin"))
```

Brands go further: wrap a capability in your own type to encode *policy*
(e.g. a `Backup` that only a checked constructor can produce). The footprint
analyzer sees through wrappers, so brands add discipline without hiding
authority. See `examples/branded_caps.witchy`.

## Auditing

```sh
witchy caps program.witchy
```

recomputes the program's capability footprint **from source** — per public
function, per right. It is not declared metadata; it cannot drift or lie.

```sh
witchy caps-diff old.witchy new.witchy   # exit 2 if authority widened
```

is the CI gate: a change that grows the footprint (a new capability kind, or a
new right on an existing one) fails loudly and shows the diff. The package
manager applies the same gate to dependencies: `witchy add`/`update` block when
a rune's footprint widens until you explicitly approve.

## Enforcement: the sandbox

Static checking keeps *your* code honest. To run code you don't trust, compile
it to WebAssembly and let the VM boundary enforce the grant:

```sh
witchy sandbox program.witchy [--dir <root>] [--net <host:port>]...
```

The sandbox computes the program's footprint, shows it, and instantiates the
module with **exactly those host functions linked — nothing else exists to
call**. The enforcement is structural, not a runtime permission check:

- A module that was not granted `Clock` has no `now` import to call;
  instantiation fails if it asks for one.
- The Dir operations are linked *per right*: a module that imports the write
  operation cannot instantiate under a `Dir[Read]` grant.
- A guest `Dir` value is an opaque handle into a host-side path table; the
  paths never enter guest memory, so a module cannot forge or widen one, and
  every resolution runs the same `..`/absolute/symlink confinement as the
  interpreter.
- Memory is capped; a scheduler can preempt runaway actors at loop back-edges.

The interpreter (`witchy program.witchy`) enforces capabilities at the type
level and confines `Dir` paths identically, but it is a development runtime,
not a security boundary — untrusted code belongs in the sandbox.

## What capabilities do NOT defend against

Honest limits, so you can reason about the system:

- **The compiler and runtime are trusted.** A bug in the type checker, the
  code generator, or wasmtime is a bug in the security boundary.
- **Granted authority is granted.** If you hand a program `Dir[Write]` on your
  home directory, it can write your home directory. Capabilities make
  authority *visible and minimal*, not magically safe.
- **Covert channels.** Timing and other microarchitectural side channels
  (e.g. Spectre-class) are out of scope.
