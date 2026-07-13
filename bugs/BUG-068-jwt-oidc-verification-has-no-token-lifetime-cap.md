# BUG-068: JWT OIDC verification has no token lifetime cap

- **Severity:** LOW
- **Status:** FIXED
- **Verified:** 2026-07-13 TESTED on `fix/bug068-oidc-max-ttl`
- **Component:** `std/jwt`, trusted publishing, social login
- **Found:** 2026-07-05
- **Source:** `security-eval/findings/SEC-029-jwt-no-max-ttl.md`

## Summary

`std/jwt.verify_oidc` checked signature, issuer, audience, expiry, `nbf`, and
`azp`, but it did not encode the short-lived-token assumption used by trusted
publishing and social login. A trusted issuer could sign a token valid for an
arbitrarily long interval.

## Resolution

`jwt.verify_oidc_fresh` is the explicit relying-party policy API. It performs
the complete `verify_oidc` validation and additionally:

- requires an integer `iat` claim;
- rejects an `iat` before the Unix epoch or after `exp`;
- rejects `exp - iat` above the caller's maximum lifetime;
- rejects a future `iat` beyond the caller's allowed clock skew; and
- rejects negative policy inputs.

The generic `verify_oidc` API remains compatible because RFC 7519 makes `iat`
optional and OIDC leaves the acceptable issuance range to the relying party.
Its documentation directs short-lived identity workflows to the stricter API.

Coven trusted publishing permits a maximum signed lifetime of 600 seconds and
60 seconds of issuer clock lead. The local IdP defaults to a 300-second token.
Coven Web's Google login permits Google's documented one-hour ID-token lifetime
and the same 60-second clock allowance.

Clock skew applies only to future `iat`: it never extends signed `exp` or `nbf`
validity.

## Regression coverage

The real-RS256 differential test runs the policy on the interpreter and WASM
backend and covers:

- a valid short-lived token;
- `iat` exactly at the allowed skew boundary;
- a missing `iat`;
- a lifetime one second above the maximum; and
- an `iat` one second beyond the skew bound.

Typed errors carry the actual and accepted values for policy failures, so
callers do not parse diagnostics.

The existing Coven Web e2e cannot currently reach its Google callback on
`master`: Coven Web is rejected at startup because its pre-existing
`Dir`-capturing handler closure requires the deferred RFC-0005 capability-
aggregate lowering. The focused failure occurs before `select_and_verify`; it
is tracked outside this JWT policy change and must be rerun when that compiler
blocker lands.

## Acceptance

- Trusted-publishing verification rejects a token whose signed lifetime exceeds
  the accepted short-lived window.
- Missing and far-future `iat` claims are rejected by security-sensitive callers.
- `std/jwt` generated documentation describes the freshness semantics and the
  caller-selected policy.
