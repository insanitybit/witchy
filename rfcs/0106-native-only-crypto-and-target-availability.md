---
rfc: 0106
title: "Native-only cryptographic primitives and target availability"
status: accepted
created: 2026-07-27
updated: 2026-08-03
tracking: "Design accepted; no open questions. Implementation not started — native SHAKE128/256 XOF via AWS-LC behind the native/interpreter target, browser target denies it by omission (RFC-0007). Remaining: the native primitive, target-availability gating, and a pure-Witchy browser fallback or documented absence."
predecessors:
  - "[0007](0007-witchy-wasm-browser-target.md) (browser WASM is the compiled backend under a deny-by-omission host)"
  - "[0044](0044-std-error-policy.md) (stdlib error contracts)"
related:
  - "[0037](0037-correctness-harness.md) (interpreter/WASM differential evidence)"
  - "[0091](0091-browser-virtual-capabilities.md) (browser host-provider boundary)"
---

# RFC-0106: Native-only cryptographic primitives and target availability

## Summary

Witchy should be able to expose cryptographic primitives that are available in
the native runtime but deliberately unavailable in the browser target. The
first application is SHAKE128/SHAKE256 backed by AWS-LC's low-level EVP API:
the native interpreter and native-hosted compiled WASM use AWS-LC, while the
browser host does not provide the corresponding import. This RFC preserves the
existing browser deny-by-omission model immediately and adds a reusable
target-availability contract so future browser compilation can reject such
calls before producing a module that will fail at instantiation.

The public SHAKE API is binary and variable-length:

```text
crypto.shake128(input: Bytes, output_len: Int) -> Result(Bytes, CryptoError)
crypto.shake256(input: Bytes, output_len: Int) -> Result(Bytes, CryptoError)
```

The first implementation does not require an upstream change to `aws-lc-rs`.
It adds a small, isolated adapter over the locked `aws-lc-sys` dependency and
does not expose generated AWS-LC types outside that adapter.

## Motivation

Witchy's current crypto module exposes fixed-output operations such as
SHA3-256 through native seams. Post-quantum signature implementations need
SHAKE, an extendable-output function whose output length is selected by the
caller. A fixed-size hexadecimal `String` API is the wrong representation:
callers need raw bytes and output lengths substantially larger than ordinary
digest sizes.

AWS-LC already provides `EVP_shake128`, `EVP_shake256`,
`EVP_DigestFinalXOF`, and `EVP_DigestSqueeze`. The current high-level
`aws-lc-rs` digest facade does not expose an XOF interface, and upstreaming a
new facade is outside this project's desired scope. The repository already
links `aws-lc-rs`, whose locked dependency graph includes `aws-lc-sys`, so a
carefully bounded local adapter avoids introducing a second crypto
implementation for native targets.

The browser target intentionally does not provide every native-only service.
Today this is enforced only at the WASM import boundary: a browser module that
imports an omitted function fails to instantiate. That is a valid containment
property, but it gives poor feedback when the browser compiler itself could
have diagnosed the unsupported operation.

## Design

### 1. Native AWS-LC adapter

Add a private module in `witchy-runtime`, compiled only for non-`wasm32`
targets, with a narrow safe interface:

```text
shake128(input: &[u8], output_len: usize) -> Result<Vec<u8>, CryptoError>
shake256(input: &[u8], output_len: usize) -> Result<Vec<u8>, CryptoError>
```

The adapter owns all direct `aws-lc-sys` use and all unsafe FFI. It must:

- select `EVP_shake128()` or `EVP_shake256()`;
- initialize an `EVP_MD_CTX`;
- absorb the complete input with `EVP_DigestUpdate`;
- produce exactly `output_len` bytes with `EVP_DigestFinalXOF`;
- use RAII cleanup for every allocated context, including error paths;
- reject negative, oversized, or otherwise unrepresentable Witchy output
  lengths before allocation or FFI;
- never expose an AWS-LC pointer, context, or generated binding type to the
  rest of the runtime.

The adapter should use the ordinary AWS-LC build selected by the existing
native dependency configuration. If the repository's FIPS build feature is
enabled, the adapter must compile against that matching sys crate and must not
silently select a second crypto provider.

### 2. Witchy API

Add `shake128` and `shake256` to `std/crypto.witchy`. The input is `Bytes`,
not `String`, and the result is raw `Bytes`, not hexadecimal text. Invalid
output lengths return a typed error rather than trapping or wrapping.

The error type should identify at least an invalid length. Allocation failure
remains a runtime failure under the existing runtime policy; it is not
converted into a cryptographic false result.

The functions are pure infrastructure for capability-footprint purposes. They
consume no `Secret` and add no authority grant.

### 3. Interpreter path

Register the qualified native functions in the interpreter's native registry.
The interpreter implementation must convert `Value::Bytes` to the adapter's
input slice and return `Value::Bytes` of exactly the requested length. The
placeholder declarations in `std/crypto.witchy` remain ordinary stdlib
declarations; native interception is an implementation detail, as with the
existing digest functions.

### 4. Compiled native-WASM path

Add dedicated WIR helpers for variable-length raw-byte crypto results. The
helper should validate and narrow the requested length, allocate a guest
`Bytes` buffer with its normal `[length][payload]` layout, and call a host
import shaped approximately as:

```text
crypto.shake128(input_ptr, output_ptr, output_len)
crypto.shake256(input_ptr, output_ptr, output_len)
```

