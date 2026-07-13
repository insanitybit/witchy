# BUG-219: Core Coven promote accepts a string marker instead of verified 2FA

- **Severity:** HIGH
- **Status:** IN PROGRESS — core fix implemented; end-to-end verification blocked
- **Verified:** 2026-07-13 CODE + 21 focused Coven unit tests
- **Component:** `projects/coven`, package release gate, WebAuthn/2FA enforcement
- **Found:** 2026-07-05

## Summary

Release-facing docs described promotion as a human, out-of-band 2FA gate, but
the trusted core Coven endpoint accepted any non-empty client-controlled
`second_factor` string and signed that marker into the released record.

Coven Web already verified WebAuthn at its edge before forwarding to an
anonymous upstream. Direct trusted Coven had no equivalent enforceable proof.

## Resolution

Trusted promotion now derives the factor only from the `amr` claim of the
signature-verified OIDC identity token. The accepted methods are `mfa` and
`webauthn`; `amr` may be the string used by the local IdP harness or the array
shape used by real providers. The request-body `second_factor` field is ignored
in trusted mode.

The token must also carry a fresh, non-empty `jti`. Coven consumes that token
before writing the released record, so the same MFA-attested token cannot
promote twice. The signed record stores the derived value, for example
`oidc-amr:webauthn`, rather than the request marker.

Anonymous mode retains a local confirmation marker. It is local/demo-only when
called directly. Coven Web may place an anonymous Coven behind its own verified,
fresh, single-use WebAuthn edge, but that upstream is an internal deployment
boundary and must not be publicly exposed.

## Regression coverage

- Pure Coven tests accept string and array `amr` forms.
- Pure Coven tests reject an untrusted `second_factor=webauthn` marker when the
  verified `amr` does not attest an accepted method.
- The trusted-publishing e2e test is extended to reject marker-only promotion
  and replay, then require a fresh attested token and the signed
  `oidc-amr:webauthn` record value.

## Implementation blocker

The focused Coven suite is green, but the e2e registry server cannot start on
current `master`. `Coven` stores `Dir` and `Secret` authority and its route
handlers close over that state. RFC-0005's current GC-struct slice supports
named capability records, but still rejects capability-carrying closure
environments. The exact e2e therefore stops at type checking before it can
exercise promotion. This bug cannot be marked fixed until RFC-0005 Stage 4
lands and the trusted-publishing e2e passes.

## Acceptance

- In trusted/production-like mode, an arbitrary non-empty marker cannot release
  a package.
- The PM promote flow requires an IdP-attested `amr` in trusted mode and labels
  its request marker as anonymous/local confirmation only.
- README, book, local-registry spec, Coven Web security model, Coven comments,
  and the package-manager RFC describe the same enforceable boundaries.
