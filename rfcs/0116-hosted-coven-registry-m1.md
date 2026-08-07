---
rfc: 0116
title: "Hosted coven registry, milestone 1: deployable on Fly.io"
status: accepted
created: 2026-08-07
tracking: "Accepted 2026-08-07. M1 tracks: (1) scheme-aware registry addressing, (2) live JWKS-over-HTTPS issuer trust, (3) deploy artifacts + runbook, (4) status-doc refresh. Delegated TUF keys, WebAuthn RP, and custom domains are explicitly deferred."
---

# RFC-0116: Hosted coven registry, milestone 1 — deployable on Fly.io

## Summary

Take the self-hosted coven registry (`projects/coven`) and pm client
(`projects/pm`) from loopback-only dogfood to a deployable, TLS-fronted hosted
service on Fly.io, reachable by the shipped pm client over HTTPS, trusting a
real OIDC issuer (GitHub Actions) via live JWKS discovery. Platform domains
(`*.fly.dev`) are used for now; a purchased domain — and with it the WebAuthn
relying-party work from RFC-0107 — is deliberately deferred, because RP IDs
should not churn.

`spec/local-registry.md` already names the gap precisely: what separates the
local registry from production is operational — TLS termination, a real OIDC
issuer with live JWKS discovery, a TUF root-key ceremony, and backups. This RFC
scopes the first, second, and (documentation-level) fourth of those, plus the
client-side defect that makes any of it unreachable.

## Motivation

Binary releases shipped (v0.1.0, 2026-08-07). The next infrastructure step the
project needs is a real package registry: coven + pm already implement
OIDC-bound publishing, TUF snapshot/timestamp verification, vendoring with
capability-widening gates, and separation-of-duties promotion — all e2e-tested
— but only against loopback HTTP and synthetic issuers. Nobody outside a
checkout can use any of it.

## The blockers, verified in-tree

1. **The pm client cannot speak HTTPS.** `projects/pm/src/pm.witchy`
   `registry_origin` hardcodes `"http://${host}:${port}"`, and `strip_scheme`
   deletes any `https://` from `COVEN_URL` before the origin is rebuilt as
   `http://`. The native Fetch provider itself is fine — it parses `https`
   origins and dials rustls (aws-lc-rs provider) — the scheme is simply
   discarded before it gets there.
2. **The Rust bootstrap's auto-grant drops the scheme too.**
   `src/commands/embedded_pm.rs` trims `http(s)://` off `COVEN_URL` to build
   the Net allow-list entry, and an `https://host` URL without an explicit port
   produces a portless grant instead of `host:443`.
3. **Issuer trust is pinned-key only.** `coven-serve` accepts
   `issuer=pubkeyhex` or `issuer=jwks:<inline json>`; RS256 verification and
   `kid` selection are real (`projects/coven/src/coven_trust.witchy`), but
   there is no live JWKS-over-HTTPS discovery, so a real IdP (GitHub Actions,
   `https://token.actions.githubusercontent.com`) cannot be trusted without
   hand-copying keys.
4. **Zero deployment artifacts.** No container image, no fly.toml, no runbook,
   no backup story, no key ceremony. The signing seed is `openssl rand` into a
   file by a demo script.

## Milestone 1 scope

### 1. Scheme-aware registry addressing (pm + bootstrap)

The registry address becomes an *origin* end-to-end:

- `coven_addr` preserves the scheme from `COVEN_URL`/the positional argument;
  bare `host:port` keeps meaning `http://host:port` (loopback compatibility),
  and the default stays `http://127.0.0.1:8787`.
- `parse_hostport` grows into origin parsing that yields (scheme, host, port),
  defaulting the port from the scheme (`https` → 443, `http` → 80) when the
  authority has none. Invalid schemes fail loudly.
