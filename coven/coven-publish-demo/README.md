# coven-publish-demo — trusted publishing from CI

A tiny, zero-authority rune (`insanitybit/greeter`) whose only job is to prove
the **"outsider can publish via CI"** path against the hosted coven registry
(RFC-0117 Lane A). No API key, no long-lived secret — the publisher's identity
is a short-lived GitHub Actions **OIDC token**, verified by the registry.

## How trusted publishing works here

1. The workflow [`.github/workflows/coven-publish-demo.yml`](../../.github/workflows/coven-publish-demo.yml)
   runs a single job with least-privilege `permissions: { contents: read,
   id-token: write }`.
2. That `id-token: write` grant lets the job ask the Actions runtime for an OIDC
   token scoped to **audience `coven-registry`** (via the runtime-injected
   `ACTIONS_ID_TOKEN_REQUEST_URL` / `ACTIONS_ID_TOKEN_REQUEST_TOKEN`). The token
   is a compact RS256 JWT signed by GitHub, carrying claims `iss =
   https://token.actions.githubusercontent.com`, `aud = coven-registry`,
   `repository`, `repository_owner`, `workflow_ref`, a single-use `jti`, and a
   short `exp`.
3. The job exports it as `COVEN_ID_TOKEN` and runs
   `COVEN_URL=https://witchy.fly.dev witchy pm publish projects/coven-publish-demo`.
   `pm publish` sends the token through opaquely in the publish envelope.
4. The registry verifies it (no client-asserted identity is trusted):
   - `coven_trust.verify_oidc_fresh` checks the RS256 signature against the
     issuer's pinned JWKS key (selected by the token `kid`), that `aud ==
     coven-registry`, and that the token is fresh (`iat` present, signed
     lifetime `<= 600s`, `<= 60s` issuer clock lead), and that the `jti` has not
     been seen before (single-use, replay-rejected).
   - On the **first** trusted publish it binds the namespace to the token's
     `repository` org: `coven.witchy` requires `namespace == namespace_of(
     repository)`. Because the namespace is `insanitybit` and the repo is
     `insanitybit/witchy`, the org matches. Every later publish must match that
     first binding on `issuer` + `repository` + `workflow_ref`.

The published version lands **STAGED**. That is the whole demo — publish only.

## Publish is not promote

Moving a staged version to **released** is a separate, deliberately human step:
the registry requires the identity token to carry an IdP-attested MFA/WebAuthn
`amr` (`pm promote`, driven by a logged-in human). CI never promotes. This keeps
the "a machine can stage, only a human can release" separation of duties.

## What the operator must configure server-side

The workflow is correct on its own but cannot succeed until the **live registry
trusts the GitHub Actions issuer**. The operator must run `coven-serve` with:

```sh
witchy coven-serve \
  --addr 0.0.0.0:8787 \
  --root /data/registry \
  --secret-file signing=/secrets/coven-signing.seed \
  --trust-issuer-oidc https://token.actions.githubusercontent.com
```

`--trust-issuer-oidc` makes coven fetch GitHub's OIDC discovery document and
JWKS at startup (through a Fetch derived from its Net grant) and accept RS256
tokens from that issuer, selecting the verifying key by the token's `kid` so a
GitHub key rotation does not break publishing.

Two invariants that must hold for a dispatch to succeed:

- the registry runs with `--trust-issuer-oidc
  https://token.actions.githubusercontent.com`; and
- the rune namespace (`insanitybit`) equals the workflow's `repository_owner`.

## Running the demo

The workflow is guarded so it cannot fire by accident — it triggers only on
manual **Run workflow** (`workflow_dispatch`) or an explicit maintainer-pushed
`demo-publish-vX.Y.Z` tag; never on ordinary pushes or pull requests. After the
operator enables issuer trust, dispatch it from the Actions tab. A stranger can
then resolve the staged rune with:

```sh
COVEN_URL=https://witchy.fly.dev witchy pm add insanitybit/greeter
```
