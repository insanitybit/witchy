---
status: implemented
note: Implementation + stdlib-expansion plan behind RFC-0009, RFC-0010, and coven-namespaces-plan. Design decisions live in those; this is the build sequence.
implemented: 2026-06-24
---

# Identity stack — implementation plan

> **Status (2026-06-25): executed.** Every workstream that gates the goal shipped.
> WS-1 TLS landed on **rustls 0.23 + aws-lc-rs** (not s2n-tls — see the WS-1 note and
> RFC-0009); WS-2 RS256, WS-3 base64url, WS-4 JWT/JWKS (`std/jwt`+`std/oauth`),
> WS-5 OAuth state (signed stateless HMAC), WS-6 social login, WS-7 dev IdP
> (`src/idp.rs`), WS-9 publish/promote OIDC (coven), and WS-8 3-segment names are all
> live with differential tests. The "Blocker summary (verified 2026-06-23)" table
> below is the **pre-work snapshot** — every row marked *missing/partial* is now
> resolved at the location named in its last column.

The "log in with GitHub/Google" + OIDC-trusted-publishing direction needs three
**design** documents (the new features) and a body of **implementation / stdlib**
work (this file). The new-feature design lives in:

- `rfcs/0009-https-tls-client.md` — HTTPS/TLS as a `tls:` address scheme on `Net` (transport).
- `rfcs/0010-web-console-social-login.md` — human OAuth/OIDC login for coven-web.
- `rfcs/coven-namespaces-plan.md` — machine-publish OIDC identity + namespaces.

Everything below is mechanical implementation or expansion of existing stdlib — no
new design decisions — sequenced with its dependencies. File paths are the real
targets (verified against the current tree).

## Blocker summary (verified 2026-06-23)

| Need | State today | Where it lands |
|---|---|---|
| HTTPS / TLS client | **missing** — `std::net::TcpStream` only, no TLS crate; `std/http` is all `Net[Connect, Tcp]` | WS-1 (RFC-0009) |
| RS256 (RSA) verify | **missing** — `std/crypto` has Ed25519 + ES256 only | WS-2 |
| URL-safe base64 | **partial** — `encoding` has `base64_*` + `base64url_of_hex`, no `base64url_decode` | WS-3 |
| JWT / JWKS parse+verify | **missing** — no jwt/jose anywhere | WS-4 |
| OAuth state / nonce | available — `getrandom` is a host dep; or derive from `Secret`+`Clock` | WS-5 |
| server redirect / query / headers | **present** — `server.redirect`, `server.query`, `server.with_header` | WS-6 |
| ES256 verify | **present** — `crypto.ecdsa_p256_verify` | — |
| crypto backend | **aws-lc-rs** (FIPS), native; `sha2`/`ed25519-dalek` are the wasm32 fallback only | WS-2, WS-1 |

The two genuine new primitives are **TLS** (WS-1) and **RS256** (WS-2); everything
else composes existing stdlib.

## Dependency graph

```
WS-2 RS256 ┐
WS-3 b64url ┘─> WS-4 JWT/JWKS ─┬─> WS-6 social login (RFC-0010)
WS-1 TLS ──────────────────────┴─> WS-9 publish/promote OIDC (namespaces)
WS-5 OAuth state ──────────────────> WS-6
WS-7 dev IdP (real OIDC) ───────────> WS-10 tests
WS-8 3-segment names ── independent, land anytime
```

Recommended order: **WS-1, WS-2, WS-3 first** (foundational, unblock everything),
then **WS-4**, then **WS-6** and **WS-9** in parallel. **WS-8** can land immediately
(no dependencies).

## Workstreams

### WS-1 — TLS transport (impl of RFC-0009)
> **As built:** the crate is **rustls 0.23 + aws-lc-rs** (a `CryptoProvider`), not
> s2n-tls — the polling/`s2n-tls-tokio` glue and `unsafe` FFI weren't worth it for a
> blocking host op. Trust roots are webpki-roots (Mozilla CA) plus a
> `WITCHY_TLS_EXTRA_ROOTS` PEM hook. The rest of this workstream shipped as written.
- Add **rustls + aws-lc-rs** to the native build; teach the existing `net_connect` /
  `net_try_connect` host ops in `src/runtime.rs` (and the interpreter path) to
  perform a TLS handshake when the address is `tls:`-schemed (equivalently a sibling
  `net_connect_tls` selected by the address): enforce the allowlist,
  handshake, verify cert, return an opaque plaintext socket handle.
