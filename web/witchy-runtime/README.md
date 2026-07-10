# witchy-runtime — a pure-compute JavaScript host for witchy-WASM

`witchy-runtime.mjs` runs witchyc-compiled WASM in JavaScript (browser or Node)
with **every capability denied**. It is the browser analog of the wasmtime host
in `crates/witchy-runtime/src/runtime.rs`, with the capability set fixed to
empty — the implementation of
[RFC-0007](../../rfcs/0007-witchy-wasm-browser-target.md). The ABI it targets is
the public contract in [`spec/wasm-abi.md`](../../spec/wasm-abi.md).

This is a *general* witchy-WASM browser runtime, not specific to any one app. It
is distinct from the `web/witchy-host.js` playground shim, which compiles
snippets via a Rust-built `witchy.wasm` lib and delegates the pure helpers back
to that lib; this runtime is standalone and implements the pure helpers in JS.

## The guarantee: deny-by-omission

witchyc tree-shakes imports — a module declares only the host functions it
reaches. A rune that touches the filesystem, network, clock, etc. imports a
capability function this runtime **does not provide**, so
`WebAssembly.instantiate` throws a `LinkError` and the module never runs. The
host therefore admits no authority-bearing module. It is deliberately stricter
than "all footprint-empty modules": native-only launch and toolchain services
such as argv and `std/compiler` are absent too. No trap stubs are involved —
the imports are simply absent.

## API

```js
import { instantiate } from "./witchy-runtime.mjs";

const { run, output } = await instantiate(wasmBytes, {
  onPrint: (line) => console.log(line), // optional; else lines collect in `output`
});
run();          // calls the module's exported `run`; returns the output array
```

`instantiate(wasmBytes, opts) -> { instance, output, run, callString, memory }`:

- `wasmBytes` — a witchyc-compiled module (`witchy compile foo.witchy --out
  foo.wasm`).
- `opts.onPrint(line)` — called once per printed line; if omitted, lines
  accumulate in the returned `output` array.
- `opts.nodeCrypto` / `opts.cryptoBackend` — override the crypto backend (Node's
  `node:crypto` is auto-detected; SHA-256/HMAC/`rune_hash` work with no backend).

The exported `WITCHY_ABI_VERSION` is the ABI version this runtime implements;
the Rust catalog test compares it with the compiler-owned version.

### `callString` — the `String -> String` export ABI (RFC-0008)

A `pub fn export_*(s: String) -> String` compiles to a JS-callable export. The
runtime's `callString` drives it (writes the input String header via `__galloc`,
calls `__export_*`, reads the result String header back) — pure mechanics over
guest memory, no capability:

```js
const { callString } = await instantiate(wasmBytes);
const out = callString("__export_export_step", '{"model":0}'); // -> result string
```

`callStringExport(wasmBytes, exportName, str, opts)` is a one-shot convenience
(instantiate + call). See [`spec/wasm-abi.md`](../../spec/wasm-abi.md)
§"String-export entry points".

### `glamour-dom.mjs` — the DOM host shell (RFC-0008)

`glamour-dom.mjs` is the capability-holding shell that runs a glamour MVU rune in
a browser: `mount(wasmBytes, rootElement, opts)` instantiates the rune under this
pure-compute host, calls its `export_step` to render, diffs the returned VNode
into the real DOM (`createElement` / `textContent` / `setAttribute` only — never
`innerHTML`), and wires `on` attributes as `addEventListener` handlers that route
events back as `Msg` values and re-render. The witchy rune computes; the shell
acts (holds the DOM, the events, and the effects).

**Effects as data.** Each `export_step` also returns a `cmd` — the effect the rune
*described* but cannot perform (it holds no capability). The shell interprets it:
`{"cmd":"none"}` does nothing; `{"cmd":"after","ms":N,"msg":…}` arms a timer
(`opts.setTimeout`, defaulting to the global `setTimeout`, injectable for tests)
and dispatches the deferred `msg` back into the loop when it fires;
`{"cmd":"batch","cmds":[…]}` interprets each. The timer is the *shell's* authority;
the rune only emitted the description. The `autocounter` example
(`projects/glamour/examples/autocounter/`) demonstrates this — its count
auto-increments once a second via `After(1000, Tick)`, with a footprint-empty rune.
`glamour-dom-timer.test.mjs` (and `tests/glamour_dom.rs`) prove it headlessly with
a fake, controllable clock.

## What it provides — and refuses

**Provides (no authority):** `print` / `print_int` / `print_float` (capturable
output), `fill_pending` / `write_pending_list` (the string bridge),
`float_to_str`, `string_from_code`, `encoding` (hex/base64/base64url),
`regex_match_spans_len`, the `crypto.*` digests/verifies, launch-provided
`user_cap_field_len`, the reflection field-length stubs, and runtime abort
diagnostics. The exported `WITCHY_BROWSER_IMPORTS` list is checked against both
the compiler catalog and the actual JavaScript import object.

**Refuses (absent):** all capability-authority imports (`dir_*`, `net_*`,
`exec_run`, `now`, `env_*`, secrets, build authority, signing) plus unsupported
native launch/toolchain services such as `args_size` and `compiler_*`. A module
importing any of these cannot instantiate.

See [`spec/wasm-abi.md`](../../spec/wasm-abi.md) for the full import table and the
pending-buffer protocol.

## Crypto

The host functions are synchronous (the guest expects results written before the
call returns), so the async WebCrypto `subtle.digest` cannot back them. This
runtime carries a self-contained synchronous **SHA-256** (enough for
`crypto.sha256`, `crypto.hmac_sha256`, and `crypto.rune_hash` with zero
dependencies) and defers the wider set (`sha512`, `sha3_256`, the verifies) to an
injected backend — Node's `node:crypto` by default. In a plain browser those
wider algorithms need an injected `cryptoBackend`; using one without it raises a
clear error rather than producing wrong output.

## Spike

`spike.mjs` is the RFC-0007 proof, runnable directly:

```sh
node web/witchy-runtime/spike.mjs            # uses ./target/debug/witchy
node web/witchy-runtime/spike.mjs path/to/witchy
```

It compiles a footprint-empty rune, runs it under this runtime, asserts the
output matches the native interpreter run byte-for-byte, and confirms a
capability-using rune is refused with a `LinkError`. The Rust test
`tests/browser_shim.rs` drives the same spike (skipping cleanly if `node` is
absent).
