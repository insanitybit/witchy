# BUG-462: Bytes API is one-way, incomplete, and only has lossy text decode

Severity: MED
Status: FIXED
Verified: 2026-07-09 fixed on master 05883095
Component: `std/bytes`, binary payload APIs, RFC-0050 Bytes API completion, generated stdlib docs

## Problem

Historical problem: `std/bytes` presented `Bytes` as Witchy's canonical
UTF-8-free binary payload type for files, network frames, hashes, serialized
payloads, and worker VM boundaries. The old public API only let Witchy code
build `Bytes` from a `String`, inspect it back as a `List(Int)` through
`to_list`, or convert it to text through `to_string`. It had no checked inverse
`from_list`, no total `get` counterpart to the trapping `at`, no basic
byte-search helpers such as `contains` or `index_of`, and no strict UTF-8 decode
that returned `Result`.

That made `Bytes` feel like a backend transport artifact rather than a usable
stdlib collection. A program could observe raw byte values, but could not
assemble an arbitrary non-UTF-8 byte buffer from validated byte values without
going through strings, lossy text, or a future codec API.

The text bridge was especially release-facing: `bytes.to_string` was explicitly
a lossy UTF-8 decode that replaces invalid sequences with U+FFFD, but the
function name read like a normal exact conversion and there was no
`to_string_lossy` / `decode_utf8` split. Witchy source could already manufacture
invalid UTF-8 bytes without a future `from_list` by slicing through a multibyte scalar, e.g.
`bytes.slice(bytes.from_string("é"), 0, 1)`.

This is adjacent to BUG-456 but distinct:

- BUG-456 covers codecs: hex/base64 should accept/return `Bytes`.
- BUG-462 covered the `bytes` module itself: it had `to_list` but no checked
  `from_list`, lacked the total `get` shape neighboring collections expose, and
  lacked the basic search helpers RFC-0050 already calls out. It also covered
  the missing strict-vs-lossy UTF-8 boundary in the core `bytes` module.

## Code Evidence

- `std/bytes.witchy` describes `Bytes` as the canonical raw payload type.
- `std/bytes.witchy` now exposes `from_list`, `get`, `to_string_lossy`,
  `decode_utf8`, `contains`, `index_of`, `starts_with`, and `ends_with`.
- `spec/stdlib.md` publishes the completed public surface after regeneration
  from stdlib comments.
- `std/bytes.witchy` documents `to_string_lossy` as the explicit lossy UTF-8
  boundary, keeps `to_string` as a compatibility wrapper, and provides
  `decode_utf8` for strict `Result`-returning decode.
- `crates/witchy-interp/src/interpreter.rs:1240-1242` implements
  `__bytes_to_string` with `String::from_utf8_lossy`.
- `crates/witchy-lower/src/codegen/builtins.rs:758-763` lowers compiled
  `__bytes_to_string` through the lossy `$bytes_to_string` helper for backend
  parity.
- `src/example_tests.rs` has `bytes_type_backends_agree`, which verifies
  `from_list`, invalid byte rejection, strict decode erroring on invalid UTF-8,
  total `get`, search helpers, prefix/suffix helpers, and both backends.

Regression:

```sh
CARGO_TARGET_DIR=target-codex-iter cargo test bytes_type_backends_agree -- --nocapture
```
- `rfcs/0050-method-call-generalization.md:136-139` explicitly lists "Bytes API
  completion" as adding `from_list` / `contains` / `index_of`; those APIs now
  exist.
- `std/string.witchy:46-54`, `std/string.witchy:145-153`, and
  `std/list.witchy:213-219` / `std/list.witchy:495-504` show the neighboring
  collection/search shape: total element access is `Option`, `contains` is Bool
  membership, and `index_of` returns `Option(Int)`.
- `bugs/BUG-456-encoding-has-no-bytes-native-codecs.md:35-36` notes the missing
  `from_list`/codec integration, but its expected fix is a codec family rather
  than completing `std/bytes` as a collection-like module.

## Fix Direction

Fixed: the core `bytes` surface is now complete enough to rely on `Bytes` as the
public binary story:

- `from_list(xs: List(Int)) -> Result(Bytes, String)` is a checked
  constructor that rejects values outside `0..=255`;
- `get(b: Bytes, index: Int) -> Option(Int)` is the total counterpart to
  trapping `at`;
- `contains(b, needle)`, `index_of(b, needle)`, `starts_with`, and `ends_with`
  provide the core search surface;
- text decoding is split into `decode_utf8(b) -> Result(String, String)` and
  `to_string_lossy(b) -> String`.

Do not silently clamp byte values in `from_list`; invalid byte values are
ordinary invalid input and should follow the RFC-0044 `Result` policy.

## Acceptance

- Witchy source can construct arbitrary valid byte buffers, including non-UTF-8
  sequences, without routing through lossy `String`.
- `bytes.to_list(bytes.from_list(xs)?) == xs` for every `xs` whose elements are
  all in `0..=255`.
- `bytes.from_list([-1])` and `bytes.from_list([256])` return clear errors.
- `bytes.get(b, i)` returns `Some(byte)` in range and `None` out of range,
  matching `list.get` / `string.char_at` rather than requiring users to trap or
  pre-check length manually.
- Invalid UTF-8 byte buffers have a strict decode path that returns `Err` instead
  of requiring callers to accept U+FFFD normalization.
- Any lossy text conversion is named or documented as lossy in generated docs and
  examples.
- Generated stdlib docs present `Bytes` as a complete byte-buffer API, not only
  a string bridge plus a one-way `to_list`.
