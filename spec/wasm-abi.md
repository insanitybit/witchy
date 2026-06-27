---
verified: 0783c22
---

# The `"witchy"` WASM import ABI

witchyc-compiled modules reach the outside world through a single WASM import
module named `"witchy"`. Each import is a host function the runtime must supply;
**a granted host function is a capability** (or a piece of pure infrastructure).
This is the handshake between the compiler
(`crates/witchy-lower/src/codegen.rs` emits the imports) and a host that
satisfies them (`crates/witchy-runtime/src/runtime.rs` is the wasmtime host;
`web/witchy-runtime/witchy-runtime.mjs` is the JavaScript pure-compute host).

This document is that import surface as a **stable public contract**. A host —
the browser pure-compute runtime, or any third-party tool — depends on the names,
signatures, and marshaling protocol below; a compiler change to any of them is a
breaking ABI change that must bump `WITCHY_ABI_VERSION`.

## ABI version

The current ABI version is **1**. The JavaScript host pins it as
`WITCHY_ABI_VERSION` (exported from `web/witchy-runtime/witchy-runtime.mjs`). The
version covers: the import module name `"witchy"`, the set of import names and
their `(params) -> results` signatures, the value/memory representation, and the
pending-buffer protocol. Bump it in lockstep with any change to those.

## Import inclusion is tree-shaken

A compiled module declares **only the imports it actually reaches**, not the full
set. The codegen path (`assemble_wir_module` in `src/codegen.rs`) prunes the
import list to the host functions the reachable code calls. A footprint-empty
("pure") rune therefore imports only the non-capability functions it uses; a rune
that touches the filesystem/network/clock additionally imports the corresponding
capability function.

This is what makes the browser target's containment **structural**. The browser
host (`web/witchy-runtime/`) provides only the non-capability imports below. Per
the WebAssembly spec, a module that imports a function the host does not supply
**fails to instantiate** with a `LinkError`. So:

- a pure rune imports only functions the host provides → it instantiates and runs;
- an impure rune imports a capability function the host does **not** provide →
  `WebAssembly.instantiate` throws, and the module never runs.

The host is a sieve that admits exactly the footprint-empty modules. This is
**deny-by-omission**: capabilities are denied by simply not being on offer, the
strongest "structurally incapable of I/O" guarantee. No trap stubs are needed (or
installed) for capability imports — their *absence* is the guarantee.

## Module exports

A compiled module exports:

- `memory` — the linear memory the host reads/writes for marshaling.
- `run` — the no-arg entry the host calls to run the program. The compiler
  synthesizes it around `main`, supplying `main`'s parameters (a Console is
  type-level; `args`/`Dir` parameters are host-provided). Present only when the
  module has a `main`.
- `__witchy_reowns`, `__region_copy_bytes` — diagnostic globals (optional).

### String-export entry points (the `String -> String` call ABI)

A `pub fn` whose name starts with `export_` and has the shape
`(String) -> String` is a **JS-callable string export**: the compiler emits a
stable export wrapper plus a bump allocator so a host (the browser pure-compute
shim, the glamour DOM shell — RFC-0008) can call a pure witchy function with a
JSON string in and a JSON string out. These are **exports, not imports**: they
grant **no** authority — the wrapper only reads/writes guest memory.

- `__galloc` — `(i32 len) -> i32 ptr`. Reserves `len` bytes on the bump heap
  (`ensure` + advance `$heap`) and returns the pointer, so the host can write an
  input String header into guest memory before the call.
- `__export_<name>` — `(i32 in_ptr, i32 in_len) -> i32 out_ptr`, one per string
  export (the linker's `{module}.` prefix is dropped: `export_step` ->
  `__export_export_step`... i.e. `__export_<unqualified-source-name>`). The host
  `__galloc`s `4 + len` bytes, writes a String header `[i32 len][bytes]` at the
  returned pointer, then calls `__export_<name>(ptr, len)`. The wrapper forwards
  `in_ptr` to the witchy function (whose single `String` parameter **is** that
  header) and returns a pointer to the result String header `[i32 len][bytes]`,
  which the host reads back. `in_len` is accepted for ABI symmetry; the header is
  self-describing. This call path adds **no** import, so a string-export module
  stays footprint-empty and instantiates under the deny-all host.

