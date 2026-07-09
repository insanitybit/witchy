# BUG-456: Encoding has no Bytes-native codecs

Severity: MED
Status: FIXED
Verified: 2026-07-09 fixed on master 8fd07eab
Component: `std/encoding`, `std/bytes`, binary payload APIs, JWT/WebAuthn helpers

## Problem

Historical problem: Witchy now has a first-class `Bytes` type for raw binary
payloads, but `std/encoding` used to expose hex/base64 codecs only through
`String`:

- `hex_encode(data: String) -> String`
- `hex_decode(data: String) -> Result(String, String)`
- `base64_encode(data: String) -> String`
- `base64_decode(data: String) -> Result(String, String)`
- `base64url_decode(data: String) -> Result(String, String)`
- `base64url_to_hex(data: String) -> Result(String, String)`

That API predated `Bytes` and made binary data either lossy text or a hex-string
detour. For example, decoding arbitrary base64 bytes could only produce a lossy
UTF-8 `String`; the one lossless binary escape hatch was `base64url_to_hex`,
which was format-specific and then pushed callers into hex plumbing instead of
the language's canonical binary type.

This was not a malformed-input bug like BUG-006/198/201. Those tracked
validation and truncation. BUG-456 was the remaining API-shape gap: valid
non-text bytes could not round-trip through `std/encoding` as `Bytes`.

## Code Evidence

- `std/bytes.witchy` defines `Bytes` as the canonical UTF-8-free binary payload
  type for files, network frames, hashes, serialized payloads, and worker VM
  boundaries.
- `std/encoding.witchy` now describes the module as hex/base64 for text
  conveniences and raw `Bytes` payloads.
- `std/encoding.witchy` now exposes `hex_encode_bytes`,
  `hex_decode_bytes`, `base64_encode_bytes`, `base64_decode_bytes`,
  `base64url_encode_bytes`, and `base64url_decode_bytes`.
- String helpers remain as UTF-8 convenience wrappers over the Bytes-native
  codecs and lossy text decoding is explicit.
- `crates/witchy-runtime/src/native.rs` registers the Bytes-native encoding
  operations and returns/accepts `Value::Bytes` where appropriate.
- `crates/witchy-lower/src/codegen/builtins.rs`,
  `crates/witchy-lower/src/codegen/types.rs`, `src/lib.rs`, and
  `crates/witchy-runtime/src/runtime.rs` all include the Bytes-native op IDs.
- `src/example_tests.rs` has
  `encoding_bytes_codecs_round_trip_binary_on_both_backends`, which verifies
  arbitrary non-UTF-8 bytes round-trip across hex, base64, and base64url on both
  backends.
- Some callers still use the legacy text/hex helpers where their downstream API
  is itself text or hex-shaped; that is separate from BUG-456's API-surface gap,
  which is fixed.

Regression:

```sh
CARGO_TARGET_DIR=target-codex-iter cargo test encoding_bytes_codecs_round_trip_binary_on_both_backends -- --nocapture
```

## Why This Matters

The stdlib used to have two competing binary stories:

- `Bytes` is the typed binary value used by `vm.par_map`, `vm.serve`, and
  low-level byte-buffer APIs.
- `encoding` treated `String` as the binary carrier and needed
  `*_lossy` names plus hex escape hatches for non-text data.

That made security-facing examples feel improvised. JWT signatures, WebAuthn
challenges, hashes, and binary network/file payloads should compose through one
binary type, not through "text unless it is not text, then hex."

## Expected

Fixed by adding Bytes-native codec APIs and making them the canonical binary
surface:

- `hex_encode_bytes(data: Bytes) -> String`
- `hex_decode_bytes(data: String) -> Result(Bytes, String)`
- `base64_encode_bytes(data: Bytes) -> String`
- `base64_decode_bytes(data: String) -> Result(Bytes, String)`
- `base64url_encode_bytes(data: Bytes) -> String`
- `base64url_decode_bytes(data: String) -> Result(Bytes, String)`

The existing string helpers are kept as UTF-8 convenience wrappers:

- string encoders can call `bytes.from_string` then the bytes codec;
- string decoders can call the bytes decoder then `bytes.to_string`, with their
  lossy behavior documented explicitly.

The browser/native/interpreter host-op tables share the same op IDs and tests
cover arbitrary non-UTF-8 bytes, not only text payloads.
