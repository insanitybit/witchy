---
status: in-progress
note: Master ordering across the capability-model RFCs (0011-0013) and the identity/login RFCs (0009/0010/namespaces). Detailed identity workstreams live in identity-stack-implementation-plan.md; this file is the sequence and the decision points.
progress: The identity track (Phases 3-5) shipped — HTTPS/TLS, OIDC verification, GitHub+Google login, and OIDC trusted publishing are all live and dogfooded. The capability-model substrate (Phases 1-2, 6) is partial: RFC-0011's Net tier shipped; RFC-0012 (File), RFC-0013 (grants), and RFC-0011's carried-state/Dir/restrict-retirement remain. See "Status of the artifacts" at the bottom.
---

# Implementation roadmap

> **Status (2026-06-25): the goal shipped on the identity side.** "Log in with
> GitHub/Google" and OIDC trusted publishing are live and dogfooded in coven-web /
> coven. The fast-track deviation noted under Phase 1 is what happened: the identity
> track (Phases 3→4→5) was built on the existing `restrict` string before the full
> RFC-0011 substrate, and the deeper capability-model work (File, carried-state,
> grant documents) is the remaining tail. Per-phase truth is in
> [Status of the artifacts](#status-of-the-artifacts) at the bottom; the phase
> descriptions below are preserved as the original plan of record.

## The goal

A fully **dogfooded** witchy registry: coven-web with real **"Log in with
GitHub/Google"** and **OIDC trusted publishing**, both written in witchy on a
capability model that is precise, refinable, and honest — the registry's own authority
audited the way it audits every rune it serves.

Two tracks feed it:

- **Capability model** — RFC-0011 (refinement: carried state + methods + two tiers),
  RFC-0012 (File capability), RFC-0013 (grant documents). The substrate.
- **Identity / login** — RFC-0009 (HTTPS/TLS), RFC-0010 (social login),
  `coven-namespaces-plan` (OIDC publishing), plus the stdlib work detailed in
  `identity-stack-implementation-plan.md` (RS256, JWT/JWKS, base64url, name-shape).

This file is the **order**; the identity file is the **detail** for its phases.

## Dependency overview

```
RFC-0011 refinement ─┬─> RFC-0012 File ─────────────┐
   (+ RFC-0002 state) │                              │
                      └─> RFC-0009 TLS ──┬─> JWT/JWKS (WS-4) ─┬─> RFC-0010 login ── GOAL
RS256 (WS-2) ─ base64url (WS-3) ─────────┘                   │
                                          └─> namespaces publish ┘
RFC-0013 grant docs ── depends on 0011/0012 ── lands last (ergonomics)
name-shape (WS-8) ── independent ── land anytime
```

Key fact that shapes the order: **GitHub login needs only TLS + the OAuth flow** (GitHub
returns an access token + a plain-JSON `/user`, *no* JWT). **Google login and OIDC
publishing** are what need RS256 + JWT/JWKS. So the first *visible* win is reachable
without the crypto-verification stack.

## Phases (in order)

### Phase 1 — RFC-0011: the refinement substrate
**Objective:** retire the universal `restrict` and land carried-state + library-defined
refinement, because everything below sits on it.
- Replace `restrict(net, "…")` / `subdir(dir, "…")` with **methods**: `net.only(…)`,
  `net.deny(…)`, `dir.subtree(…)`, `dir.only(kind, ext)`. Typed policy values
  (`tls`/`tcp`/`cidr`/`any_port`/`kind`/`ext`); union across args, intersection in the
  method, `deny` as monotone set-difference. Keep a `*_policy("…")` string parser for
  config/`--net`.
- **RFC-0002 extension:** a sealed capability may be a **record** carrying ≥1 underlying
  cap + policy fields, still footprint-transparent. Land a worked `Postgres`-style
  carried-state example (confined `Net` + table-filter; `query` enforces it).
- Migrate existing `restrict`/`subdir` sites (`fmt`-assisted).
- **Files:** `src/capabilities.rs`, `src/typeck.rs`, `std/{net,dir}.witchy` (methods),
  `src/{interpreter,codegen}.rs` (method lowering), RFC-0002 sealing in `src/linker.rs`.
- **Done when:** differential tests for `net.only`/`net.deny`/`dir.*`; a carried-state
  library cap mints, refines, and audits as its underlying cap; `restrict`/`subdir` gone.
- **Decision point:** if this proves too large to land before *any* goal progress,
  fast-track Phase 3 (TLS) + Phase 5-GitHub using the existing `restrict` string, then
  return here and migrate. Default: do Phase 1 first (build the substrate once).

### Phase 2 — RFC-0012: the File capability
**Objective:** `File` as a host-primitive; fold `Exec`.
- `File[Read|Write|Exec]`; `main` accepts files; `dir.open`/`dir.join`/`dir.create`
  navigation (no `../`/absolute escape); `spawn(File[Exec], …)`; remove ambient `Exec`;
  `caps` surfaces `File[Exec] <path> ⚠`.
- **Files:** capability type (`src/capabilities.rs`/`typeck.rs`), `std/{fs,file}.witchy`,
  runtime file/spawn host ops, `caps` formatting, RFC-0004 driver → `File[Exec]`.
- **Done when:** `main(cfg: File[Read])` works both backends; `dir.open` yields a
  rights-≤ `File`; exec path shown with the warning.
- **Note:** independent of the identity track — can interleave with Phase 3+.

### Phase 3 — RFC-0009: HTTPS/TLS + crypto/encoding primitives
**Objective:** the hard transport blocker for the whole identity track.
- s2n-tls + aws-lc host op (`net_connect` learns the `tls:` endpoint, or a sibling
  `net_connect_tls`) — **as built: rustls + aws-lc-rs** (see RFC-0009); cert
  verification mandatory; `std/http` dials `tls:` on `https://`
  URLs (`std/url` already parses them, port 443). **HTTP robustness:** chunked decode,
  redirect-follow, gzip-or-`identity`.
- **RS256** (`crypto.rsa_pkcs1_sha256_verify`, aws-lc) + **base64url** codec —
  identity-stack WS-2/WS-3, small and independent, land alongside.
- **Files:** `src/runtime.rs` (TLS host op), `std/http.witchy`, `std/crypto.witchy` +
  native (RS256), `std/encoding.witchy` (base64url), `Cargo.toml`.
- **Done when:** HTTPS GET to a local TLS server, identical bytes both backends; chunked
  decoded; bad cert fails closed; RS256 KAT passes.
- **Detail:** `identity-stack-implementation-plan.md` WS-1/2/3.

### Phase 4 — OIDC verification (JWT / JWKS) + dev IdP
**Objective:** the shared verification stack for Google login *and* OIDC publishing.
- `std/jwt`: compact-JWT split, base64url+json decode, verify (`RS256`→Phase 3,
  `ES256`→existing `ecdsa_p256_verify`), `iss`/`aud`/`exp`/`iat`; JWKS discovery over TLS
  (cache + size bound; fetch-fail ⇒ refuse, never downgrade).
- `src/idp.rs` → mint real RS256/ES256 tokens + serve a JWKS for tests.
- **Done when:** a dev-IdP token verifies; forged/expired/wrong-aud rejected with the
  right reason.
- **Detail:** identity-stack WS-4/WS-7.

### Phase 5 — The goal: social login + OIDC publishing
**Objective:** the visible payoff.
- **GitHub login first** (needs only Phase 3): `coven_web.witchy`
  `/auth/github/{start,callback}`, provider table, code→token exchange (HTTPS + the
  user's client_id/secret via `--secret`), `/user`, mint the existing session token,
  deliver via `/#session=…`; "Log in with GitHub" button. `dev.sh` grants
  `github.com:443` + `api.github.com:443` (scheme-agnostic; `tls:` is dialed, not
  allowlisted).
- **Google login** (adds Phase 4): same flow, verify the OIDC `id_token` via JWKS.
- **OIDC publishing** (`coven-namespaces-plan` Phases 1-4, on Phase 3/4): `IssuerCfg`,
  `derive_namespace`, `authorize_publish` (derive-equality + immutable-id), decoupled
  `authorize_promote`. **Name-shape migration** (WS-8) is independent — land it early.
- **Done when:** GitHub login mints a session that passes the promote gate (verified via
  a virtual authenticator-style harness / a mock provider); Google login + an OIDC
  publish complete the milestone.
- **Detail:** `RFC-0010`, identity-stack WS-6/WS-8/WS-9.

### Phase 6 — RFC-0013: grant documents + capability polish
**Objective:** ergonomics, last.
- Grant-document format + loader; the **footprint cross-check** (request vs computed
  footprint → warn/error); semi-broad-then-refine wired through.
- **Done when:** `witchy run app --grants app.grants.toml` grants per-`main`; an
  over-request warns, an under-grant errors at launch.

## How I'll run this autonomously

- **First step:** Phase 1, RFC-0011 — start by mapping the current `restrict`/`subdir`
  implementation (`src/capabilities.rs`, the `("restrict",2)`/`("subdir",2)` lowering in
  `codegen.rs`/interpreter) and the `Net`/`Dir` value's carried scope, then introduce the
  `.only`/`.deny`/`.subtree` methods + typed policy values behind them, keeping the string
  parser. Land it green on both backends before touching RFC-0002 state-carrying.
- **Cadence:** one phase = one or more focused, separately-committed, both-backends-green
  changes; `cargo nextest` + `clippy -D warnings` gate every commit; a runnable
  `book/`/example for anything user-visible (the parity rule).
- **Decision levers I'll exercise:** (a) the Phase-1 fast-track deviation above if the
  substrate is too big to front-load; (b) GitHub-login-before-Google (Phase 5) for an
  early visible win; (c) name-shape migration (WS-8) slotted whenever it's least
  disruptive. I'll flag each such call when I make it.
- **Guardrails:** no destructive git ops; never skip hooks; keep the shared working tree
  safe (`git checkout --` not stash/reset); commit as configured with the Co-Authored-By
  line; surface any design ambiguity that isn't resolvable from these RFCs rather than
  guessing on a security-sensitive point.

## Status of the artifacts

Updated 2026-06-25. The identity track is built; the capability-model substrate is
partial. Per phase:

| Phase | RFC / artifact | State | Evidence |
|---|---|---|---|
| 1 | RFC-0011 refinement | **partial** | Net tier shipped: typed `NetPolicy` + `confine.tcp/any_port/cidr/cidr_any/union`, `net.only`/`net.deny`. **Not** built: `Dir`/`File` policy methods, the RFC-0002 carried-state *record* (so library caps like `Postgres.table`), and retiring `restrict`/`subdir`. `restrict` survives as the `--net` string form. |
| 2 | RFC-0012 File capability | **not started** | `status: proposed`. `Exec` not yet folded into `File[Exec]`. |
| 3 | RFC-0009 HTTPS/TLS + RS256 + base64url | **shipped** | rustls 0.23 + aws-lc-rs (not s2n-tls); `tls:` connect-time scheme; `std/crypto` RS256; `std/encoding` base64url. `status: implemented`. |
| 4 | OIDC verification (JWT/JWKS) + dev IdP | **shipped** | `std/jwt` (`verify_rs256`/`verify_oidc`/JWKS) + `std/oauth`; dev IdP in `src/idp.rs`. |
| 5 | Social login + OIDC publishing | **shipped** | GitHub + Google login in `projects/coven-web` (RFC-0010 `implemented`); OIDC trusted publishing in `projects/coven` (`coven_trust.witchy` `verify_token` + `coven.witchy` `authorize_publish`/`trusted-publisher` attestation), with differential coverage in `src/example_tests.rs` and `projects/coven/src/coven_test.witchy`. |
| 6 | RFC-0013 grant documents | **not started** | `status: proposed`. |

Related, not numbered above: `rfcs/coven-namespaces-plan.md` (the publish-identity
model — its §1-4 derive-equality / immutable-id / decoupled-promote gate is the design
realized in coven), `rfcs/identity-stack-implementation-plan.md` (the WS-1…WS-9 detail
for Phases 3-5, now mostly executed), and `rfcs/0014-remove-capability-firewall.md`
(`proposed`, independent cleanup — retire `retain`/`without`).

**The remaining tail** is the deep capability-model work: RFC-0011's carried-state
record + `Dir`/`File` refinement methods, RFC-0012 (File), and RFC-0013 (grants).
These were deliberately deferred (the Phase-1 fast-track decision) so the visible
identity goal could ship on the existing `restrict` string first.
