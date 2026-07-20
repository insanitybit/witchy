---
rfc: 0066
title: Verified passkey 2FA — the registry checks a real WebAuthn assertion, not a marker
status: implemented
created: 2026-07-05
predecessors:
  - "0010 (web console social login), 0038 (grantable capabilities), coven promotion identity"
  - "bugs/BUG-219, BUG-412, BUG-360 (the marker-not-assertion gaps this RFC closes)"
tracking: "operation-bound WebAuthn promote/yank assertions implemented and exercised by projects/coven-web/verify.py; registration/bootstrap hardening moved to deferred RFC-0099 and privileged Glamour slot authority moved to deferred RFC-0100"
---

# RFC-0066: Verified passkey 2FA — the registry checks a real WebAuthn assertion, not a marker

> **2026-07-19 terminal disposition:** the title promise is implemented:
> coven-web verifies fresh, operation-bound WebAuthn assertions for promote and
> yank, rejects replay/cross-operation/tampered assertions, and forwards a
> verified factor to Coven; trusted publishing derives MFA from authenticated
> issuer claims rather than a client marker. The original draft bundled two
> independent boundaries that are not prerequisites for that delivered flow.
> Registration ceremony, bootstrap authorization, and signature counters are
> now deferred [RFC-0099](0099-webauthn-registration-bootstrap.md).
> Authority-bearing Glamour renderers are deferred
> [RFC-0100](0100-authorized-glamour-host-slots.md). Definition-of-done rows 2 and 3 below are preserved as historical
> proposal text, not claimed implementation.

## Summary

Coven's promote step is the human, second-factor, separation-of-duties gate on
a release: a staged version becomes RELEASED only when a human with a passkey
approves it. Today that gate is a **string marker** — `do_promote` accepts any
non-empty `second_factor` and the PM hard-codes `"webauthn"` — so the "second
factor" is asserted, not verified. This RFC makes the registry verify an actual
WebAuthn assertion, bound to the specific operation, before it will release;
makes credential registration verify the create-ceremony before trusting a
public key; and gives glamour's secret/port slots an authority token instead of
raw host capability. The verifier already exists ([`std/webauthn.verify_assertion`](../std/webauthn.witchy));
the gap is that the trust boundary doesn't call it.

## Motivation

The security pitch is "a registry where releasing a version requires a real
human second factor, separate from the CI identity that published it." That is
only true if the registry checks the factor. Three holes make it a promise the
code doesn't keep:

- **BUG-219**: `coven do_promote` checks `!is_empty(req.second_factor)` and signs
  the marker into the release record; [`pm.witchy`](../projects/coven/src/coven.witchy) sends the literal `"webauthn"`.
  Nothing verifies a signature over a challenge — a direct API caller releases
  by sending any non-empty string. The separation-of-duties gate is decorative.
- **BUG-412**: registration (`h_wa_register`) stores the submitted `publicKey`
  after only a non-empty check — no create-ceremony / attestation validation —
  so an attacker-chosen key can be enrolled and then used to "verify" later.
- **BUG-360**: glamour's `slot(kind, data)` dispatches to a host renderer with no
  authority token; the host trusts the guest-named slot kind, so a compartment
  can address a slot it was never granted.

coven-web's own handlers were already hardened to bind the assertion to the
operation and consume challenges single-use (BUG-278/365/372/421/424). This RFC
closes the *registry-core* and *registration* and *glamour-host* halves so the
guarantee holds no matter which client speaks to coven.

## Design

Three changes, all "verify at the boundary that already has the data":

1. **Promote verifies an operation-bound assertion (core).** `do_promote` (and
   the trusted-publish/yank equivalents that claim a second factor) takes the
   full WebAuthn assertion (`auth_data`, `client_data_json`, `signature`, the
   `credentialId`) and calls `webauthn.verify_assertion` against the stored
   public key for the promoting identity, with:
   - `expected_challenge` = a server-minted, single-use challenge that the server
     issued for *this* `(name, version, op=promote)` — the challenge record binds
     the operation (the coven-web change already stores op+params with the
     challenge; the core now enforces it too, so a raw API caller can't skip it).
   - `expected_origin` / `expected_rp_id` from the registry's configured origin.
   - `require_uv = true` (user verification, i.e. a real human gesture).
   A failed or absent assertion is a hard `403`; only a verified assertion signs
   the release. The stored release record records the *verified credential id*,
   not a free-text marker.

2. **Registration verifies the create-ceremony (registration).** `h_wa_register`
   validates the attestation object structure and that the credential's public
   key is well-formed and self-consistent with the attested `authData` (rpIdHash,
   flags, the same origin binding) before persisting it. A malformed or
   origin-mismatched registration is refused. (Full attestation-statement
   verification against a metadata service is out of scope for 0.x — self-
   attestation with structural + origin checks is the bar, stated honestly.)
   This composes with BUG-261 (verify the signature counter is non-decreasing
   across assertions) so a cloned authenticator is detectable.

3. **glamour slots carry an authority token (host).** `slot(kind, data)` gains a
   capability token minted from the grant the compartment actually holds; the
   host `mountSlot` dispatches only for a token that authorizes that slot kind.
   A guest cannot address a slot it wasn't granted — the same sealed-authority
   discipline capabilities already use (this is the glamour analogue of RFC-0002
   sealing; see also RFC-0065).

## Definition of done

1. `do_promote` (+ yank/trusted-publish second-factor paths) reject a promote
   whose assertion doesn't verify against the enrolled credential for the
   operation's challenge; an e2e test drives a real assertion through and a
   forged/absent one is refused (BUG-219). `pm.witchy` sends the assertion, not
   the `"webauthn"` literal.
2. `h_wa_register` refuses a structurally-invalid / origin-mismatched credential
   and a regression test proves an attacker-chosen bogus key can't enroll
   (BUG-412); signature counter is checked (BUG-261).
3. glamour `slot` requires an authority token; a compartment addressing an
   ungranted slot is refused, with a test (BUG-360).
4. The whole flow works on both backends (the crypto is `std/crypto` +
   `std/webauthn`, already parity-tested) and the coven e2e suite stays green.

## Alternatives

- **Trust the client (today)**: the marker approach — rejected, it's the bug.
- **Full FIDO metadata attestation**: correct long-term, but heavy and not a 0.x
  bar; self-attestation + origin/counter checks close the practical hole now and
  the metadata path can be a later RFC.
- **Password/TOTP second factor instead of passkeys**: weaker (phishable,
  shared-secret) and off-brand for a capability-secure system; passkeys are the
  right primitive and the verifier already exists.

## Drawbacks

- The promote API grows a real WebAuthn payload (challenge round-trip), so the
  CLI `promote` flow must fetch a challenge, prompt the authenticator, and submit
  the assertion — more moving parts than a flag. That complexity is the point: a
  second factor that's easy to fake isn't one.

## Prior art

- WebAuthn/FIDO2 assertion + attestation ceremonies; the repo's own
  `std/webauthn.verify_assertion` and coven-web's operation-bound challenge
  handling (BUG-365). RFC-0002/0065 sealing (the glamour-slot authority model),
  the coven promotion-identity model (human 2FA + separation of duties).