The JS host's `callString(instance, exportName, str) -> str`
(`web/witchy-runtime/witchy-runtime.mjs`) implements this protocol.

## Value & memory representation

- **String**: a header `[i32 len][len bytes of UTF-8]` at a guest pointer. `len`
  is a little-endian `i32`; the bytes follow immediately.
- **List(String)**: `[i32 count][count × i64 element pointers]`. Each `i64` slot
  holds the absolute guest pointer of a String header (above).
- All multi-byte integers are **little-endian**. Pointers are `i32` byte offsets
  into `memory`.

## The pending-buffer protocol (the string-bridge)

Host→guest values of dynamic size cross the boundary in **two calls**, so the
data is read once with no time-of-check/time-of-use gap:

1. The guest calls a sizing import (e.g. `regex_match_spans_len`,
   `dir_read_len`, `args_size`). The host computes the result, **stashes the
   bytes in a one-slot `pending` buffer**, and returns the byte length.
2. The guest allocates that many bytes, then calls the matching drain import,
   which copies the staged bytes into guest memory and clears the buffer:
   - `fill_pending(out_ptr)` — writes the staged **bytes** at `out_ptr` (for
     String results).
   - `write_pending_list(base_ptr)` — lays a staged **List(String)** out at
     `base_ptr`: the `[count][count × i64 ptr]` header followed by the string
     objects, each slot pointing at its `[len][bytes]`.

Some imports instead take an `out_ptr` and write a **fixed-size** result directly
(no staging) — e.g. `crypto.sha256` writes 64 hex bytes, `float_to_str` writes the
decimal and returns its length. The guest reserves a sufficient buffer first.

The pure-compute host implements `fill_pending` (drains a staged String, used by
`regex_match_spans_len`) and `write_pending_list` (a no-op there, since no pure
sizing import stages a list). It does **not** implement the capability sizing
imports (`dir_read_len`, `net_recv_*_len`, `args_size`, …), so a module using one
cannot instantiate.

## The imports

ABI version 1 declares **58 imports** (`IMPORT_COUNT` in `src/wir_prelude.rs`),
classified below. **Infrastructure** imports carry no authority and the
pure-compute host provides them. **Capability** imports are authority (or
interpreter-only host services) and the pure-compute host **omits** them. The
tables group imports by family and, for a *staged* result, list the representative
`…_len`/`…_size` import (its paired drain — `dir_read`, `dir_list`,
`net_recv_*`, `file_read`, … — is implied), so the rows classify the 58 rather
than enumerate each half of a pair.

### Infrastructure — provided by the pure-compute host

| import | signature | purpose |
| --- | --- | --- |
| `print` | `(i32 ptr, i32 len)` | write a UTF-8 buffer to output (capturable; output is not authority) |
| `print_int` | `(i64)` | write an integer result |
| `print_float` | `(f64)` | write a float result (canonical `render_float` form) |
| `fill_pending` | `(i32 out_ptr)` | drain the staged String into guest memory |
| `write_pending_list` | `(i32 base_ptr)` | lay a staged List(String) into guest memory |
| `float_to_str` | `(f64, i32 out_ptr) -> i32` | format a float; write bytes, return length |
| `string_from_code` | `(i64 cp, i32 out_ptr) -> i32` | UTF-8-encode a code point (U+FFFD on out-of-range) |
| `encoding` | `(i32 op, i32 in_ptr, i32 out_ptr) -> i32` | hex / base64 / base64url transforms |
| `regex_match_spans_len` | `(i32 pat_ptr, i32 text_ptr) -> i32` | regex spans; stages result, returns length |
| `crypto.sha256` | `(i32 in_ptr, i32 out_ptr)` | SHA-256, 64 hex bytes |
| `crypto.sha512` | `(i32 in_ptr, i32 out_ptr)` | SHA-512, 128 hex bytes |
| `crypto.sha3_256` | `(i32 in_ptr, i32 out_ptr)` | SHA3-256, 64 hex bytes |
| `crypto.hmac_sha256` | `(i32 key_ptr, i32 msg_ptr, i32 out_ptr)` | HMAC-SHA256 (key is hex), 64 hex bytes |
| `crypto.rune_hash` | `(i32 paths_ptr, i32 contents_ptr, i32 out_ptr)` | content hash `sha256:<hex>` (71 bytes) |
| `crypto.ed25519_verify` | `(i32 pk, i32 msg, i32 sig) -> i32` | verify (pk/sig hex); pure compute |
| `crypto.ecdsa_p256_verify` | `(i32 pk, i32 msg, i32 sig) -> i32` | verify; pure compute |
| `crypto.ecdsa_p256_verify_hex` | `(i32 pk, i32 msg, i32 sig) -> i32` | verify; pure compute |
| `field_str_len` | `(i32 h) -> i32` | reflection field length (host cell; returns 0 for ordinary programs) |
| `field_intlist_len` | `(i32 h) -> i32` | reflection field length |
| `field_strlist_size` | `(i32 h) -> i32` | reflection field length |