- `registry_origin` renders the parsed scheme instead of a literal `http`.
- `embedded_pm.rs`'s COVEN_URL auto-grant appends the scheme-default port when
  the URL has none, so `COVEN_URL=https://coven-witchy.fly.dev` yields the Net
  grant `coven-witchy.fly.dev:443`.
- Tests: unit coverage for the address parser (scheme/port defaulting,
  rejection), plus an e2e that exercises a pm command against a `serve_tls`
  loopback registry (std `server.serve_tls` exists and is already tested;
  reuse its fixture certificates) proving the client's HTTPS path end to end.

### 2. Live JWKS-over-HTTPS issuer trust (coven-serve)

- New trust spec form: `--trust-issuer-oidc <issuer-url>`. At startup,
  coven-serve fetches `<issuer>/.well-known/openid-configuration`, follows
  `jwks_uri`, and installs the JWKS for that issuer — through an explicit
  `Fetch` capability granted to exactly those origins (the capability model is
  the point: the registry's outbound reach is visible in its grants).
- Startup fails loudly if discovery fails; a registry must never come up with
  silently-empty trust. A `refresh-trust` admin path (or restart) re-fetches;
  key rotation cadence is documented in the runbook. No background timer in
  M1 — restarts are cheap on Fly.
- The existing pinned-key and inline-JWKS forms remain (tests and air-gapped
  deploys use them).
- Server-side changes stay inside `projects/coven/src/coven_trust.witchy` +
  argv plumbing in `coven.witchy`; the verification code paths (RS256, `kid`)
  are already there and already differential-tested.

### 3. Deploy artifacts + runbook

- `projects/coven/deploy/Dockerfile`: multi-stage — build the release `witchy`
  binary, then a slim runtime image whose entrypoint runs
  `witchy coven-serve 0.0.0.0:8080 --root /data --signing-key /secrets/...`
  with exactly the grants the program's capability footprint demands.
- `projects/coven/deploy/fly.toml`: single instance, a Fly volume mounted at
  `/data` for the registry store, internal port 8080, Fly edge terminating
  TLS. (In-process `serve_tls` stays available for non-Fly deploys but is not
  the M1 path — the edge proxy owns certificates.)
- `projects/coven/DEPLOY.md` runbook: signing-seed generation as a documented
  ceremony (generated offline, stored as a Fly secret, never in the image or
  repo; loss/rotation procedure), volume snapshot backups + a restore drill,
  trust configuration for GitHub Actions OIDC, log/health checks, and the
  explicit statement of what this deployment does NOT promise (mirrors
  PRODUCT-STATUS.md's experimental labeling).
- CI builds the Docker image (no push) so the artifact cannot rot silently.

### 4. Status-doc refresh

`rfcs/package-manager.md` §15 still describes the deleted Rust `src/pm/*`
implementation; `RELEASE-READINESS.md` predates the shipped v0.1.0 release.
Both get refreshed to current reality as part of this milestone so the next
contributor doesn't navigate by a stale map.

## Explicit non-goals (deferred)

- **Delegated per-namespace TUF keys and a real root-key ceremony** beyond the
  documented single-key procedure — M2, tracked in `rfcs/package-manager.md`.
- **WebAuthn relying party + custom domain** — waits for a purchased domain
  (RFC-0107's external ledger); RP IDs must not churn.
- **Compiled-backend coven-serve** — the registry stays on the interpreter
  (it needs the `compiler.footprint` host intrinsic); acceptable for a
  single-instance experimental service, revisit under load.
- **Availability/durability promises** — the deployment inherits coven's
  Experimental status; the runbook says so.

## Acceptance

M1 is done when: a pm client on a laptop with
`COVEN_URL=https://<app>.fly.dev` can `pm add` a package published from a
GitHub Actions workflow whose OIDC identity the registry verified via live
JWKS discovery — with TUF chain verification and the capability-widening gate
active — and the deploy is reproducible from `DEPLOY.md` alone by someone who
has never read the coven source.
