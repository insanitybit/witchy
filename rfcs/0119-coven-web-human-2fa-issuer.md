# RFC-0119: coven-web as a human-2FA identity issuer (in-browser promote/yank on a trusted registry)

- Status: Implemented
- Author: coven-web / registry track
- Depends on: RFC-0116 (live OIDC issuer trust), the trusted-publishing model in `projects/coven`

## Problem

On a **trusted** registry (one configured with any `--trust-issuer*`), publishing,
promoting, and yanking each require a short-lived **identity token** from a trusted
issuer, not a session bearer. Publishing works: CI mints a GitHub Actions OIDC token
(RS256) and coven verifies it (RFC-0116). **Promote and yank do not work from the
browser**: coven-web verifies a WebAuthn assertion server-side and then forwards a
`{promoted_by, second_factor}` body to coven, but on a trusted registry coven ignores
client-asserted identity and demands a verified identity token whose `amr` claim
attests `webauthn`/`mfa` (`authorized_promoter`). A browser passkey assertion is not
such a token, so coven correctly refuses with
`promoting on a trusted registry requires a short-lived identity token`.

This is by design — releasing a staged version is meant to require a *human presenting
a second factor at a distinct system* (separation of duties from the machine that
published). The missing piece is that **coven-web is exactly that system** (it
authenticates a human via passkey/WebAuthn) but is not wired as an **issuer** coven
trusts. So the gate is unsatisfiable from the browser, which is where a human-2FA
promotion belongs.

## Why not "just mint an RS256 token"

coven verifies identity tokens as **RS256** (`jwt.verify_oidc_fresh` →
`crypto.rsa_pkcs1_sha256_verify`). witchy's crypto surface has RS256 **verify** but
**no RSA signing** — the only native signing primitive is **Ed25519** (`crypto.sign`,
used with `--signing-key`). coven-web cannot produce an RS256 token, and adding RSA
private-key signing to the language is a much larger, unrelated crypto-surface change.

## Design: an internal EdDSA issuer

coven-web already holds an Ed25519 signing key (`--signing-key`, the
`COVEN_WEB_SIGNING_SEED`) and already verifies the passkey ceremony server-side. We make
coven-web mint an **EdDSA** (`alg: "EdDSA"`, Ed25519) identity token *at the moment the
WebAuthn assertion verifies*, and teach coven to verify an EdDSA token from a trusted
**internal** issuer. External issuers (GitHub Actions) stay RS256, unchanged.

EdDSA is appropriate here precisely because this is an *internal* issuer: the token is
minted and consumed on the same machine (coven-web → coven over loopback) and never
crosses a network. GitHub-style external issuers remain RS256/JWKS.

### Frozen token contract

coven-web mints, immediately after `webauthn.verify_assertion` succeeds in
`wa_do_promote` / `wa_do_yank`, a compact JWT (`mint_2fa_token`):

