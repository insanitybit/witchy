---
rfc: 0121
title: Sealed secrets as a capability right - `Secret[Seal]` replacing the `use-only` bit
status: implemented
created: 2026-08-09
tracking: "renames use-only -> sealed; moves the reveal refusal from runtime to check time"
---

# RFC-0121: Sealed secrets as a capability right

`Secret` is the only host capability whose policy lives outside the rights system.
Every other capability spells its permitted verbs in its type - `Dir[Read]`,
`Console[Write]`, `Net[Connect, Tcp]` - and the checker rejects a verb the rights
do not permit, at compile time, with a diagnostic naming the missing right. A
`Secret`'s reveal policy instead rides an out-of-band boolean (the `use-only`
grant bit), is invisible in every signature, and is enforced by a **runtime**
refusal.

This RFC makes sealed-ness a right: `Secret[Seal]` versus `Secret[Reveal]`, with
the same monotone narrowing, the same footprint reporting, and the same
check-time enforcement as `Dir[Read]`. It also renames the concept from
`use-only` to `sealed`.

## Motivation

### 1. `use-only` names the prohibition, not the state

"use-only" describes what the runtime will refuse, from the runtime's point of
view. Every explanation of it has to be followed by a gloss ("usable by handle but
never readable"). `sealed` names the *state of the secret*, is one word, and is
already this project's vocabulary: `sealed type`, sealed `capability`
constructors, "sealed to the framework" in the Glamour tokens. A grant row reading
`sealed = true` and an error reading "this secret is sealed" need no gloss.

### 2. The policy is invisible in signatures

Today these two functions are indistinguishable:

```witchy
fn sign_release(key: Secret, tag: String) -> String     // needs only by-handle use
fn call_api(key: Secret) -> String                      // reads the bytes
```

Both say `Secret`. A reviewer cannot tell from the signature which one can
exfiltrate the key, and the capability model's central promise is that
*"a function's complete authority is visible in its signature"*
([capabilities.md](../spec/capabilities.md)). `Secret` is the one type where that
promise does not hold. With rights:

```witchy
fn sign_release(key: Secret[Seal], tag: String) -> String   // provably cannot reveal
fn call_api(key: Secret[Reveal]) -> String                  // reads the bytes; says so
```

### 3. The refusal is a runtime error where it could be a type error

`crypto.reveal` on a sealed secret currently traps at run time. Under rights it
does not compile: `reveal` requires `Reveal`, and the diagnostic is the existing
shape:

```text
`reveal` needs `Reveal` but the capability is `Secret[Seal]`
```

That is a strict improvement. A program that would have died in production on a
grant it never tested fails in the checker instead. RFC-0060's runtime guard stays
as defense in depth for the compiled/precompiled artifact path, but it stops being
the only line.

### 4. Consistency is the point

This is not a new mechanism; it is deleting a special case. `Secret` joins
`Dir`/`Console`/`Net` in the model that already exists, and inherits implicit
narrowing, `as` ascription, `witchy caps` reporting, and `caps-diff` widening
detection without new machinery. The alternative considered and rejected below,
two distinct types, would make `Secret` the only capability shaped that way.

## Design

### The rights

```text
Secret[Reveal]   the bytes may be read into guest memory (crypto.reveal)
Secret[Seal]     by-handle use only; reveal is a check-time error
```

A bare `Secret` in a signature means the full right set, exactly as a bare `Dir`
does today - so existing code keeps working unchanged.

Narrowing is monotone and one-directional: `Secret[Reveal]` flows into a
`Secret[Seal]` parameter (more authority stands in for less), never the reverse.
Both the implicit call-boundary form and the explicit `key as Secret[Seal]`
ascription work, per §"Narrowing patterns" of the capability reference.

### Verb-to-right mapping

| Operation | Requires |
|---|---|
| `crypto.reveal` | `Reveal` |
| `crypto.sign`, `crypto.public_key` | `Seal` (satisfied by either right) |
| `jwt.sign_eddsa` | `Seal` |
| `server.serve_tls`, `serve_tls_n` (key param) | `Seal` |

`Seal` is the weaker right and every by-handle op needs only it, so a narrowed
handle can still do all the useful work - which is what makes the narrowing
worth performing.

### The grant floor

The launch grant remains the floor; narrowing can only shrink from it.

```sh
--secret name=value              # Secret[Reveal, Seal]  (unchanged default)
--secret name=value,sealed       # Secret[Seal]
--signing-key <path>             # Secret[Seal]          (already non-revealable)
```

```toml
[secrets]
tlskey = { from = "env:MY_TLS_KEY", sealed = true }
```

A grant that confers only `Seal` cannot be widened to `Reveal` by any in-language
operation, so the host's decision is final - the same relationship
`--dir`/`Dir[Read]` already has.

### Implementation sketch

The pieces mirror `Dir` exactly:

- `crates/witchy-cap-model/src/lib.rs`: add `CapabilityRight::{Reveal, Seal}` and
  a `SECRET_RIGHTS` const beside `READ_WRITE_RIGHTS`/`NET_RIGHTS`; register them
  on `CapabilityKind::Secret` in `right()`.
- `crates/witchy-types/src/typeck/capability_calls.rs`: a `check_secret_op`
  following `check_dir_op`'s shape - resolve the receiver to `Ty::Secret(rights)`,
  reject the verb when the right is absent, reusing the
  "`{verb}` needs `{Right}` but the capability is `{rights}`" wording so the
  diagnostic family stays uniform.
- `Ty::Secret` gains a rights payload; `SecretStore.get`/`require` return the
  store's granted rights.
- The runtime keeps its refusal (defense in depth, and required for precompiled
  `.wasm` where no checker ran). `USE_ONLY_SECRET_REVEAL_ERROR` is renamed and its
  text updated to say "sealed".

### Rename scope

`use-only` -> `sealed` across: the CLI grant modifier, the grant-document field,
`USE_ONLY_SECRET_REVEAL_ERROR` and its message, `SecretGrant.use_only`, the
`(use-only)`/`(revealable)` approval markers from RFC-0060/BUG-610, and the prose
in `std/secretstore.witchy`, `std/crypto.witchy`, `std/server.witchy`,
`spec/capabilities.md`, and `spec/stdlib.md` (regenerated). Per this project's
"break, don't deprecate" rule this is a single cut with no alias for the old
spelling.

## What this does NOT do

**`Secret` is closed to extension, and no right changes that.** The set of
by-handle operations is fixed by the host: Ed25519 sign/public-key, EdDSA JWT
signing, and TLS serving. There is no `Secret` -> `Bytes` path and no
`Secret` -> `Secret` derivation. So a hand-rolled construction - `hmac_sha512`,
Argon2id, AES-GCM, any KDF the stdlib lacks - needs the raw bytes and therefore
needs `Reveal`. Verified: a use-only key cannot be hand-rolled against today, and
this RFC does not change that.

Two consequences worth stating plainly:

1. **This RFC does not make sealed-by-default viable.** A default of `Seal` would
   break every program doing custom crypto or sending a credential to an external
   sink, and no finite amount of by-handle plumbing closes an open-ended space of
   user-defined constructions. The default stays `Reveal`.
2. **There are two tiers, and the docs should say so.** A *host-custody* tier
   (fixed menu, bytes never enter the program, `Seal` is a real guarantee) and a
   *guest* tier (everything else; `reveal` is the correct, unapologetic
   operation). `Seal` is precise about which tier a given handle is in - that is
   its value, rather than pretending the guest tier can be eliminated.

A host `Secret` -> `Secret` derivation primitive (HKDF-Expand) would genuinely
extend tier 1: derive a scoped subkey by handle, then reveal *that* to a
hand-rolled construction instead of the root secret. That is a separate RFC.

## Alternatives

### Two distinct types plus a `Secret` trait

`SealedSecret` and `UnsealedSecret` as separate capability types, unified by a
`Secret` trait for their common surface. Verified expressible today: a trait can
be implemented for a capability type (checks and runs), two types unify under
`impl Trait`, and naming the concrete type restricts statically
(`expected SealedSecret, found UnsealedSecret`).

Rejected for three reasons:

- **Inconsistent.** No other capability splits into two types for a policy
  variant. `File` is a separate type because it is the *leaf of a hierarchy*
  (RFC-0012), not a policy on `Dir`. Rights are how witchy expresses "same
  resource, fewer permitted verbs".
- **The trait would not restrict.** A bound (`s: impl Secretish`) unifies; it
  accepts both kinds. Restriction still requires naming a concrete type, so the
  trait only serves the genuinely-common surface - which is thin (roughly
  `public_key`) once `reveal` is excluded by definition.
- **Costs the free machinery.** Narrowing, `as` ascription, footprint summing, and
  `caps-diff` widening all key off rights today and would need per-type
  reimplementation.

### Keep the runtime-only bit, improve only the docs

This is the status quo after RFC-0060/BUG-610: enforced correctly, visible at the
approval prompt, gated by `grants-diff`. Rejected because it leaves the
signature-level invisibility (motivation 2) and the runtime-only refusal
(motivation 3) in place - the two things a reviewer and the checker respectively
cannot work around.

### A `SecretStore` name-scoped refinement

Narrowing *which secrets* a store reaches (`secrets.only(Secrets.named("api_key")))`,
as a second dimension beside the verb. Deferred, not rejected: it addresses
store-level amplification (a helper holding a `SecretStore` reaches every granted
secret with every verb) rather than the reveal policy, and closure attenuation
already covers the common case. Worth its own RFC if multi-tenant secret handling
becomes real.

## Verification

Implemented and pinned by:

- `example_tests::crypto::sealed_secret_signs_identically_on_both_backends` -
  narrowing implicitly at a call and via `as`, and `sign`/`public_key` through a
  sealed handle producing byte-identical output on interpreter and compiled WASM.
  The right is erased before either backend runs, so narrowing changes what the
  checker permits, never what the program computes.
- `example_tests::crypto::revealing_a_sealed_secret_is_a_type_error` - `reveal` on
  a `Secret[Seal]` is a check-time error naming the missing right; `as` cannot
  re-widen a sealed handle (the grant floor holds); a bare `Secret` still reveals.
- `example_tests::crypto::an_unknown_secret_right_is_rejected` - `Secret[Sealed]`
  (a plausible typo) is rejected rather than silently normalized to full authority,
  extending the BUG-154 guarantee to `Secret`.
- `tests/e2e/sandbox_grants.rs::serve_tls_accepts_a_sealed_key` - a real TLS 1.3
  handshake against `serve_tls_n` with a sealed key, proving the positive half.
- `witchy caps` reports the narrowed rights (`fingerprint  Secret[Seal]`), and the
  RFC-0060/BUG-610 reviewable surface keeps working under the new spelling.
- The runtime refusal (`SEALED_SECRET_REVEAL_ERROR`) still fires, as defense in
  depth for a precompiled `.wasm` that no checker ever saw.

One thing the implementation added beyond the sketch: the four sites that
special-cased which capabilities take bracket *rights* rather than type arguments
(name resolution, formatting, marker validation, `meta.type_capability`) each held
a hardcoded `"Console" | "Dir" | "File" | "Net"` list. They now ask
`witchy_cap_model::bears_rights_markers`, so a capability gaining a rights
vocabulary needs no edit at those sites - `Secret` was the first to exercise that
path, and it had to be fixed for `Secret[Seal]` to resolve at all.

## Migration

Bare `Secret` keeps meaning full rights, so existing programs compile unchanged.
The breaking changes are the `use-only` -> `sealed` spelling in grants (CLI and
document) and the renamed error constant. Both are single-cut renames with test
coverage already in place from BUG-610.
