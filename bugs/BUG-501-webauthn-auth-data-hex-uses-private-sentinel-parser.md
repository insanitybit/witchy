# BUG-501: WebAuthn auth data hex uses private sentinel parser

Severity: HIGH
Status: FIXED
Verified: 2026-07-09 REGRESSION on master c02031b3
Component: `std/webauthn`, `std/encoding`, typed trust-boundary errors

## Problem

`webauthn.verify_assertion` accepted authenticatorData as hex text at a trust
boundary but used a sentinel-style parser internally. Malformed hex could be
interpreted as bytes far enough for later semantic checks to report the wrong
kind of failure.

For WebAuthn, malformed wire encoding and semantic assertion rejection must be
distinct: a malformed `authenticatorData` string is not the same as a well-formed
assertion missing the user-presence flag.

## Resolution

`std/webauthn` now decodes authenticatorData through the fallible bytes decoder
and maps failures into the typed `AssertionError.AuthenticatorDataHex(String)`
case before any flag or signature checks run.

Regression coverage:

- `example_tests::webauthn_authenticator_data_rejects_malformed_hex_before_flags_on_both_backends`

