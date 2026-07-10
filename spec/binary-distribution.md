# Binary backend & distribution

Witchy compiles to a WebAssembly **binary**. Codegen lowers the checked AST to a
typed IR (`WirModule`) and `wir_encode` emits the `.wasm` bytes directly
(`compile_module_binary` in `crates/witchy-lower/src/codegen/assembly.rs`); there
is no WAT text stage and no `wat` crate. A compiled program is a portable `.wasm`
module that a trusted `witchy` host instantiates under a delegated capability
grant — the same enforcement as `witchy sandbox`, frozen into an artifact.

## The one run path, and the interpreter's roles

There is one user-program run path: the compiled WASM backend. `witchy <file>`,
`witchy run`, and `witchy sandbox` all compile to a wasm binary and execute it
under wasmtime, so dev == deploy by construction.

The tree-walking interpreter is **not** a user run path. It is the parity oracle
(`witchy parity`), the `comptime:` const-evaluator, the in-language test runner,
and the effectful build-step executor. Because `comptime` blocks are evaluated at
compile time through the interpreter (`interpreter::run_module_budgeted`, called
from `linker::link`), the evaluator is a compile-time dependency of every build
that compiles witchy — including the wasm32 playground build — not a strippable
oracle-only component. The CLI/oracle-only entry points are unreferenced from the
wasm library and dropped by `wasm-opt`, but the core evaluator stays linked.

## Distribution: one artifact, two hosts

The compile artifact is **`app.wasm`**: the program module, importing the
`"witchy"` capability host and carrying a versioned `witchy.launch` custom
section with `main`'s source-derived host-capability contract. Imports are the
executable authority floor — a module cannot call a host op it does not import —
while the launch section preserves declared capability parameters even when
lowering eliminates every use.

```
codegen:  checked AST → WirModule → wasm binary        (no WAT anywhere)
artifact: app.wasm     ("witchy" host imports)

run it:   witchy app.wasm --dir . --net api:443        host = consumer's witchy
browser:  app.wasm + web/witchy-host.js (loader)       host = JS (deny-by-omission)
```

| Host | Provides the `witchy.*` imports + enforces grants |
|---|---|
| native `witchy` / pm / build | wasmtime + `crates/witchy-runtime/src/runtime.rs` |
| browser | JS — `web/witchy-runtime/` (no wasmtime in a browser) |

- **Portable module.** `witchy emit-wasm app.witchy [-o app.wasm]` produces the
  binary; `witchy app.wasm` (or `witchy sandbox [--dir <root>] [--net
  <host:port>]... app.wasm`) runs it. The installed `witchy` binary is the host:
  it reads the module's `witchy.launch` contract and `witchy.*` imports, unions
  their requirements, and links those families (Dir/Net at the `--dir`/`--net`
  roots, default cwd; secrets from `--secret`/
  `--secret-file`/`--signing-key`), and a module importing an ungranted op fails
  to instantiate. `precompiled_wasm_runs_like_the_source` checks that a
  precompiled module runs like its source. Legacy and external wasm modules with
  no launch section retain import-derived classification; malformed or unknown
  Witchy launch metadata is rejected instead of silently ignored.
- **Browser host.** Ship `app.wasm` + `web/witchy-host.js` as the loader. The
  pure-compute JS host provides only infrastructure imports and omits every
  capability import, so a capability-using module fails to instantiate
  (deny-by-omission — see [`wasm-abi.md`](wasm-abi.md)).

The capability guarantee travels with the artifact: the host links only the
imported families, grants are launch-time, and secret bytes and `Dir`/`Net`
confinement go through the same `runtime.rs`/`confine.rs` as `witchy sandbox`. A
"binary" does not trade away the security model — it is `witchy sandbox` frozen.

> **No `build-exe`.** Witchy does not bundle a program plus an embedded runtime
> into one self-contained executable. That would collapse the trust boundary: the
> runtime that sandboxes untrusted code would be supplied by the program's author,
> who could ship one that ignores the capability flags. The model holds only when
> the runtime is the **consumer's** trusted `witchy`. Distribution is therefore:
> ship the `.wasm`, and the consumer runs it with their own `witchy`.

## Invariants

- **Two independent semantics + the parity gate.** The interpreter oracle is the
  independent check on the hand-written codegen; it is never replaced by running
  the same wasm on a second engine.
- **One confinement implementation, shared.** The `Dir`/`Net` escape checks
  (`resolve`/`resolve_write`) live in one module,
  `crates/witchy-runtime/src/confine.rs`, used by the wasmtime sandbox and the
  interpreter alike — never a second copy. A capability-granting browser host
  delegates path checks to the same code compiled to wasm.
- **Authority is source-derived and host-mediated.** A program's authority is its
  host-import set plus the footprint from `capabilities::analyze`; wasmtime
  validates the binary module and links only the granted host functions.
- **Secret bytes never enter guest memory.** The runtime host holds secret
  material — signing keys, revealable value secrets, and TLS private keys — and
  exposes each only through the operations that consume it (by handle for signing
  and TLS serving; `crypto.reveal` for revealable value secrets, which errors on
  signing keys and use-only secrets). No secret's raw bytes are copied into guest
  linear memory.
- **No silent capability grants.** A host links only the host functions the
  footprint declares; concrete resources (`--dir`, `--net`, `--secret`,
  `--secret-file`, `--signing-key`) are granted at launch.

## Out of scope

- **WASI retarget** — mapping the effect imports onto WASI preview 2
  (`wasi:filesystem`/`sockets`/`clocks`) so a witchy `.wasm` runs on generic
  runtimes (wasmtime CLI, jco, wasmCloud) with no witchy host. Witchy's
  `Dir`/`Net` grants map onto WASI's own capability model. Buys ecosystem
  portability; the distribution story above needs none of it.
- **Runtime-only launcher.** Shipping a per-target `cwasm` plus a slim launcher is
  additive future work; the portable artifact remains `app.wasm` plus the
  consumer's trusted Witchy host.
