# The Sandbox

Static checking keeps *your* code honest: a function without a `Net` can't
connect, and the compiler proves it. But what about code you didn't write — a
plugin, a dependency, something you fetched? You can't audit it by hand, and even
if you could, the binary you run might not match the source you read.

The WebAssembly backend enforces the same capability model at runtime. `witchy
sandbox` compiles the program and controls which host functions its VM can call.

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
asks for one, instantiation fails before any code runs. No clock import exists
for the program to call.

```sh
# The host backs the Dir with a real directory and the Net with an allowlist.
witchy sandbox --dir ./data program.witchy
witchy sandbox --net api.example.com:443 client.witchy
witchy sandbox --fetch https://api.example.com portable-client.witchy
```

A granted `Dir` is backed by `--dir <root>`; a granted `Net` by one or more
`--net <host:port>` allowlist entries; a granted `Fetch` by one or more explicit
`--fetch <scheme://host:port>` origins; a single file by `--file <path>` (filling
a `main(config: File[Read])` parameter). Anything omitted from the program's
footprint is absent from the VM. Fetch origins are canonicalized before launch;
malformed origins and a required Fetch with no origin both fail before execution.

## Grant documents

A named Fetch parameter binds to the same key in a reviewable grant document:

```toml
[fetch]
api = ["https://api.example.com"]
```

For `main(api: Fetch)`, the host mints exactly that origin-scoped root. The
section confers `Fetch`, never raw `Net`.

Flags don't scale once a program needs several files, a couple of directories, a
host, and a secret. A **grant document** enumerates the whole grant as reviewable,
diffable data:

```toml
# app.grants.toml — the authority the host hands to `main`, bound by parameter name
[files]
config = { path = "config.toml", rights = ["Read"] }
[dirs]
data = { root = "./data", rights = ["Read", "Write"] }
[net]
api = ["api.example.com:443"]
[env]
environment = ["HOME", "LANG"]
[exec]
runner = ["bin/git"]
[secrets]
token = { from = "env:API_TOKEN" }    # the host resolves it; never inlined
```

```sh
witchy sandbox --grants app.grants.toml program.witchy
```

Each `Dir`/`File` parameter of `main` binds to the document entry of the *same
name* (`[files].config` → the `config` parameter). `[net]`, `[fetch]`, `[env]`,
and `[exec]` entries likewise bind same-named policy-carrying parameters. And
because witchy already *computes* a program's footprint, the grant is
**cross-checked against it**: a grant
that asks for authority the code never exercises is a warning (the classic
over-permission smell), and a grant that withholds authority the code needs is a
hard error before launch — so "approve this program's permissions" becomes a diff
against what the code actually does, not blind trust. The same check runs
standalone: `witchy grants-check program.witchy app.grants.toml`.

At launch the grant is printed as a reviewable diff — each capability and its
`dir`/`file`/`net`/`fetch`/`env`/`exec`/`secret` binding — and on an
interactive terminal you are
prompted to approve it before any authority is handed over. Pass `--accept-grants`
to skip the prompt for non-interactive launches (CI, installers):

```sh
witchy sandbox --grants app.grants.toml --accept-grants program.witchy
```

## Host-held references, not guest pointers

When a program holds a `Dir` inside the sandbox, it doesn't hold a path — it
holds an opaque WebAssembly `externref` to an authority object owned by the
*host*. Paths, address/name/program allowlists, streams, listeners, and secret
bytes stay
outside guest linear memory. Corrupting or fabricating an integer in memory
therefore cannot mint a `Dir`, `Net`, socket, listener, or `Secret`. The host
creates the root reference from a launch grant; operations such as `subtree`,
`read_file`, `net.only`, `env.only`, and `exec.only` return narrower host
objects as new references.

Path confinement is handle-anchored on native hosts. The launcher consumes an
ambient path once to open each root grant; every `subtree`, `File`, read, write,
append, existence check, directory check/list, and directory creation after that
resolves relative to an open directory handle. `..` and absolute paths are
rejected, escaping symlinks are rejected, and replacing any parent path component
cannot redirect an operation. Writes also refuse a symlink leaf. The interpreter
and compiled host carry the same `ConfinedDir`/`ConfinedFile` objects, so this
security boundary is shared rather than reimplemented for parity.

Executable selection uses descriptor execution or an already-open private
snapshot. On macOS, platform binaries may instead use a path only after Witchy
verifies that its opened identity still matches and that every ancestor is
root-owned and non-writable; mutable grant paths cannot redirect execution.

## Console input is separate authority

Bare `Console` carries both rights for compatibility. A logger needs only
`Console[Write]`; input code asks for `Console[Read]`, so receiving typed input
does not silently give a library an output channel:

```witchy
fn main(input: Console[Read], output: Console[Write]):
    let name = input.read_line()
    output.print("hello, ${name}")
```

The native provider reads stdin. The browser provider consumes explicit
page-supplied lines; when its finite fixture is exhausted, `read_line` returns
the empty string.

## Why you can trust the sandbox runs your program

The everyday `witchy program.witchy` run and `witchy sandbox` compile your program
to the same WebAssembly; the sandbox just grants narrower authority. A sandboxed
deploy therefore runs the same binary you developed against, not a second build
you hope agrees with the first.

That compiled backend is held honest by **parity**, the invariant from the
introduction. The test suite runs every program on both the WebAssembly backend
and a reference tree-walking interpreter and requires identical output, including
identical failures. A program that traps on an out-of-bounds index in one traps
in the other; one that prints `42` in one prints `42` in the other. When the two
can't agree, that is a compile-time error, never a silent difference.

So the confinement you reason about statically is the confinement you get at
runtime, on a binary you can re-derive from source.

## The honest boundaries

The boundary has explicit limits:

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

Next: concurrency with async and channels.