The host reads the guest input as raw bytes, invokes the shared native adapter,
and writes exactly `output_len` bytes to the guest buffer. This is a direct
output-pointer operation; the fixed-size hex digest helper and the pending
String protocol must not be stretched to cover an arbitrary raw buffer.

The import is classified as pure infrastructure with no authority. Its native
host implementation is provided; the browser host deliberately does not
provide it.

### 5. Target availability metadata

Extend the canonical WASM import catalog so each import has explicit target
availability, initially represented by the existing browser-provided boolean:

```text
crypto.sha3_256  browser: provided
crypto.shake128  browser: omitted
crypto.shake256  browser: omitted
```

This immediately gives SHAKE the existing structural behavior: a native
compiled module can use it, while the browser cannot instantiate a module that
imports it.

As a follow-on within this RFC, the browser compilation path should consume
the same metadata and emit a compile-time diagnostic when reachable code
requires an omitted import. The diagnostic should identify the import and
target, for example:

```text
crypto.shake128 is unavailable for target browser
```

This check belongs at the checked/lowering boundary, before WASM emission. It
must be reachability-sensitive: an unused helper mentioning a native-only
operation must not make an otherwise pure program fail. The existing import
tree-shaking remains authoritative for the final module import set.

The first implementation may ship the metadata and runtime deny-by-omission
behavior before the compile-time diagnostic if doing both would broaden the
change. It must not claim browser support merely because the interpreter can
execute the function.

### 6. Browser policy

The browser target does not gain an AWS-LC or JavaScript SHAKE implementation
as part of this RFC. A browser-compatible package may still implement SHAKE in
pure Witchy, subject to the normal performance and parity constraints, but a
package that calls `crypto.shake128` or `crypto.shake256` is native-target
only.

This preserves RFC-0007's core invariant: the browser runs the compiled
backend under a host that supplies a strict subset of imports. It does not
create a third cryptographic backend whose semantics need to be maintained.

## Contracts and acceptance evidence

The implementation is complete only when all of the following are true:

1. FIPS 202 SHAKE128 and SHAKE256 known-answer vectors pass through the
   interpreter.
2. The same vectors pass through native-hosted compiled WASM.
3. Output lengths zero, one, a rate boundary, multiple rate blocks, and the
   largest accepted length are covered.
4. Negative and oversized lengths fail identically on both backends.
5. The first `output_len` bytes of a longer request equal a shorter request's
   complete output.
6. The native adapter has no direct callers outside its owning runtime module.
7. The generated WASM ABI catalog includes the new imports and marks them
   browser-omitted.
8. A browser compile/run test proves that a reachable SHAKE import is rejected
   by the browser host today; once target-aware diagnostics land, the test
   moves to the compile-time diagnostic path.
9. `spec/stdlib.md` and `spec/wasm-abi.md` are regenerated from their
   authoritative sources and agree with the catalog.
10. No `Secret` capability is required or accepted by the SHAKE functions.

The tests must compare AWS-LC output against an independent reference for the
known-answer vectors. Agreement between two Witchy paths alone is not enough
to catch a common-mode FFI or padding error.

## Alternatives

### Upstream `aws-lc-rs` support

Rejected for this RFC. It would provide a cleaner Rust API, but requires an
external project change and release coordination that Witchy does not want to
own.

### Use a RustCrypto SHA-3 crate

Rejected as the native primary implementation. It would be a reasonable
portable fallback, but it creates a second native crypto provider despite the
project's preference for AWS-LC and its FIPS-capable provider path.

### Implement SHAKE in Witchy only

Still possible and useful for a browser-compatible package, but not the
stdlib primitive. A pure implementation would be slower, larger, and would
make the standard crypto surface responsible for maintaining a second
implementation of a security-sensitive primitive.

### Keep the current fixed-string crypto API

Rejected. SHAKE's variable-length raw-byte output cannot be represented
correctly or ergonomically as a fixed hexadecimal digest helper.

### Add a browser JavaScript SHAKE implementation

Deferred. It would make the API available in the browser, but would introduce
another host implementation and a browser-specific cryptographic dependency.
Native-only support is the safer initial boundary.

## Drawbacks

- Direct dependency on generated `aws-lc-sys` bindings is less stable and less
  ergonomic than a high-level wrapper.
- The adapter must track AWS-LC sys-crate version and FIPS/non-FIPS feature
  compatibility.
- Native-only APIs create a target distinction that currently surfaces at
  browser instantiation rather than at compile time.
- Native WASM and interpreter tests require AWS-LC's native build, increasing
  local build cost.
- Browser-compatible consumers cannot use the native SHAKE API and must either
  avoid it or implement a pure Witchy alternative.

## Prior art

- [AWS-LC digest API](https://raw.githubusercontent.com/aws/aws-lc/main/include/openssl/digest.h)
  defines SHAKE accessors and XOF finalization/squeeze operations.
- [FIPS 202](https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.202.pdf) defines
  SHAKE128 and SHAKE256.
- [RFC-0007](0007-witchy-wasm-browser-target.md) defines the existing browser
  deny-by-omission model.

---

<!--
  Once this RFC is implemented/rejected/superseded it is FROZEN.
  - To change the decision: write a NEW RFC that supersedes this one.
  - Allowed edits after freeze: the `status:`/`superseded-by:` fields, and
    appending dated change-notes below.
-->