- Wire the WASM host import (same place as `net_connect`) so the `witchy sandbox`
  backend gets TLS host-side → parity.
- **As built**, the allowlist stays **scheme-agnostic `host:port`**:
  `net::parse_scheme` strips a `tls:` prefix off the *dialed* address before the
  `host:port` allowlist check in `src/capabilities.rs`, so `tls:` is a connect-time
  choice, not an allowlist token (`--net github.com:443`, not `--net
  tls:github.com:443`). **No new right** — `Net`'s type stays `Net[Connect, Tcp]`
  (TLS rides on TCP); encryption is an endpoint fact, elected at connect time.
- `std/http`: scheme-dispatch in the existing fns (`get_url`/`request_with`) — an
  `https://` URL dials a `tls:` address — keeping the `Net[Connect, Tcp]` signature;
  default port 443; SNI from host; ALPN `http/1.1`; loud errors, never downgrade.
- Trust roots: OS store by default + `--ca-file` pin; the named `--tls-insecure
  <host>` dev escape (tests only).
- **HTTP/1.1 robustness:** real provider responses use **chunked transfer-encoding**
  and often **gzip**, and OAuth endpoints may **30x-redirect**. Audit `std/http`'s
  response parser (today tuned for the localhost coven proxy) and extend it to
  decode chunked bodies, handle (or `Accept-Encoding: identity` to avoid) gzip, and
  follow a bounded redirect chain. This is a latent blocker independent of TLS.
- **Done when:** a differential test does an HTTPS GET against a local TLS server on
  both backends and gets identical bytes; a chunked response is decoded; a bad cert
  fails closed.

### WS-2 — RS256 crypto intrinsic
- Add `crypto.rsa_pkcs1_sha256_verify(public_key, message, signature) -> Bool` to
  `std/crypto.witchy` (placeholder body), the interpreter intercept, and the WASM
  host import — exactly mirroring `ed25519_verify`. Implement via **aws-lc-rs** RSA
  in the native crypto registry (`src/native.rs` / `src/runtime.rs`).
- Public-key input shape: RSA public key from a JWK (`n`,`e`) → DER/SPKI; decide the
  on-the-wire encoding the intrinsic accepts (hex SPKI, to match the existing
  key-as-hex convention).
- wasm32 note: like the other intrinsics, the in-browser target keeps a pure-Rust
  path or simply doesn't expose it (no `Net`, so no OIDC there anyway).
- **Done when:** a known-answer RS256 vector verifies true/false on both backends.

### WS-3 — URL-safe base64
- Add `base64url_encode` / `base64url_decode` (URL-safe alphabet, no padding) to
  `std/encoding.witchy`. JWT segments are base64url; today only standard
  `base64_decode` and `base64url_of_hex` exist.
- **Done when:** round-trip + JWT-segment decode tests pass on both backends.

### WS-4 — JWT / JWKS verification (stdlib / coven module)
- A `std/jwt` (or `projects/coven/src/coven_oidc.witchy`) module, **shared** by
  social login and trusted publishing: split a compact JWT, base64url-decode the
  header/payload (WS-3) + `json`, select the JWK by `kid`, verify the signature
  (`RS256` → WS-2, `ES256` → existing `ecdsa_p256_verify`), then check
  `iss`/`aud`/`exp`/`iat`.
- JWKS discovery: fetch `<issuer>/.well-known/openid-configuration` → `jwks_uri` →
  JWKS over **HTTPS (WS-1)**; cache by issuer with a TTL; bound the response size;
  fetch failure ⇒ "issuer temporarily unverifiable" (refuse, never downgrade).
- **Done when:** a token minted by the dev IdP (WS-7) verifies; a forged/expired/
  wrong-audience token is rejected with the right reason.

