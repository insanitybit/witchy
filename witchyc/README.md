# Witchy

`witchyc` is the compiler binary. `grimoire` is the developer-facing project CLI and Coven is the signed package registry server. The Rust package and this source checkout retain their internal names.

Witchy compiles a checked source subset directly to core WebAssembly. Wasmtime
executes the result with explicit capability grants and bounded resources. The
compiler checks selected definitions, including unused functions and unreachable
statements, before emission. The [native targets](docs/native-backend.md) compile a
smaller subset to macOS and Linux ARM64 executables. Both install a sandbox
before entering Witchy code, with runtime grants from the checked entry point.

The central foundation is a small proof-required memory layer. Generic
`memory.get_unchecked` returns a CopyStorage element only inside an explicit
Witchy contract and `proof: auto` context whose actual bounds, initialization
and declared postconditions are kernel-checked. The ordinary safe `vec_get`
checks logical length, actual capacity and occupancy before calling it.
`memory.init_unchecked` additionally requires an exclusive mutable borrow, proven
bounds and an empty slot; `vec_push_spare_mut` establishes those facts before
initializing and advancing length. Users
write Witchy, not companion Lean. See [proof-required memory](docs/wasm-proof-memory.md).

These are bounded typed-buffer operations, not arbitrary unsafe pointers. Ordinary
generic Vec and UTF-8 String source lives in `library/wasm-vec.wy` and
`library/wasm-string.wy`. Vec is a sealed type with a stated invariant
(`memory.prefix(data, length, capacity)`); every Vec and String function that uses
an unchecked operation carries a kernel-checked contract, elements may be owners
(a `Vec<String>` works), and functions that only borrow a Vec are proved from its
methods' contracts (`docs/wasm-scalar-proofs.md`). Not proved: termination, and
anything about the compiler, allocator or runtime themselves.
[The Witchy Book](book/README.md) is a guided introduction.
[`docs/language.md`](docs/language.md) has a syntax tour and [feature index](docs/language.md#feature-index)
with links to language guides and runnable fixtures. [`docs/progress.md`](docs/progress.md) is the current account of what works, what is
proved and what is trusted; the older [language and trust boundary](docs/wasm-cutover.md)
overview is kept for the route's history.

## Build and run

Use MBX and independently owned build targets. The compiler and Wasmtime host
are separate crates:

```sh
mbx build --offline --target-dir /absolute/owned-compiler-target
mbx build --manifest-path tools/wasmtime-host/Cargo.toml --release --offline \
  --target-dir /absolute/owned-runtime-target
export WITCHY_WASMTIME_HOST=/absolute/owned-runtime-target/release/witchy-wasmtime-host
/absolute/owned-compiler-target/debug/witchyc build examples/wasm-scalar.wy -o scalar.wasm
$WITCHY_WASMTIME_HOST scalar.wasm --output
```

The runtime is that one Rust binary; nothing else is installed or started.
Building grants no runtime authority. Grant `--output` and `--allocator` only
when needed. The runtime checks the module against the closed binary profile,
then runs it in Wasmtime without WASI, and with filesystem, network and other host
access only as the command line grants. Fuel, memory, stack,
output and deadline limits are documented in [production builds](docs/wasm-production-build.md).

Proof-free builds need no Lean installation. Proof-bearing builds use trusted
caller configuration: `WITCHY_WASM_LEAN_ROOT` for fresh checking, or
`WITCHY_WASM_READY_BINDING` for an explicitly prepared selected helper/SDK
generation. Invalid configured bindings refuse without fallback. Selected
sessions require the operator to keep a distinct-inode readonly SDK unchanged
throughout the logical session, including renewal gaps, until final helper cleanup; permissions do not establish verified immutability or
currentness of the original SDK path. Follow the production guide for setup,
receipts, atomic publication, watch/build-session renewal and lifecycle limits.

## Development

[Focused development](docs/development.md) lists current tests and validation
boundaries. [Proof architecture](docs/proof-architecture.md) describes the trusted
compiler/runtime boundary; [modular calls](docs/modular-calls.md) distinguishes
body proofs, actual caller obligations and bounded summary reuse.

The frontend parses Witchy source before the [shared compiler](src/compiler/)
checks types, ownership and proof obligations. That compiler contains both the
[Wasm emitter](src/compiler/emit/) and [Cranelift lowering](src/compiler/native/).
The [native guest runtime](runtime/native/) is linked into native executables.
The [Wasmtime host](tools/wasmtime-host/) runs Wasm as a separate executable.

Cargo runs library unit tests, `wasm_scalar`, and `wasm_cli`. The CLI suite also
includes native executable tests on supported hosts. See the [retirement
record](docs/legacy-retirement.md) for historical routes and suites.

Ordinary `vec_push_spare` now uses proof-required generic initialization in its
actual consuming body, with checked success and unchanged failure contracts;
`vec_push` uses that path around its existing checked growth. `vec_get` uses the
proof-required read. This is a bounded memory-safe operation slice. The actual reserve ownership
handoff is proved below, and the Vec invariant is stated on the sealed type and shown by every function that builds or changes one. The actual reserve copy
loop uses general source-authored invariants checked for entry, preservation and
exit; errors may leave a partial fresh destination for checked cleanup. See
[proof-required memory](docs/wasm-proof-memory.md).

Actual `vec_reserve` and cleanup bodies now carry general allocation/ownership
proofs, including real failure handoff, preserved logical elements and closure of
tracked allocations. This extends the loop proof without claiming total
termination or physical allocator refinement.

Build the book website locally with Python 3 and mdBook.

```sh
python3 book/build-site.py /tmp/witchy-book-site
python3 -m http.server 8765 --bind 127.0.0.1 --directory /tmp/witchy-book-site
```

Then open `http://127.0.0.1:8765/`.
