# Pinned-key OIDC token verification

The `@oidc` Witchy package verifies the compact RS256 JWT profile used
by Coven's GitHub Actions identity workers. Its only capability is
`Allocator`, supplied by the caller. It uses the Wasm host's public RSA
signature verification and accepts the current time as an argument.

Use `read_rs256_jwks` with the exact JWKS bytes provisioned for the
configured issuer. The caller authenticates and pins those bytes.
Coven compares the SHA-384 pin before admitting its CI configuration.
The reader accepts up to 256 KiB and 32 distinct, explicitly marked
RSA/RS256 signing keys with canonical, unpadded base64url components.
It rejects duplicate object keys, repeated key IDs, and weak RSA
component encodings.

Pass the resulting `Rs256Keys` to `verify_rs256_id_token` along with
the compact JWT, expected issuer and single-string audience, UTC
seconds, and maximum lifetime in seconds. The verifier checks a
bounded fixed algorithm header, signature, issuer, audience, and time
claims before returning a sealed `VerifiedIdToken`. It selects a local
key by `kid`. The token cannot specify a key source or endpoint. Read
the claims through the returned token, and close both owned values
with their `close()` methods.

The caller owns provider configuration, required workload or subject
claims, any nonce binding, and replay state. Coven checks enrollment
and its signed request before committing the nonce and token ID to
SQLite. This package covers the documented ID-token profile. Provider
discovery, key fetching and rotation, and OAuth exchanges belong to
the application.

Build version `0.1.0` from the monorepo root.

```sh
grimoire build witchyc/packages/oidc
```
