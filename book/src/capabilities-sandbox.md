# The Sandbox

Static checking keeps *your* code honest: a function without a `Net` can't
connect, and the compiler proves it. But what about code you didn't write — a
plugin, a dependency, something you fetched? You can't audit it by hand, and even
if you could, the binary you run might not match the source you read.

This is where the WebAssembly backend earns its place. `witchy sandbox` turns
the capability model from a property of the *type system* into a property of the
*runtime*.

## How it works

```sh
witchy sandbox program.witchy
```

This:

1. Computes the program's capability footprint from the source (the same
   analysis as `witchy caps`).
2. Compiles it to WebAssembly.
3. Instantiates the module in a wasmtime VM, linking **exactly** the host
   functions the footprint calls for.

The enforcement is *structural*, not a permission check. A capability isn't a
flag the runtime consults — it's the *presence* of a host function. A program
that wasn't granted `Clock` has no `now` import linked into its module; if it
asks for one, instantiation fails before any code runs. The VM doesn't say "you
may not read the clock" — there is simply no clock to read. Nothing exists to
call.

```sh
# The host backs the Dir with a real directory and the Net with an allowlist.
witchy sandbox --dir ./data program.witchy
witchy sandbox --net api.example.com:443 client.witchy
```

A granted `Dir` is backed by `--dir <root>`; a granted `Net` by one or more
`--net <host:port>` allowlist entries. Whatever the program's footprint *doesn't*
include is simply absent.

## Handles, not pointers

When a program holds a `Dir` inside the sandbox, it doesn't hold a path — it
holds an opaque integer handle into a table the *host* keeps. The path strings
live outside the VM's memory entirely. So a malicious module can't manufacture a
directory by writing bytes into its own memory and casting them: the only way to
get a `Dir` handle is for the host to grant the root, and the only way to get a
narrower one is `subdir`, which the host resolves and confines. The same goes
for `Net` allowlists and sockets.

Path confinement uses the same rules as the interpreter: `..` is rejected,
absolute paths are rejected, and symlinks that escape the subtree are rejected —
checked host-side, where the program can't interfere.

## Why you can trust the sandbox runs your program

Here's the crucial connection. You develop and test against the interpreter. The
sandbox runs the *compiled* version. Why believe they behave the same?

Because of **parity** — the invariant from the introduction. The interpreter and
the WebAssembly backend are held, by the test suite, to produce identical output
on every program, including identical *failures*. A program that traps on an
out-of-bounds index in one traps in the other; a program that prints `42` in one
prints `42` in the other. When the two backends *can't* agree on something, that
is a compile-time error, never a silent difference.

So "I tested it on the interpreter" and "it runs the same in the sandbox" are the
same statement. The confinement you reason about statically is the confinement
you get at runtime, on a binary you can re-derive from source.

## The honest boundaries

A capability system is a strong tool, not a magic one. Worth stating plainly:

- **The compiler and the VM are trusted.** A bug in witchy's type checker, code
  generator, or in wasmtime is a bug in the boundary. The boundary is small and
  testable, which is the point — but it isn't zero.
- **Granted authority is granted.** If you `--dir /` a program, it can read `/`.
  Capabilities make authority *minimal and visible*; they don't make a grant you
  chose to give somehow safe.
- **Side channels are out of scope.** Timing and microarchitectural channels
  (Spectre-class) aren't defended against.

What you get, within those boundaries, is the thing most systems can't offer at
all: the ability to run code whose blast radius is written in its type and
enforced by the machine. There's one more place that boundary matters — between
the concurrent parts of a running program.

Next: concurrency with actors.
