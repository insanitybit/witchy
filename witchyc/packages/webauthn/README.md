# WebAuthn assertion verification

This Witchy package verifies the bounded ES256 assertion profile used by
Coven. It requires no Witchy capability grants. The cryptographic hash
and signature checks use the Wasm host's public crypto operations.

`verify_es256_assertion` accepts a registered COSE key, the exact
clientDataJSON bytes, the authenticator data, the expected relying-party
ID, the DER signature, and the stored counter. On success it returns the
verified counter. The package caps clientDataJSON at 64 KiB and the DER
signature at 72 bytes. The caller must independently bind clientDataJSON to
the request's type, origin, challenge and cross-origin policy, select the
registered credential, and store the accepted counter as part of the
application transaction. Coven does these checks in its approval layer.

The accepted COSE key is a 77-byte, definite-length CBOR map with five
distinct labels for an EC2 P-256 ES256 key. The coordinate fields are
32-byte byte strings. Authenticator data is exactly 37 bytes and must
assert user presence and user verification. Backup state requires backup
eligibility. The counter must increase unless both the stored and observed
counts are zero. The package handles assertions for already registered
keys. Registration and attestation remain application responsibilities.

Build a versioned package from the monorepo root.

```sh
grimoire build witchyc/packages/webauthn
```

Use the package as `import "@webauthn"` with version `0.1.0` in the
project's dependencies. To check its signed test vector, run
`grimoire test witchyc/packages/webauthn`.
