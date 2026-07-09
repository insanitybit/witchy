# BUG-391: PM resolver normalizes registry version coordinates before fetching

Severity: MED
Status: FIXED
Verified: 2026-07-09 REGRESSION on master 1e93cc6f
Component: package manager, `std/semver`, Coven registry client, version identity

## Problem

The package manager resolves versions by parsing registry version strings into
`semver.Version`, then returning `semver.format(v)` to callers. That can discard
the exact registry coordinate selected from `/coven/versions`.

The leading-zero and missing-component forms had already been narrowed by
`canonical_version`, but that helper still trimmed the signed registry string
before comparing. A record version such as `" 1.2.3 "` was accepted as
`Version(1, 2, 3)` and then fetched as `1.2.3`, not as the exact coordinate that
appeared in the registry payload.

## Resolution

`canonical_version` now compares `semver.format(parse(raw))` to the original
untrimmed `raw` string. `semver.parse` may trim internally for ordinary semver
APIs, but PM registry resolution rejects any coordinate whose source text is not
already canonical byte-for-byte.

Regression coverage:

- `e2e::resolver_rejects_whitespace_padded_registry_versions`

Focused validation:

- `cargo nextest run --test e2e -E 'test(resolver_rejects_whitespace_padded_registry_versions)'`
