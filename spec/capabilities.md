# Capabilities: the witchy security model

This is the user-facing guide to witchy's capability system — what it
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
| `Dir`, `Dir[Read]`, `Dir[Write]` | a directory **subtree** | `read`, `write`, `append`, `exists`, `is_dir`, `list`, `make_dir`, `subdir` |
| `Exec` | spawn a confined native subprocess | `exec.run(e, dir, path, args, stdin) -> (Int, String)` (std `exec`) |
| `Net`, `Net[Connect]`, `Net[Listen]` (+ `Tcp`/`Udp`/`Uds` transport markers) | the network | `connect`, `listen`, `accept`, `send_line`, `recv_line`, `recv_all`, … |
| `SecretStore` | named secrets provisioned by the host (`--secret`/`--secret-file`/`--signing-key`) | `require(store, name) -> Secret`, `get(store, name) -> Option(Secret)` |
| `Secret` | an Ed25519 seed obtained from a `SecretStore` | `crypto.sign`, `crypto.public_key`, `crypto.reveal` |

A `Dir` is not "the filesystem" — it is one subtree. `read(dir, path)` resolves
`path` relative to the capability and rejects `..`, absolute paths, and
symlinks that point outside the subtree. `subdir(dir, "sub")` mints a new,
smaller capability — handing a callee `subdir(dir, "uploads")` gives it that
folder and nothing else.

`Exec` is the right to spawn a native subprocess — the runtime analog of the
build-time `BuildExec`. It is right-less and carries no payload of its own: the
executable is **named through a `Dir[Read]` argument**, resolved with the same
confinement as `read`, so **you can only execute a file you can read**. The std
`exec` module wraps the low-level primitive as
`exec.run(e, dir, path, args: List(String), stdin) -> (Int, String)`, returning
the child's `(exit_code, stdout-then-stderr)`. `Exec` is the most dangerous
capability — it escapes the WASM sandbox by running native code — so it is
footprinted and gated like any other, the `Dir[Read]` confinement and
argv-only (no shell string) call shape are load-bearing, and almost nothing
should hold it. It exists chiefly so the `witchy` CLI can drive the `witchyc`
compiler; see `rfcs/0004-self-hosted-cli.md`.

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
authority. See `examples/branded_caps/src/branded_caps.witchy`.

## Block firewalls: `retain` / `without`

The patterns above attenuate along *calls*. A `retain`/`without` block attenuates
along *scope*: it carves out a region where some capabilities simply aren't in
scope, no matter what the enclosing function holds — and the type checker
enforces it.

```witchy
fn main(console: Console, clock: Clock):
    without clock:
        // `clock` is walled off here; `now(clock)` would not compile.
        print(console, "this region provably does not read the clock")

    retain console:
        // Only `console` survives; every other capability is dropped.
        print(console, "this region can print and nothing else")

    print(console, "${now(clock)}")
```

`retain:` with no names is a complete sandbox — no authority survives, so the
block is pure computation. The guarantee is sealed against the future: if `main`
later gains a `Net` parameter, the `retain console:` block above still cannot
reach it, because it was never named. A block's authority is fixed by what it
asks for, not by what its scope accumulates.

## Auditing — and what it actually defends

First, be precise about where the *enforcement* lives. At **runtime**, the type
system is the defense, and it is complete on its own: a dependency cannot use a
capability you don't pass it, and it cannot change what it demands without
changing its signatures — which breaks your compile. A malicious version bump
either fails to type-check or sits there unable to act.

```sh
witchy caps program.witchy
```

recomputes the capability footprint **from source**, on two axes — the runtime
authority each public entry point demands, and the **build footprint** (what the
rune's `build` step may do). It is not declared metadata; it cannot drift or lie.
For the runtime axis this is *reporting and governance* over what the types
already guarantee: a one-shot answer to "what would I have to grant this code?"

The **build axis is where the audit is the defense**. A build step runs while a
rune is *built* — outside your type-checked call graph — so a version that newly
wants to `exec` a tool or reach the network at build time is precisely what the
gate exists to catch:

```sh
witchy caps-diff old.witchy new.witchy   # exit 2 if either axis widened
```

A runtime widening prints `WIDENING`; a build-axis widening prints
`BUILD WIDENING` and cannot run until you explicitly grant the new capability.
The package manager applies the same gate to dependencies: `witchy add`/`update`
block on a widening until you approve. Build steps themselves run under confined,
safe-by-default grants — try one with
`witchy build-step <file> [--out <dir>] [--read <dir>] [--env K]... [--exec tool]...`.

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
- Memory is capped; a scheduler can preempt a runaway guest at loop back-edges.
- Concurrency stays *inside* the single VM: `async`/`await`, `spawn`, and
  channels lower to a cooperative executor written in witchy (`std/task`,
  `std/chan`), so concurrent tasks share that VM's one linear memory and one
  capability grant. There is no per-task sandbox boundary — isolating untrusted
  code means running it as its own sandboxed program, not as a task.

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