### WS-5 — OAuth state + secrets plumbing
- `state`: single-use, server-bound — derive from `crypto.sign(Secret, "oauth:"+now)`
  (or expose `getrandom` as a small `random` builtin if we want true randomness),
  recorded in `_oauth_state.json` in the served `Dir` with a short expiry, consumed
  on callback. Mirrors the existing `_wa_challenge` handling.
- Per-provider `client_secret` via the existing `--secret <name>=…` Secret cap;
  `client_id` as config/arg.
- **Done when:** a tampered/expired/missing `state` is refused; secrets are read from
  the Secret store, never a file in git.

### WS-6 — coven-web social-login endpoints (impl of RFC-0010)
- `coven_web.witchy`: `GET /auth/<provider>/start` (302 to authorize URL) and
  `GET /auth/<provider>/callback` (state check → token exchange over WS-1 → identity
  via WS-4/`/user` → mint the existing session token → `302 /#session=<token>`).
- Declarative provider table (GitHub OAuth-not-OIDC vs Google OIDC `id_token`).
- Client (`web/src/`): "Log in with GitHub/Google" buttons in the session bar;
  `main.ts` reads `location.hash` on boot, `setToken`, strips the hash. **Reuses**
  the session-token machinery and `require_session` gate verbatim.
- `dev.sh`: grant `--net github.com:443`, `api.github.com:443`, Google's hosts
  (scheme-agnostic host:port — `tls:` is elected at dial time, not in the allowlist);
  pass `--secret github_oauth=…` / `--secret google_oauth=…`.
- **Done when:** end-to-end login (against a mock provider in tests) mints a session
  that passes the promote/yank gate; signed-out is refused.

### WS-7 — dev IdP → real OIDC test fixtures
- `src/idp.rs`: mint **RS256/ES256** provider-shaped tokens and serve a **JWKS**
  endpoint (replacing the Ed25519-envelope stand-in for OIDC tests). Keep it a Rust
  dev/test helper (per RFC-0004 §7) — test scaffolding, not request-path TCB.
- **Done when:** the differential/e2e tests mint real tokens that WS-4 verifies.

### WS-8 — 3-segment name migration (namespaces Phase 0, independent)
- `coven_validate.valid_name` → exactly three `/`-segments + charset; `coven_trust.
  namespace_of` → first two segments (`provider/owner`). Update example/fixture
  manifests and `projects/coven-web/seed-examples.mjs` (its `examples/*` become
  `local/`-or-bare dev data). No auth change.
- **Done when:** validation tests pass; the dev registry still seeds + browses.

### WS-9 — publish/promote OIDC wiring (namespaces Phases 1–4)
- Implements `rfcs/coven-namespaces-plan.md` in `coven_trust.witchy` /
  `coven.witchy` on top of WS-1/WS-2/WS-4: `IssuerCfg` rows, `derive_namespace`,
  `authorize_publish` (derive-equality + immutable-id), the decoupled
  `authorize_promote` (§4.5). Tracked by that RFC; listed here for the dependency.

### WS-10 — tests & parity
- KAT vectors (WS-2), TLS differential (WS-1), JWT verify matrix (WS-4), mock-provider
  login e2e (WS-6), multi-provider namespace proof (namespaces §6 / WS-9). Every
  observable behavior identical on interpreter and compiled backends.

## Open implementation questions

- **RSA public-key encoding** at the `crypto.rsa_pkcs1_sha256_verify` boundary
  (hex SPKI vs raw `n`/`e`) — pick one and keep JWK→key conversion in witchy.
- **Random vs Secret-derived `state`** (WS-5): expose a `random` builtin (getrandom
  already linked) or stay with the `Secret`+`Clock` derivation used for WebAuthn
  challenges. Lean: reuse the derivation (no new capability) unless PKCE wants a
  high-entropy verifier.
- **JWKS cache TTL + size bounds** (WS-4) — pick conservative defaults; surface
  fetch failure as a refused publish, never a downgrade.
- **`std/jwt` vs a coven-local `coven_oidc`** (WS-4) — stdlib if we expect general
  reuse; coven-local if it stays registry-specific. Lean stdlib (`std/jwt`), since
  both publish and login consume it.