The crypto digests, encoding transforms, float formatting, and code-point
encoding are pure functions; the pure-compute host mirrors `src/native.rs` /
`src/runtime.rs` **byte-for-byte**, so a browser run and a native run agree on
every observable byte (the parity rule). The ed25519/p256 verifies need a platform
crypto backend; the SHA-256 core (and HMAC and `rune_hash`, which build on it) is
a self-contained synchronous implementation needing none.

### Capability — omitted by the pure-compute host

Providing any of these would grant authority (or an interpreter-only host
service). The pure-compute host omits them all; a module that imports one cannot
instantiate.

| family | imports | authority |
| --- | --- | --- |
| Dir | `dir_read_len`, `dir_list_size`, `dir_subdir` (mints a child `Dir` — user op `subtree`), `dir_exists`, `dir_is_dir`, `dir_write`, `dir_append`, `dir_make_dir`, `dir_open`/`dir_create` (mint a `File[Read]`/`File[Write]` — user ops `read_file`/`write_file`) | filesystem |
| File | `file_read_len`, `file_write` | one file — the leaf of `Dir` (RFC-0012) |
| Net | `net_connect`, `net_try_connect`, `net_listen`, `net_accept`, `net_restrict`/`net_deny` (intersect/subtract the address-set — user ops `net.only`/`net.deny`), `net_send_line`, `net_send_bytes`, `net_recv_line_len`, `net_recv_all_len`, `net_recv_bytes_len`, `net_close` | network |
| Exec | `exec_run` | subprocess |
| Clock | `now` | wall clock |
| Env | `env_len`, `env_fill` | process environment |
| Secret | `crypto.sign`, `crypto.public_key` | host-held key material |
| SecretStore | `secretstore_lookup`, `crypto_reveal_len` | named secrets |
| Args | `args_size` | host-chosen argv (pure input, but a host service) |
| Build | `build_read_len`, `build_out_write` | build-time confined I/O |
| Compiler | `compiler_footprint_len`, `compiler_diff_len` | interpreter-only toolchain services |

> `args_size` is host-chosen *input* rather than authority, but it is a host
> service the pure-compute target does not offer, so it is omitted; a browser
> module receives no argv.

## Hosts

- **`src/runtime.rs`** — the wasmtime host. Links only the granted capability
  imports plus the pure infrastructure; the reference implementation of every
  signature and the pending-buffer protocol above.
- **`web/witchy-runtime/witchy-runtime.mjs`** — the JavaScript pure-compute host
  (RFC-0007). Provides the infrastructure imports only; omits every capability
  import. Its `instantiate(wasmBytes, { onPrint })` returns `{ instance, output,
  run }`. See `web/witchy-runtime/README.md`.

The spike `web/witchy-runtime/spike.mjs` (driven by the Rust test
`tests/browser_shim.rs`) compiles a pure rune, runs it under the JS host, asserts
its output equals the native interpreter run byte-for-byte, and confirms a
capability rune is refused with a `LinkError`.
