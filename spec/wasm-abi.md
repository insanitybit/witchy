---
verified: 0783c22
---

# The `"witchy"` WASM import ABI

witchyc-compiled modules reach the outside world through a single WASM import
module named `"witchy"`. Each import is a host function the runtime must supply;
**a granted host function is a capability** (or a piece of pure infrastructure).
This is the handshake between the compiler (the import declarations live in
`crates/witchy-wir/src/wir_prelude.rs`, and the codegen path in
`crates/witchy-lower/src/codegen/` selects which a module reaches) and a host
that satisfies them (`crates/witchy-runtime/src/runtime.rs` is the wasmtime host;
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
Version 1 is the baseline frozen for the first 0.1 release; earlier pre-release
import counts were not separately published ABI versions.

## Import inclusion is tree-shaken

A compiled module declares **only the imports it actually reaches**, not the full
set. The codegen path (`assemble_wir_module` in
`crates/witchy-lower/src/codegen/assembly.rs`) prunes the import list to the host
functions the reachable code calls. A footprint-empty rune therefore imports no
capability-authority function; a rune that touches the
filesystem/network/clock additionally imports the corresponding capability
function. Authority-free modules can still require a native-only launch or
toolchain service such as `args_size` or `compiler_footprint_len`.

This is what makes the browser target's containment **structural**. The browser
host (`web/witchy-runtime/`) provides only entries marked `browser: provided`
below. Per the WebAssembly spec, a module that imports a function the host does not supply
**fails to instantiate** with a `LinkError`. So:

- an authority-free rune using only browser-provided services instantiates and
  runs;
- an impure rune imports a capability function the host does **not** provide →
  `WebAssembly.instantiate` throws, and the module never runs.

The host is a sieve that admits no authority-bearing module. It is deliberately
stricter than "all footprint-empty modules": native-only launch and toolchain
services are omitted too. This is **deny-by-omission**: capabilities are denied
by simply not being on offer, the strongest "structurally incapable of I/O"
guarantee. No trap stubs are needed (or installed) for capability imports —
their *absence* is the guarantee.

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

ABI version 1 declares **86 imports** (`IMPORT_COUNT` in
`crates/witchy-wir/src/wir_prelude.rs`). That file owns the ordered signatures
and the explicit metadata rendered below. The classes are:

- **pure infrastructure**: deterministic computation or marshaling, no authority;
- **capability authority**: only linked when the corresponding grant is present;
- **launch input**: data selected by the host at launch, not ambient authority;
- **internal/toolchain service**: compiler, reflection, or worker-runtime plumbing;
- **runtime diagnostic**: checked-heap or abort reporting, no application authority.

`browser: provided` is the exact deny-by-omission surface implemented by the
JavaScript host. `omitted` means a browser module importing that function cannot
instantiate. `authority` is the concrete grant family used by launchers for
precompiled `.wasm` classification; `none` means the import is not a capability
grant. Regenerate the table with `cargo run -p witchy-wir --example abi_catalog`;
the test suite compares the committed block with the compiler catalog
byte-for-byte and instantiates an all-import probe against the native host.

<!-- BEGIN GENERATED WASM ABI IMPORTS -->
| import | signature | class | authority | browser |
| --- | --- | --- | --- | --- |
| `print` | `(i32, i32)` | pure infrastructure | none | provided |
| `crypto.sha256` | `(i32, i32)` | pure infrastructure | none | provided |
| `crypto.rune_hash` | `(i32, i32, i32)` | pure infrastructure | none | provided |
| `compiler_footprint_len` | `(i32) -> i32` | internal/toolchain service | none | omitted |
| `compiler_diff_len` | `(i32, i32) -> i32` | internal/toolchain service | none | omitted |
| `compiler_doc_len` | `(i32, i32) -> i32` | internal/toolchain service | none | omitted |
| `compiler_doc_result_json_len` | `(i32, i32) -> i32` | internal/toolchain service | none | omitted |
| `user_cap_field_len` | `(i32, i32) -> i32` | launch input | none | provided |
| `field_str_len` | `(i32) -> i32` | internal/toolchain service | none | provided |
| `field_intlist_len` | `(i32) -> i32` | internal/toolchain service | none | provided |
| `field_strlist_size` | `(i32) -> i32` | internal/toolchain service | none | provided |
| `float_to_str` | `(f64, i32) -> i32` | pure infrastructure | none | provided |
| `encoding` | `(i32, i32, i32) -> i32` | pure infrastructure | none | provided |
| `crypto.sign` | `(externref, i32, i32)` | capability authority | Secret | omitted |
| `crypto.public_key` | `(externref, i32)` | capability authority | Secret | omitted |
| `secretstore_lookup` | `(i32) -> externref` | capability authority | Secret | omitted |
| `crypto_reveal_len` | `(externref) -> i32` | capability authority | Secret | omitted |
| `mint_secret` | `(i32) -> externref` | capability authority | Secret | omitted |
| `env_len` | `(i32) -> i32` | capability authority | Env | omitted |
| `env_fill` | `(i32, i32)` | capability authority | Env | omitted |
| `dir_read_len` | `(externref, i32) -> i32` | capability authority | Dir.Read | omitted |
| `dir_list_size` | `(externref) -> i32` | capability authority | Dir.Read | omitted |
| `args_size` | `() -> i32` | launch input | none | omitted |
| `write_pending_list` | `(i32)` | pure infrastructure | none | provided |
| `vm_par_map_run` | `(i32, i32) -> i32` | internal/toolchain service | none | omitted |
| `vm_par_map_write` | `(i32)` | internal/toolchain service | none | omitted |
| `vm_par_map_bytes_run` | `(i32, i32) -> i32` | internal/toolchain service | none | omitted |
| `vm_par_map_bytes_write` | `(i32)` | internal/toolchain service | none | omitted |
| `vm_with_dir_run` | `(externref, i32, i32) -> i32` | internal/toolchain service | none | omitted |
| `vm_serve_run` | `(i32, i32, i32) -> i32` | internal/toolchain service | none | omitted |
| `build_read_len` | `(i32, i32) -> i32` | capability authority | Build.Read | omitted |
| `build_out_write` | `(i32, i32, i32)` | capability authority | Build.Out | omitted |
| `build_env_len` | `(i32, i32) -> i32` | capability authority | Build.Env | omitted |
| `build_env_fill` | `(i32, i32, i32)` | capability authority | Build.Env | omitted |
| `build_fetch_len` | `(i32, i32, i32) -> i32` | capability authority | Build.Fetch | omitted |
| `build_exec_run` | `(i32, i32, i32) -> i32` | capability authority | Build.Exec | omitted |
| `net_recv_line_len` | `(externref) -> i32` | capability authority | Net.Connect | omitted |
| `net_recv_all_len` | `(externref) -> i32` | capability authority | Net.Connect | omitted |
| `net_recv_bytes_len` | `(externref, i64) -> i32` | capability authority | Net.Connect | omitted |
| `fill_pending` | `(i32)` | pure infrastructure | none | provided |
| `crypto.sha512` | `(i32, i32)` | pure infrastructure | none | provided |
| `crypto.sha3_256` | `(i32, i32)` | pure infrastructure | none | provided |
| `crypto.hmac_sha256` | `(i32, i32, i32)` | pure infrastructure | none | provided |
| `print_int` | `(i64)` | pure infrastructure | none | provided |
| `print_float` | `(f64)` | pure infrastructure | none | provided |
| `string_from_code` | `(i64, i32) -> i32` | pure infrastructure | none | provided |
| `mint_dir` | `(i32) -> externref` | capability authority | Dir.grant | omitted |
| `dir_subdir` | `(externref, i32) -> externref` | capability authority | Dir.Read | omitted |
| `dir_only` | `(externref, i32) -> externref` | capability authority | Dir.Read | omitted |
| `dir_exists` | `(externref, i32) -> i32` | capability authority | Dir.Read | omitted |
| `dir_is_dir` | `(externref, i32) -> i32` | capability authority | Dir.Read | omitted |
| `dir_write` | `(externref, i32, i32)` | capability authority | Dir.Write | omitted |
| `dir_append` | `(externref, i32, i32)` | capability authority | Dir.Write | omitted |
| `dir_make_dir` | `(externref, i32)` | capability authority | Dir.Write | omitted |
| `dir_open` | `(externref, i32) -> externref` | capability authority | Dir.Read | omitted |
| `dir_create` | `(externref, i32) -> externref` | capability authority | Dir.Write | omitted |
| `mint_file` | `(i32) -> externref` | capability authority | File.grant | omitted |
| `file_read_len` | `(externref) -> i32` | capability authority | File.handle | omitted |
| `file_write` | `(externref, i32)` | capability authority | File.handle | omitted |
| `mint_net` | `(i32) -> externref` | capability authority | Net.grant | omitted |
| `net_connect` | `(externref, i32) -> externref` | capability authority | Net.Connect | omitted |
| `net_try_connect` | `(externref, i32) -> externref` | capability authority | Net.Connect | omitted |
| `net_resolve_size` | `(externref, i32) -> i32` | capability authority | Net.Connect | omitted |
| `net_connect_pinned` | `(externref, i32, i32, i64, i32) -> externref` | capability authority | Net.Connect | omitted |
| `net_try_connect_pinned` | `(externref, i32, i32, i64, i32) -> externref` | capability authority | Net.Connect | omitted |
| `net_listen` | `(externref, i32) -> externref` | capability authority | Net.Listen | omitted |
| `net_listen_tls` | `(externref, i32, i32, externref) -> externref` | capability authority | Net.Listen, Secret | omitted |
| `net_accept` | `(externref) -> externref` | capability authority | Net.Listen | omitted |
| `serve_pool` | `(externref)` | capability authority | Net.Listen | omitted |
| `net_restrict` | `(externref, i32) -> externref` | capability authority | Net.Connect | omitted |
| `net_deny` | `(externref, i32) -> externref` | capability authority | Net.Connect | omitted |
| `net_send_line` | `(externref, i32)` | capability authority | Net.Connect | omitted |
| `net_send_bytes` | `(externref, i32)` | capability authority | Net.Connect | omitted |
| `net_close` | `(externref)` | capability authority | Net.Connect | omitted |
| `now` | `() -> i64` | capability authority | Clock | omitted |
| `now_monotonic` | `() -> i64` | capability authority | Clock | omitted |
| `rand_u64` | `() -> i64` | capability authority | Rand | omitted |
| `regex_match_spans_len` | `(i32, i32) -> i32` | pure infrastructure | none | provided |
| `crypto.__ecdsa_p256_verify_status` | `(i32, i32, i32) -> i64` | pure infrastructure | none | provided |
| `crypto.__ecdsa_p256_verify_hex_status` | `(i32, i32, i32) -> i64` | pure infrastructure | none | provided |
| `crypto.__rsa_pkcs1_sha256_verify_status` | `(i32, i32, i32) -> i64` | pure infrastructure | none | provided |
| `crypto.__ed25519_verify_status` | `(i32, i32, i32) -> i64` | pure infrastructure | none | provided |
| `exec_run` | `(externref, i32, i32, i32) -> i32` | capability authority | Exec | omitted |
| `heap_register` | `(i32, i32)` | runtime diagnostic | none | omitted |
| `heap_frontier` | `(i32)` | runtime diagnostic | none | omitted |
| `__witchy_abort` | `(i32, i64, i64, i32)` | runtime diagnostic | none | provided |
<!-- END GENERATED WASM ABI IMPORTS -->

### Encoding sub-ABI

`encoding` is a byte-oriented sub-ABI: `in_ptr` addresses a `[i32 len][bytes]`
buffer, the host writes result bytes at `out_ptr`, and the return value is their
length. String results are UTF-8; `Bytes` results are raw. Its op ids are part of
ABI version 1:

| op | operation | input -> output |
| ---: | --- | --- |
| 0 | `hex_encode` | String -> String |
| 1 | `hex_decode_lossy` | String -> String |
| 2 | `base64_encode` | String -> String |
| 3 | `base64_decode_lossy` | String -> String |
| 4 | `hex_to_base64url_lossy` | String -> String |
| 5 | `base64url_decode_lossy` | String -> String |
| 6 | `base64url_to_hex_lossy` | String -> String |
| 7 | `utf8_lossy` | Bytes -> String |
| 8 | `hex_encode_bytes` | Bytes -> String |
| 9 | `base64_encode_bytes` | Bytes -> String |
| 10 | `base64url_encode_bytes` | Bytes -> String |
| 11 | `hex_decode_bytes_raw` | String -> Bytes |
| 12 | `base64_decode_bytes_raw` | String -> Bytes |
| 13 | `base64url_decode_bytes_raw` | String -> Bytes |

Public decoders validate their alphabet and shape before calling a raw decode
op. Hosts still reject an unknown op or malformed direct hex decode loudly.

The crypto digests, encoding transforms, float formatting, and code-point
encoding are pure functions; the pure-compute host mirrors
`crates/witchy-runtime/src/native.rs` / `crates/witchy-runtime/src/runtime.rs`
**byte-for-byte**, so a browser run and a native run agree on
every observable byte (the parity rule). The ed25519/p256 verifies need a platform
crypto backend; the SHA-256 core (and HMAC and `rune_hash`, which build on it) is
a self-contained synchronous implementation needing none.

### Host policy notes

> `args_size` is host-chosen *input* rather than authority, but it is a host
> service the pure-compute target does not offer, so it is omitted; a browser
> module receives no argv.

> The `compiler_*` imports are pure functions of their source arguments and grant
> no authority — they never appear in the capability footprint (a program that
> `import compiler` and calls `compiler.footprint` shows only its real
> capabilities). The interpreter **and** the native wasmtime host
> (`crates/witchy-runtime/src/runtime.rs`) both link them, so `std/compiler`'s
> `footprint`/`diff`/`doc` run on the compiled backend as well as the
> interpreter. Only the pure-compute browser host omits them — it ships no
> compiler — so a module importing one cannot instantiate there.

## Hosts

- **`crates/witchy-runtime/src/runtime.rs`** — the wasmtime host. Defines every
  non-authority import and defines capability-authority imports only when the
  corresponding grant is present; it is the reference implementation of every
  signature and the pending-buffer protocol above.
- **`web/witchy-runtime/witchy-runtime.mjs`** — the JavaScript pure-compute host
  (RFC-0007). Provides exactly the imports marked `browser: provided` above and
  omits every capability-authority import. Its
  `instantiate(wasmBytes, { onPrint })` returns `{ instance, output, run }`.
  See `web/witchy-runtime/README.md`.

The spike `web/witchy-runtime/spike.mjs` (driven by the Rust test
`tests/browser_shim.rs`) compiles a pure rune, runs it under the JS host, asserts
its output equals the native interpreter run byte-for-byte, and confirms a
capability rune is refused with a `LinkError`.

## Runtime aborts (RFC-0045)

Every runtime abort on the compiled backend — an out-of-bounds `list`/`bytes`
index, `string.to_int` on junk or overflow, integer division/modulo failure,
ordering a `NaN`, or a user `fail(msg)` — carries the interpreter's complete
diagnostic out before it traps. The two backends agree on function, line, and
message, not merely on the fact of erroring.

- **`__witchy_abort(template, a, b, str_ptr)` is always linked** and grants no
  authority: it reads only guest-memory strings named by `str_ptr` and the
  packed site, returns nothing to the guest, and its only effect is to terminate
  execution with a diagnostic label — an ability the guest already has via
  `unreachable`. Like the checked-heap
  imports (`heap_register`, RFC-0023), it is therefore defined unconditionally on
  every host (the pure-compute shim included) and is **excluded from the
  capability footprint** (`witchy caps` and the coven widening gate never see it).
- **Rust message text has one owner** in `crates/witchy-syntax/src/diag.rs`
  (`DiagTemplate`). `template` is the stable `DiagTemplate::id()` (part of the
  compiled ABI — do not renumber); `a`/`b` are integer holes and `str_ptr` is a
  witchy-string pointer (or `0`). The interpreter and native host use the same
  renderer. The dependency-free browser host mirrors the small table, pinned by
  a compiled test matrix covering every pure template.
- **`__witchy_diagnostic_site` carries source location.** Modules with routed failures
  export one mutable `i64`: high 32 bits are a static witchy-string pointer to
  the lexical function name, low 32 bits are the source line. Lowered calls pass
  that packed site as a final argument to host-backed WIR helpers; a helper
  writes the global only on its actual host edge. Successful nested calls
  and async interleavings therefore cannot stale an outer operation's location.
  Zero means unavailable.
- **Exact error parity is enforced** by `witchy parity`: every both-error outcome
  must have byte-for-byte identical complete diagnostics. A bare Wasm trap,
  missing location, or backend-specific host error is a divergence.
- **`WITCHY_WASM_BACKTRACE`** — set this environment variable to also dump the
  full named-frame wasm backtrace beneath the message (the emitted name section
  makes frames readable). It is a debugging add-on for *frames*; the message
  itself now always prints regardless.
