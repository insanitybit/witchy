---
rfc: 0010
title: Social login for the coven web console (OAuth / OIDC human identity)
status: implemented
created: 2026-06-23
implemented: 2026-06-24
tracking: commit 99cfeaa (GitHub), 0efcaae (Google)
---

# RFC-0010: Social login for the coven web console (OAuth / OIDC)

> **Status: implemented** (2026-06-24), both providers, end-to-end tested. The
> design thesis (social login mints the same session the promote gate already
> checks) shipped intact; several *specifics* diverged from this proposal — see
> *Implementation notes* for the as-built routes, config shape, state handling,
> secret mechanism, and session delivery.

## Summary

Let a human sign in to coven-web with **"Log in with GitHub"** / **"Log in with
Google"** via the OAuth 2.0 Authorization-Code flow, establishing the same
server-issued session that already gates the write console (promote / yank). This
is the **human-identity** counterpart to the machine-publish identity in
`rfcs/coven-namespaces-plan.md`: publishing is proven by a CI OIDC token; releasing
is gated by a human who signed in. It is also the concrete realization of that
plan's §4.5 promote gate — "a human, via any login method, with a second factor" —
and a dogfood: a capability-secure OAuth client written in witchy on `std/server` +
`std/http`.

## Motivation

coven-web's only human auth today is a device-bound passkey. That is strong but
local; real maintainers expect portable, provider-backed identity, and an org wants
"the GitHub account that owns this namespace can release it." Social login provides
that, and it slots exactly into the model we already designed:

- It satisfies `coven-namespaces-plan.md` §4.5 (decoupled promote): the promoter
  authenticates by **any** verified method and need not match the publish provider
  — e.g. publish from GitHub Actions OIDC, release after a **Google** login.
- It reuses the session-token machinery coven-web already has (a bearer token the
  server signs with its `Secret`; promote/yank gated on it). Social login just adds
  a second way to *obtain* that session.

It depends on two prerequisites tracked elsewhere: **RFC-0009** (HTTPS/TLS — the
provider calls are server-side HTTPS) and the OIDC-verification primitives (RS256 +
JWT/JWKS) in the implementation plan.

## Design

### The flow (server-side Authorization Code)

```
1. GET /auth/<provider>/login
     server -> 302 to the provider authorize URL
       (client_id, redirect_uri, scope, signed state)
2. user authenticates at the provider, approves
3. GET /auth/<provider>/callback?code=...&state=...
     server: validate state (signed HMAC, expiry)
             POST provider token endpoint  (HTTPS, RFC-0009, client_secret)
             -> access token / OIDC id_token
             read identity:
               Google  -> verify id_token (RS256, JWKS) -> email
               GitHub  -> GET /user with the access token -> login
             mint the existing session bearer token
     server -> 302 to  /#token=<bearer>&login=<who>
4. app.js reads location.hash on load, stores the token, strips the hash
```

The sensitive halves — the `client_secret` and the code→token exchange — are
**server-side only**; the browser never sees them, and coven-web's
`connect-src 'self'` is never relaxed.

### Provider abstraction (declarative, like `issuers.json`)

A provider is a config row, not code — mirroring the namespaces plan's philosophy:

```jsonc
"github": {
  "authorize_url": "https://github.com/login/oauth/authorize",
  "token_url":     "https://github.com/login/oauth/access_token",
  "userinfo_url":  "https://api.github.com/user",   // GitHub: not OIDC, read /user
  "scope":         "read:user",
  "owner_claim":   "login",
  "owner_id_claim":"id"
},
"google": {
  "authorize_url": "https://accounts.google.com/o/oauth2/v2/auth",
  "token_url":     "https://oauth2.googleapis.com/token",
  "id_token":      true,                              // Google: verify the OIDC id_token
  "scope":         "openid email profile",
  "owner_claim":   "email",
  "owner_id_claim":"sub"
}
```

The one structural difference the table captures: **GitHub OAuth is not OIDC** — its
token endpoint returns an access token, and identity comes from `GET /user`. Google
returns an OIDC `id_token` verified against its JWKS (RS256). The `owner`/`owner_id`
claims map into the same `(authority, subject)` shape the namespace model uses
(`github/<login>` keyed on the immutable numeric `id`).

### Session delivery under strict CSP

The callback is a **top-level navigation**, so the server cannot hand the SPA a
token via `fetch`, and coven-web forbids inline scripts (`trusted-types 'none'`,
`script-src 'self'`). Delivery is therefore a redirect to **`/#session=<token>`**: a
URL **fragment**, which is never sent to the server and never appears in access
logs. `app.js` (already `'self'`, already running) reads `location.hash` on boot,
calls the existing `setToken`, and clears the hash. No inline script, no cookie, no
CSP change — consistent with the bearer + `sessionStorage` model the passkey login
established.

### CSRF state

`state` is single-use and server-bound, mirroring the existing WebAuthn challenge
handling: `/start` derives `state` from `crypto.sign(Secret, "oauth:" + now)` (or a
host random) and records it (`_oauth_state.json` in the served `Dir`, with a short
expiry); `/callback` checks the returned `state` matches and consumes it. PKCE is
added for providers that support it (defense in depth on public-ish flows).

### Identity → release authority

A verified login yields `(provider, owner, owner_id)`. The server records it as a
namespace **maintainer** (TOFU on first sight) and uses the **session** as the
human-presence proof the promote gate requires. Combined with coven's existing
separation-of-duties check (`promoter != uploaded_by`), a signed-in maintainer can
promote — exactly the `coven-namespaces-plan.md` §4.5 contract, no namespace match
between the promoter's login and the staged version's publish identity required.