- Header: `{"alg":"EdDSA","typ":"JWT"}`
- Claims:
  - `iss`  = the coven-web issuer id (its public origin, e.g. `https://witchy.fly.dev`)
  - `aud`  = `"coven-registry"` (coven pins this audience)
  - `sub`  = the authenticated session subject (the signed-in maintainer; **never** a
    client body field — same source as today's `promoted_by`, `session_subject`)
  - `amr`  = `["webauthn"]` — truthful: a WebAuthn assertion was just cryptographically
    verified server-side
  - `iat`  = now (unix seconds)
  - `exp`  = now + 120 s (short; the token only has to survive one loopback hop)
  - `jti`  = `"<sub>|<op>|<name>@<version>|<iat>"`, for defense-in-depth

Signature: Ed25519 over the ASCII `base64url(header).base64url(claims)`, the raw
signature bytes `base64url`-encoded as the third segment. (`crypto.sign` returns a **hex**
signature; the mint hex-decodes to bytes then base64url-encodes via
`encoding.hex_to_base64url`; the verifier reverses it with `encoding.base64url_to_hex`
before calling `crypto.ed25519_verify`, which takes hex.)

The token is added as the request body's `id_token` field forwarded to
`/coven/promote` (and `/coven/yank`). coven's existing `authorized_promoter` /
`authorized_yanker` then verify it exactly like any trusted token, find the `webauthn`
`amr`, and apply the existing maintainer/TOFU + (for yank) existing-maintainer rules.

### Verify side (std/jwt + coven_trust)

- `std/jwt.witchy` gains an EdDSA path mirroring the RS256 one:
  - `sign_eddsa(claims: Json, key: Secret) -> String`
  - `verify_eddsa(token, ed25519_pubkey_hex, audience, now)` — pins `alg == "EdDSA"`
    (never honours the header to pick the algorithm — the issuer's configured key type
    decides, closing alg-confusion, mirroring the RS256 `alg`-pinning of BUG-250).
  - `verify_oidc_eddsa` + `verify_oidc_fresh_eddsa` mirroring the RS256 freshness
    wrappers (issuer match, `iat` present, `exp - iat <= max_lifetime`, `clock_skew`).
- `coven_trust.Issuer` gains an Ed25519 key form (a 4th field `ed25519: Option(String)`).
  `verify_token` dispatches on the issuer's configured key type: an Ed25519 issuer
  verifies EdDSA and **only** EdDSA (`verify_oidc_fresh_eddsa(..., "coven-registry", now,
  600, 60)`); an RSA/JWKS issuer verifies RS256 and only RS256. No token's header can
  cross issuer key types.
- A new trust-spec form `iss=ed25519:<pubkey-hex>` in `parse_issuer_arg`.

### Deploy

The container registers coven-web as a trusted issuer of coven:
`--trust-issuer <origin>=ed25519:<coven-web-ed25519-pubkey-hex>`. With a **stable**
`COVEN_WEB_SIGNING_SEED` secret, the public key is stable and computed once; the
entrypoint injects the trust spec alongside the GitHub OIDC issuer. coven-web mints
`iss=<origin>` tokens signed by that seed.

## Security analysis

- **Truthful `amr`.** The token is minted only *after* `webauthn.verify_assertion`
  returns Ok — a real, cryptographically verified human second factor. It is never minted
  from a mere session bearer.
- **Separation of duties.** The publisher is the CI machine identity
  (`repo:org/repo:…`); the promoter is the human session subject — distinct identities, as
  the model requires (`promote_checked`: `promoter != uploaded_by`). The first promoter of
  an unclaimed namespace becomes its maintainer (existing TOFU, unchanged); yank still
  requires an existing maintainer (SEC-018).
- **`sub` provenance.** `sub` is the authenticated session subject (`session_subject`),
  never a client-supplied field (BUG-278 preserved). A body `second_factor` marker is
  ignored ("a request marker is not proof").
- **Blast radius.** The Ed25519 issuer key is coven-web's existing signing key; it can
  mint only `aud=coven-registry`, short-lived, `amr=webauthn` tokens for the
  *authenticated* subject. It cannot mint publish tokens' `repository`/`workflow_ref`
  claims, so it cannot bind a publisher policy. Whoever holds this key can forge promote
  authority, so it must exist only on the server (tmpfs, unprivileged user).
- **No network exposure.** Minted tokens travel coven-web → coven over loopback only,
  within a 120 s window, and carry a `jti`. External clients cannot present an EdDSA token
  to coven unless coven trusts an EdDSA issuer they control — which is never configured.
- **alg-confusion.** The verifier pins `alg` per issuer key type; an attacker cannot
  submit an EdDSA token to an RSA issuer slot or vice-versa.

## Acceptance (met)

1. `std/jwt` EdDSA mint+verify round-trips on **both** backends
   (`jwt_eddsa_mint_and_verify_roundtrip_backends_agree`), with wrong-aud / expired /
   wrong-iss / tampered rejections byte-identical.
2. coven verifies an EdDSA identity token from a trusted Ed25519 issuer and rejects it
   for a wrong audience / expired / bad signature / RS256-in-EdDSA-slot.
3. Verified live: a coven-web-shaped `amr=webauthn` token releases a staged version
   (`greeter@0.1.0` staged→released, HTTP 200); a valid trusted token *without* `amr`
   (CI shape) is refused 403 "trusted promotion requires ... `amr` ... `mfa` or
   `webauthn`; a request marker is not proof".
4. Publishing (RS256 GitHub Actions path, RFC-0116) is unchanged and still green.

## Non-goals

- RSA private-key signing in witchy.
- Making the CI OIDC path able to promote (it lacks a human `amr`; that is intentional —
  CI publishes, humans release).
- A general external EdDSA-OIDC issuer story — this issuer is internal by construction.
