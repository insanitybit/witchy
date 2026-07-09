# BUG-463: Crypto verify conflated malformed input with bad signatures

Status: FIXED
Severity: HIGH
Component: `std/crypto`, RFC-0044 error policy

## Summary

The ignored local bug note reported that the public crypto verification APIs
returned `Bool`, collapsing malformed public keys/messages/signatures and
well-formed-but-invalid signatures into the same `false` value.

That contradicted RFC-0044's trust-boundary rule: malformed input must be an
error, while a bad signature is an ordinary negative verification result.

## Resolution

Current `std/crypto.witchy` exposes the verifier surface as:

```witchy
crypto.ed25519_verify(...) -> Result(Bool, String)
crypto.ecdsa_p256_verify(...) -> Result(Bool, String)
crypto.ecdsa_p256_verify_hex(...) -> Result(Bool, String)
crypto.rsa_pkcs1_sha256_verify(...) -> Result(Bool, String)
```

The public wrappers map native status codes to `Ok(true)`, `Ok(false)`, or a
specific malformed-input `Err`. `tests/cli_subcommands.rs` includes
`crypto_verify_malformed_inputs_are_result_errors`, which verifies malformed
Ed25519 public-key hex and malformed ECDSA message hex report errors instead
of `false`.