### Secrets & operator setup

`client_id` is public (config/arg). `client_secret` is provided per provider via the
Secret mechanism and stays server-side. Registering the OAuth app in each provider's
console (callback `…/auth/<provider>/callback`) is **operator setup** that cannot be
automated; it produces the client id/secret the operator supplies. The deployment
grants `--net github.com:443` (+ `api.github.com:443`, Google's hosts) and the
secrets. (The allowlist is the bare `host:port`; `tls:` is a connect-time choice on
the dialed address, per RFC-0009 as implemented — not an allowlist entry.)

## Implementation notes (as built, 2026-06-24)

The proposal's intent shipped; these specifics differ from the prose above:

- **Routes** are `/auth/github/login` + `/auth/github/callback` and
  `/auth/google/login` + `/auth/google/callback` (not `/auth/<provider>/start`).
- **Provider config is per-provider CLI args + handlers, not a declarative table.**
  `coven_web.witchy`'s `main` takes `github_client_id` and (overridable) GitHub
  base/api URLs, plus `google_client_id` and (overridable) Google authorize/token/
  JWKS URLs; the handlers (`h_github_*`, `h_google_*`) are wired explicitly. A
  declarative `provider table` (the §"Provider abstraction" sketch) was not built —
  it is a clean future refactor once a third provider appears. The Google issuer is
  the fixed well-known `https://accounts.google.com`.
- **The flows compose `std/oauth` + `std/jwt`:** GitHub = `oauth.authorize_url` →
  `oauth.exchange_code` → `oauth.bearer_get_json("…/user")` → read `login` (no JWT).
  Google = `oauth.authorize_url("…", scope "openid email", state)` →
  `oauth.exchange_code_id_token` → `http.get_url` the JWKS → `jwt.kid` →
  `jwt.rsa_key_for_kid` → `jwt.verify_oidc(id_token, der,
  "https://accounts.google.com", client_id, now)` → read `email`.
- **CSRF `state` is a signed, stateless HMAC, not a file.** `sign_state(key, clock)`
  = `"${exp}.${crypto.sign(key, "oauth-state:${exp}")}"`; `valid_state` re-checks the
  signature + expiry. No `_oauth_state.json`, and **PKCE is not implemented** (the
  signed state plus the server-side `client_secret` carries the CSRF defense; PKCE is
  a possible future add).
- **Secrets ride `SecretStore` (coven's idiom).** `coven_web.witchy`'s `main` takes
  `secrets: SecretStore`; the signing key is `secrets.require("signing")`
  (`--signing-key` is sugar for `--secret-file signing=`), and the OAuth secret is the
  named secret `github_client_secret` / `google_client_secret` (`--secret
  github_client_secret=…`), read with `crypto.reveal` only at the token POST.
- **Session delivery is `/#token=<bearer>&login=<who>`** (not `/#session=`); `app.js`
  reads the fragment and uses the bearer exactly like a passkey session.
- **Login mints a session = human-presence proof for the promote gate.** It does NOT
  itself record the login identity as a namespace *maintainer*: coven's maintainer
  check keys on the *publish* OIDC token's `iss|sub` (see the trusted-publishing
  flow), while a social-login session satisfies the §4.5 "a signed-in human + second
  factor" requirement plus the separation-of-duties check. So the §4.5 contract holds
  (publish identity ≠ promote identity), just not via a login→maintainer mapping.
- **A `now`-units bug the e2e caught:** JWT `exp`/`nbf` are unix *seconds*, but
  `clock.now()` is milliseconds — Google's `verify_oidc` is passed `clock.now() / 1000`.

## Alternatives

- **Passkey-only (status quo)** — strong but device-bound and non-portable; no
  "the GitHub owner may release." Social login complements it (both mint the same
  session); neither replaces the other.
- **Cookie sessions** — would fit OAuth's redirect shape more naturally, but
  coven-web is deliberately bearer + `sessionStorage` (no ambient cookie, smaller
  CSRF surface). The `/#session=` fragment keeps that model; rejected cookies.
- **Popup + `window.postMessage`** — coven-web mandates `COOP: same-origin`, which
  complicates popup/opener messaging. The full-page redirect flow needs none of it.
  Rejected.
- **SAML / enterprise SSO** — heavier protocol, deferred; the provider table can
  grow an OIDC-discovery-based generic provider later (shared with the namespaces
  plan).
- **Do nothing** — passkey-only; the promote gate stays device-bound and the §4.5
  "any login method" promise is unrealized.

## Drawbacks

- **Per-provider human setup** that can't be driven from code (registering the
  OAuth app, generating the secret). Documented, not automatable.
- **Provider quirks** — GitHub is OAuth-not-OIDC (extra `/user` call); the table
  absorbs it but it is real special-casing of two shapes.
- **Dependency depth** — needs RFC-0009 (TLS) *and* the RS256 + JWT/JWKS primitives
  (impl plan) before any of it runs.
- **Fragment delivery** puts a short-lived token in a URL fragment (history-visible,
  not network-visible). Mitigated by short TTL + immediate hash-strip; acceptable
  for a bearer the server re-verifies on every write.

## Prior art

- OAuth 2.0 Authorization Code + OIDC; GitHub OAuth Apps; Google Identity (OIDC).
- `rfcs/coven-namespaces-plan.md` — the machine-publish identity model and the §4.5
  promote gate this fulfills.
- `rfcs/0009-https-tls-client.md` — the transport this requires.
- coven-web's existing passkey session (`projects/coven-web/`): the session token,
  `require_session` gate, and `setToken`/`sessionStorage` this reuses.
