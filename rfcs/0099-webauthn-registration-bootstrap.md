---
rfc: 0099
title: Verified WebAuthn registration, bootstrap authorization, and signature counters
status: deferred
created: 2026-07-19
superseded-by:
tracking: "split from RFC-0066; revive before remote Coven passkey registration enters a supported release contract, with an operator-authorized bootstrap/recovery model and browser plus malformed-ceremony acceptance fixtures"
---

# RFC-0099: Verified WebAuthn registration, bootstrap authorization, and signature counters

## Summary

Replace Coven Web's first-key, client-supplied registration record with a
server-challenged WebAuthn creation ceremony, an explicit operator-authorized
bootstrap/recovery policy, and persisted monotonic assertion counters.

This RFC is deferred because remote Coven lifecycle is excluded from the 0.1
release contract. It must revive before that registration endpoint is presented
as a supported security boundary.

## Motivation

RFC-0066 delivered fresh assertion verification for promote and yank, but
`h_wa_register` still stores a non-empty credential ID and SEC1 public key sent
by the browser. Checking only an attestation object would prove that a browser
possesses a key; it would not answer the more important bootstrap question:
which human or operator is authorized to install the first recovery root?

Assertion verification also parses the WebAuthn signature counter without
persisting or comparing it, so authenticators that provide a meaningful
counter cannot currently expose a cloned credential.

## Design constraints

- The server mints and persists a single-use `webauthn.create` challenge bound
  to the configured RP ID and expected origin.
- Registration validates client data, authenticator data, credential ID, COSE
  ES256 key shape, RP hash, presence/verification flags, and the chosen
  self-attestation statement before persisting a credential.
- Installing the first credential requires explicit operator/bootstrap
  authority. Rotation and recovery cannot be anonymous variants of first-write
  wins, and failed attempts never mutate the trusted record.
- Every successful assertion atomically compares and updates the stored
  signature counter when the authenticator reports a non-zero counter. A
  regression is a typed authentication failure.
- Browser and server fixtures cover valid creation, malformed CBOR/COSE,
  origin/RP mismatch, replay, unauthorized bootstrap, counter advance, and
  counter regression.

## Revisit condition

Revive this RFC when remote Coven registration is scheduled for a supported
release. The implementation plan must identify the operator bootstrap and
recovery authority before ceremony parsing begins; otherwise cryptographic
validation would preserve first-writer account takeover.

## Alternatives

- **Validate only the submitted SEC1 point:** rejects malformed keys but proves
  neither a WebAuthn ceremony nor authorization to bootstrap.
- **Trust self-attestation without operator bootstrap:** proves key possession
  while leaving the first-registration takeover unchanged.
- **Require a metadata attestation service:** stronger device provenance, but
  not necessary for the initial self-attestation contract.

## Drawbacks

Creation-ceremony parsing adds CBOR/COSE surface and recovery operations add an
explicit operator procedure. Those costs belong in the supported remote
registry cut rather than silently widening the private 0.1 release.

## Prior art

WebAuthn registration/authenticator-data verification and relying-party
credential recovery procedures; RFC-0066 owns the already-implemented assertion
gate.

## Implementation status (2026-08-09)

**Cryptographic foundation LANDED + parity-verified** (this is the reusable core
the rest of the RFC builds on):

- `std/cbor.witchy` — a new minimal CBOR (RFC 8949) decoder for the WebAuthn
  subset (unsigned/negative ints, byte/text strings, arrays, definite-length maps;
  indefinite-length and other types refused). witchy had no CBOR before this.
- `std/webauthn.verify_registration` — the server-challenged **create** ceremony
  verifier: validates clientDataJSON (`type` == `webauthn.create`, challenge,
  origin), CBOR-decodes the `attestationObject` to its `authenticatorData`, checks
  `rpIdHash` + user-presence/verification flags + the AT (attested-credential)
  flag, and extracts the attested credential id and COSE **ES256** public key as an
  uncompressed SEC1 point. Returns a typed `RegisteredCredential {credential_id_hex,
  public_key_hex, sign_count}` or a typed `RegistrationError`.
- `std/webauthn.assertion_sign_count` — extracts the assertion's signature counter
  (authData bytes 33..37) so a caller can compare-and-reject a regression.
- Differential test `webauthn_verify_registration_parses_the_create_ceremony`
  (both backends agree on a valid ceremony and on rejection of a wrong challenge).

**Remaining integration (not yet landed):**

- coven-web wiring: mint a single-use **create** challenge (op `register`), rewrite
  `h_wa_register` to verify via `verify_registration` and persist
  `{credentialId, publicKey, signCount}`, and update the browser `webauthn.ts` to
  run `navigator.credentials.create` and POST `{clientData, attestationObject}`.
- Signature-counter enforcement in the three assert handlers (promote/yank/login):
  after a valid assertion, compare `assertion_sign_count` against the stored count,
  reject a regression, persist the higher value.
- Explicit operator-authorized bootstrap/recovery (beyond the current SEC-001
  first-credential-only bootstrap), which the RFC couples to a stable purchased
  domain (see Non-goals / the RFC-0117 domain prerequisite) and MED-3
  (first-promoter hijack) in RFC-0117 Lane C.
- Browser + malformed-CBOR/COSE/origin/RP/counter fixtures.
