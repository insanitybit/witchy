---
rfc: 0117
title: "Hosted coven registry, milestone 2: usable by strangers, publishable by CI"
status: accepted
created: 2026-08-08
tracking: "Accepted 2026-08-08 after the 2026-08-08 four-lane registry audit (scratch/audit-2026-08-08-registry/). Sequenced program: Lane A onboarding + CI publish, Lane B console authority, Lane C concurrency + edge hardening. Domain-gated work (RFC-0099 passkey enrollment, WebAuthn RP, per-namespace delegated TUF keys) stays deferred."
---

# RFC-0117: Hosted coven registry, milestone 2

## Summary

RFC-0116 made the registry deployable and deployed it (`https://witchy.fly.dev`).
A four-lane read-only audit (2026-08-08) found the protocol core sound and
CI-tested, but the service around it unusable by anyone who is not the operator:
the instance is availability-starved, undiscoverable, its one package fails a
newcomer's first command, CI cannot publish, and the console cannot drive human
promotion against our own registry. M2 finishes that service layer in three
sequenced lanes. It adds no new trust primitives — it wires up and hardens what
exists.

Evidence for every claim below: `scratch/audit-2026-08-08-registry/` (ops,
trust-pipeline, identity-auth, client-journey lane reports + SYNTHESIS.md).

## Non-goals (deferred, domain-gated)

Passkey enrollment (RFC-0099), the WebAuthn relying party, per-namespace
delegated TUF keys, and a real root-key ceremony. These need a stable purchased
domain (RP IDs and registry origins must not churn) and are out of M2 by
decision (2026-08-08).

## Lane A — onboarding + CI publish (a stranger installs; CI publishes)

Ordered so each step is independently landable.

1. **Availability (operator + ops).** A Fly payment method (removes the
   trial force-stop). Then real volume snapshots, a rehearsed restore drill
   corrected to re-seed the signing key, and an offline copy of the signing
   seed. *Not code — tracked here so the lane's acceptance can reference it.*
2. **Discoverability.** Document `https://witchy.fly.dev` and a copy-paste
   `COVEN_URL` in README + book/src/packages*.md. Fix the doc/CLI discrepancies
   the client-journey lane catalogued (spec/local-registry.md port/args, the
   `build`-resolves-registry-deps claim, `[host:port]` usage strings).
3. **`pm add` writes `[dependencies]`.** Today add vendors + locks but never
   records the dependency in the manifest, so `tree`/`why`/`outdated` are blind
   to registry deps and the "see your tree's authority" story fails on first
   contact (pm.witchy ~2540-2617). This is the highest-value code fix in M2.
   Differential/e2e: after `add`, `tree` and `why` must show the dep.
4. **A first package that installs cleanly.** The only published rune sits in
   its 72h cooldown, so every newcomer's first `add` fails and their first
   learned action is `--allow-fresh`. Publish a starter package past its
   cooldown (or seed one) and reference it in the docs. Also: the cooldown
   message should state remaining time.
5. **Failure-UX truthfulness.** A DNS/resolve failure currently prints
   ``Fetch origin is not granted`` (a capability lie; fetch.rs ~264-267 maps
   resolve errors to `Denied`). Distinguish "not granted" from "could not
   resolve/connect". Small, high-trust-impact.
6. **CI publish reachable.** (a) Configure the live registry to trust
   `token.actions.githubusercontent.com` via `--trust-issuer-oidc`. (b) Add
   JWKS **refetch-on-unknown-kid** so a GitHub key rotation cannot brick
   publishing (today JWKS is fetched once at startup). (c) Author and PROVE a
   GitHub Actions publish workflow by dogfooding it — publish a witchy rune
   from the witchy repo's own CI, binding the namespace to the repo org.

Lane A acceptance: a stranger who read only the public docs installs the starter
package with a single documented command, and a `git push` triggers a CI
workflow that publishes a new version the stranger can then resolve.

## Lane B — console authority (a logged-in human can promote)

1. **Fix coven-web id_token forwarding.** The console forwards no id_token, but
   an issuer-configured registry requires one for promote/yank, so console 2FA
   works only against an anonymous registry (coven_web.witchy ~284 vs
   coven.witchy ~522-529). Forward the session's id_token to the registry.
2. **Enable login on the hosted console.** Create a GitHub OAuth app for
   `witchy.fly.dev` (operator: callback URL) and wire the client id/secret as
   coven-web secrets. Login itself is already implemented + CI-proven (RFC-0010).
3. **Deploy the coven-web UI** on the hosted origin (the RFC-0116 track-3
   deploy already contemplated this; the stalled deploy attempt is superseded
   by this lane). `/coven/*` API passthrough stays byte-identical for pm.
4. **Put the console 2FA assertion path under CI.** Today it is proven only by
   `verify.py`, which CI does not run.

Lane B acceptance: a human logs in to `https://witchy.fly.dev` with GitHub and
promotes a staged version they are authorized for, with the registry verifying
a real second factor — no hand-minted local-IdP token.

## Lane C — concurrency + edge hardening (safe for concurrent strangers)

1. **Land SEC-048** (the queued branch): `pm update` must re-gate the transitive
   closure for capability widening, not just the updated rune.
2. **First-promoter hijack (SEC-049 MED-3, source-only).** Any MFA identity can
   release a victim's staged version and seize maintainership; require the
   promoter to already be an authorized maintainer/publisher of the namespace.
3. **Atomic Dir intrinsics (SEC-049 HIGH).** The store's only shared state is a
   Dir with no exclusive-create/rename/lock, so concurrent requests can
   double-spend a jti (token replay) or brick a content-addressed version. This
   needs new atomic filesystem intrinsics on the Dir capability — parity-
   sensitive, both backends, its own detailed RFC (this lane scopes it and
   spawns that RFC; it does not hand-wave the fix).
4. **Edge + code DoS mitigation.** A Fly-edge concurrency/rate cap now; a read
   timeout + request-body-size cap in the host net/server layer as the durable
   code fix (no read timeout today ⇒ slowloris wedges a worker).

Lane C acceptance: a documented concurrency stress (parallel publishes of the
same version, parallel token reuse) cannot double-spend a token or corrupt a
version, and a slow-client flood cannot wedge the service.

## Sequencing and reporting

Lanes run in order (A → B → C); within a lane, independent steps land in
parallel through the merge queue. Each lane's generated evidence (docs, e2e,
the live deploy) updates with the change that invalidates it. Progress is
reported as critical-path + unmet acceptance criteria, not commit counts. The
atomic-Dir-intrinsics RFC is the one piece expected to outlast M2's cadence; it
is scoped in Lane C and executed on its own track.
