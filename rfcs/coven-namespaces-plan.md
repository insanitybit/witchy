---
status: planned
note: Design accepted, not yet built. Implemented in witchy (the self-hosted coven), not Rust.
---

# Coven Namespaces — Provider-Derived Identity, written in witchy

Status: design + implementation plan. Not yet built. Supersedes the author-chosen
`ns/name` + per-namespace TOFU model in `rfcs/package-manager.md` §8 for the
*naming and trusted-publishing* surface; the rest of that spec (footprint gate,
two-phase publish, TUF, content addressing) is unchanged.

## 0. The point: dogfood witchy on a security-critical system

coven is already self-hosted: the Rust package manager is gone (`src/pm` deleted,
RFC-0004), and the registry is witchy (`projects/coven/*.witchy`). This RFC keeps
it that way *on purpose*. The headline goal is the feature — provider-derived
namespaces — but the **project** goal is to prove witchy can host a real,
security-critical OIDC trusted-publishing identity layer **in witchy itself**.

The whole identity pipeline — JWKS discovery, JWT parsing, claim mapping,
namespace derivation, and the publish/promote authorization — is ordinary witchy
over the existing stdlib (`Net` for discovery, `json`/`string`/`encoding`, the
content store). The **only** thing the language cannot express is the elliptic /
modular signature math, so exactly one new native intrinsic
(`crypto.rsa_pkcs1_sha256_verify`, for RS256) joins `sha256`, `ed25519_verify`,
and `ecdsa_p256_verify` at `std/crypto`'s existing native seam. ES256 is already
there.

That split is the dogfood thesis, stated concretely:

- **TCB stays tiny and fixed.** The trusted base is the compiler plus a handful of
  crypto intrinsics. Adding real OIDC adds *one* intrinsic — not a 6k-line Rust
  subsystem. Everything policy-shaped (who may publish/promote where) is auditable
  witchy a reader can follow.
- **The identity layer has a capability footprint, like any rune.** Its authority
  is `Net` (discovery) + the content store — no ambient power. coven-web can
  *display* that footprint: the registry's own identity layer is footprint-audited
  the same way it audits everything it serves.
- **If witchy can host this, it can host the registry's whole domain.** Supply-chain
  identity is the hardest, most security-sensitive thing coven does. Doing it in
  witchy is the strongest possible evidence the self-hosting is real, not a demo.

The rest of this document is the design; read it as "what the witchy implementation
does," with the native seam called out wherever it appears (§4.2, §8).

## 1. Feature goal

Give coven namespaces that are **derived from a verified OIDC identity** rather
than hand-chosen and first-come claimed. The namespace *is* the publisher's
provider-qualified identity, so:

- A publisher can only ever publish under their own derived prefix — grabbing
  `google/...` is structurally impossible, with **no DNS, no reserved-name
  list, and no manual verification ceremony**.
- Coven supports **any OIDC provider** by configuration, not just GitHub — the
  provider is a declarative data row, not hardcoded code (crates.io's
  limitation is that its trusted-publishing adapter is GitHub-specific code).

The blast radius of any residual naming weakness is already capped by the
capability footprint gate, which is why coven can run a far lighter identity
layer than npm/PyPI.

## 2. The model

### Name shape

Publishable runes are named in **three structural segments**:

```
<provider>/<owner>/<rune>      e.g.  github/insanitybit/http
```

- **namespace** = `<provider>/<owner>` (`github/insanitybit`) — derived from the
  publish token, never hand-chosen.
- **rune / module name** = the last segment (`http`). The in-language `import http`
  is unchanged (the module name is the last `/`-segment).
- Local, never-published apps keep a single bare segment (`name = "app"`). The
  three-segment rule applies only to publishable runes.

### Identity binding

Each namespace is keyed on two claims from the verified token:

- `owner` (human-readable, e.g. GitHub `repository_owner`) → the displayed
  namespace segment.
- `owner_id` (**immutable**, e.g. GitHub `repository_owner_id`) → the canonical
  key the registry pins the namespace to.

Pinning on the immutable id (not the mutable login string) is what defeats
account rename and reclaim/repo-jacking — see §5.

## 3. Locked design decisions

1. **Derive, don't register.** Namespace = verified OIDC identity. No
   registration step, no TOFU on an author-chosen string.
2. **Provider-qualified.** The provider label prefixes the namespace so
   different providers' accounts never collide or get conflated
   (`github/acme` ≠ `gitlab/acme`).
3. **Pin to the immutable id.** Canonical key is `owner_id`; the login string is
   a refreshable display label.
4. **Declarative provider adapters.** A provider is one config row
   `{issuer, provider label, owner_claim, owner_id_claim, id_stable}`. Adding a
   provider is operator configuration, not a coven release.
5. **Real OIDC.** Replace the current Ed25519/JWT stand-in with standard OIDC:
   JWKS discovery + RS256/ES256 verification + audience binding. This is
   provider-uniform, written once.
6. **Operator-controlled issuer→label mapping.** Only the genuine issuer maps to
   a given provider label; this is the `google/`-grab defense lifted to the
   provider level (§5).
7. **Defer rename/transfer/alias.** A renamed account publishes under a new
   namespace; the old one freezes (existing locks still resolve). An alias
   record is a later addition.
8. **Promote identity is decoupled from publish identity.** Promotion requires only
   separation of duties + a human 2FA challenge, via *any* supported login method;
   the promoter need not match the publish namespace or provider (§4.5).
9. **Implemented in witchy; the TCB does not grow except by one crypto intrinsic.**
   The identity layer lives in `projects/coven/`. The only native addition is
   `crypto.rsa_pkcs1_sha256_verify` (§0, §8). No policy logic moves to Rust.

## 4. Architecture (in witchy)

Everything below is `projects/coven/*.witchy` unless it names the native seam. The
modules that already exist and grow into this design:

- `coven_trust.witchy` — today verifies an Ed25519-signed claims envelope
  (`verify_token`) and derives `namespace_of`/`claim`/`attestation`. It becomes the
  OIDC verifier + claim mapper.
- `coven_validate.witchy` — `valid_name` (today: two-segment) → three-segment.
- `coven.witchy` — `authorized_publisher` / `authorized_promoter` are the
  authorization call sites.

### 4.1 Declarative provider table (`issuers.json`)

Today the `Issuer` record is `{id, pubkey}` (a pinned Ed25519 key). It becomes a
config row whose *keys come from discovery*, not a pinned key:

```jsonc
"https://token.actions.githubusercontent.com": {
  "provider":       "github",
  "owner_claim":    "repository_owner",
  "owner_id_claim": "repository_owner_id",
  "id_stable":      true,
  "audience":       "coven-registry"
}
```

| provider | owner_claim | owner_id_claim | id_stable |
|---|---|---|---|
| `github`   | `repository_owner` | `repository_owner_id` | true |
| `gitlab`   | `namespace_path`   | `namespace_id`        | true |
| `sigstore` | `email`            | `sub`                 | false (email reassignable) |

The provider label is operator-assigned per issuer. Self-hosted instances get
their own host-qualified label (e.g. `gitlab.acme-corp.com`) so they can never
mint another provider's namespace.

### 4.2 OIDC verification, in witchy (the one native seam)

Discovery, parsing, and claim checks are plain witchy; only the signature math is
native.

```witchy
// (1) Discovery: fetch <issuer>/.well-known/openid-configuration -> jwks_uri,
//     then the JWKS — over HTTPS, so this depends on RFC-0009 (a plain
//     `Net[Connect, Tcp]` whose allowlist carries a scheme-agnostic `host:port`
//     entry; HTTPS is a connect-time `tls:` choice on the dialed address, not a
//     new right and not an allowlist scheme); cache by issuer. This is the layer's
//     only ambient authority — a `Net[Connect, Tcp]`, visible in coven's footprint.
fn discover_jwks(net: Net[Connect, Tcp], issuer: String) -> Result(Json, String)

// (2) Verify a compact JWT: split header.payload.sig, pick the JWK by `kid`, then
//     verify the signature over "header.payload". RS256 -> the new intrinsic;
//     ES256 -> the existing crypto.ecdsa_p256_verify. Both reachable, neither
//     expressible in witchy — exactly the std/crypto native-seam rule.
fn verify_jwt(jwks: Json, token: String, audience: String, now: Int) -> Result(Json, String):
    let (signed, sig, alg, kid) = split_compact(token)?
    let key = jwk_by_kid(jwks, kid).ok_or("no JWKS key for kid")?
    let ok = match alg:
        "RS256" -> crypto.rsa_pkcs1_sha256_verify(jwk_rsa_pubkey(key), signed, sig)
        "ES256" -> crypto.ecdsa_p256_verify(jwk_ec_pubkey(key), signed, sig)
        _       -> false
    require(ok, "JWT signature invalid (untrusted or forged)")?
    let claims = json.decode(base64url_decode(payload_of(token)))?
    // iss trusted, aud matches the row, exp/iat valid:
    check_standard_claims(claims, audience, now)
```

`crypto.rsa_pkcs1_sha256_verify(public_key, message, signature) -> Bool` is the
**single new native intrinsic** — a placeholder body in `std/crypto.witchy`, an
interpreter intercept, and a WASM host import, exactly like `ed25519_verify` and
`ecdsa_p256_verify`. Nothing else in this RFC is non-witchy.

### 4.3 `derive_namespace`

```witchy
type Ns:
    provider: String
    owner: String
    owner_id: String
    id_stable: Bool

fn derive_namespace(cfg: IssuerCfg, claims: Json) -> Result(Ns, String):
    Ok(Ns(cfg.provider,
          coven_trust.claim(claims, cfg.owner_claim),
          coven_trust.claim(claims, cfg.owner_id_claim),
          cfg.id_stable))
// namespace string = provider + "/" + owner
```

### 4.4 `authorize_publish` (collapses the TOFU policy)

This is `coven.authorized_publisher` once the bound-policy struct is gone:

```witchy
fn authorize_publish(self, name: String, claims: Json) -> Result(Bool, CovenError):
    let cfg  = self.issuer_cfg(coven_json.str(claims, "iss"))?  // untrusted issuer -> 401
    let want = derive_namespace(cfg, claims)?
    let got  = coven_trust.namespace_of(name)                  // "provider/owner"

    // (1) may ONLY publish under the namespace the token proves
    require(got == want.provider + "/" + want.owner, 403,
            "token authorizes `" + want.provider + "/" + want.owner + "`, not `" + got + "`")?
    // (2) immutable-id consistency; first publish records the id
    match self.namespace_owner_id(got):
        Some(id) -> require(id == want.owner_id, 403, "`" + got + "` is bound to a different account id")
        None     -> Ok(self.bind_namespace_id(got, want.owner_id))
```

The per-namespace store shrinks to one line per namespace:

```jsonc
// namespaces.json   "provider/owner" -> owner_id
"github/insanitybit": "1234567"
```

### 4.5 `authorize_promote`

Promotion is the **human release gate**, and its identity is deliberately
decoupled from publish identity. It requires two things, and only these:

- **Separation of duties** — the promoter is a different actor than whatever
  published the staged version (the CI subject `repo:…/workflow` that publishes is
  never the human subject that promotes). coven already enforces this:
  `promote_checked` refuses when `promoter == rec.uploaded_by`.
- **A human 2FA challenge** — a human-presence proof gates the release (a
  passkey/WebAuthn assertion, or an interactive OIDC login plus a second factor).

The promoter authenticates by **any supported login method** and need **not**
derive to the staged version's `provider/owner`, nor even use the same provider as
the publish token. Publishing `github/alice/http` from GitHub Actions OIDC and then
promoting it after a Google login + 2FA is a first-class flow: the publish identity
binds the namespace, while the promote identity only has to prove that a distinct
human vouched for the release. Who may promote a given namespace is governed by its
recorded maintainers (maintainer-TOFU), independent of the promoter's own
namespace. (coven-web already realizes this gate: a passkey sign-in plus the
`Promote with passkey (2FA)` console, on top of the SoD check.)

### 4.6 Name validation (`coven_validate.valid_name`)

For publishable runes: exactly three `/`-segments; each segment matches
`[a-z0-9._-]+`; `provider` is a known label; no `..` traversal. Keep the
single-bare-segment escape hatch for local apps. `coven_trust.namespace_of` returns
the first **two** segments (`provider/owner`) rather than the first one.

## 5. Security properties

- **`google/` grab → impossible.** Check (1): you cannot publish `github/google/x`
  unless your token's `repository_owner == google`, which only GitHub's `google`
  org can mint.
- **Reclaim / repo-jacking → closed** (which Go/Maven cannot do). If `alice`
  (id 100) publishes and later deletes the account, an attacker who grabs the
  login `alice` (id 999) passes check (1) but fails check (2): `github/alice` is
  pinned to id 100. The namespace freezes to the original account.
- **Rogue issuer → rejected.** An attacker running their own OIDC issuer that
  asserts `repository_owner: google` is either absent from `issuers.json`
  (rejected) or maps to its own distinct, host-qualified label — it can never
  mint a `github/...` name. This is the operator-controlled label invariant.
- **Provider id-stability is surfaced, not assumed.** Rows mark whether
  `owner_id` is truly immutable. Email-style providers (`id_stable: false`) give
  a weaker reclaim guarantee; `add`/audit output shows "verified via `github`
  (id-stable)" vs "verified via `sigstore` (email-stable)" so consumers see the
  strength of the binding.
- **Residual blast radius is capped** by the capability footprint gate
  regardless of naming — itself a witchy-authored check.

## 6. Implementation phases (witchy-first)

Each phase is independently landable and testable. File targets are
`projects/coven/*.witchy` and `projects/pm/src/pm.witchy` except where the native
seam (`std/crypto` + the runtime) is named explicitly.

- **Phase 0 — name shape + validation.** Three-segment publishable names;
  `valid_name` rules; `namespace_of` → `provider/owner`. Update example/fixture
  manifests, and `projects/coven-web/seed-examples.mjs` (its two-segment
  `examples/*` runes become `local/`-or-bare dev data under the new shape). No auth
  changes. Touches: `coven_validate.witchy`, `coven_trust.witchy`, `pm.witchy`,
  fixtures.
- **Phase 1 — real OIDC verification (the one TCB change).** Add
  `crypto.rsa_pkcs1_sha256_verify` (placeholder in `std/crypto.witchy`, interpreter
  intercept, WASM host import — mirrors `ed25519_verify`). Build JWKS discovery +
  compact-JWT parse + `verify_jwt` in `coven_trust.witchy` (gains a `Net[Connect, Tcp]`
  capability, dialed to a `tls:` endpoint for HTTPS discovery — depends on RFC-0009), replacing the Ed25519
  envelope. Update the dev IdP
  (`src/idp.rs`) to mint RS256/ES256 provider-shaped tokens and serve a test JWKS,
  so the differential tests can verify real tokens. Touches: `std/crypto.witchy` +
  runtime (intrinsic), `coven_trust.witchy`, `src/idp.rs`.
- **Phase 2 — declarative claim-map + `derive_namespace`.** Grow `Issuer` →
  `IssuerCfg` rows; `derive_namespace`. Touches: `coven_trust.witchy`,
  `issuers.json`.
- **Phase 3 — `authorize_publish` collapse + `namespaces.json`.** Derive-equality +
  immutable-id consistency; remove the bound-policy TOFU struct. Touches:
  `coven.witchy` (`authorized_publisher`), `coven_trust.witchy`.
- **Phase 4 — promote (decoupled gate).** Keep SoD + maintainer-TOFU; require a
  human 2FA challenge via any login method; drop any namespace-matching. Touches:
  `coven.witchy` (`authorized_promoter`).
- **Phase 5 — surfacing + provenance.** `pm.witchy` `add`/audit output shows the
  verified provider + id-stability; the signed record/provenance carries it; the
  lockfile carries the full namespaced name; **coven-web displays the provider and
  binding strength** (dogfood: the registry UI shows the identity it verified).
  Touches: `pm.witchy`, `coven_record.witchy`, coven-web views.
- **Phase 6 — multi-provider proof.** Differential/e2e tests: a GitHub-shaped
  (RS256) and a GitLab-shaped issuer publish under non-colliding namespaces; rogue
  issuer rejected; reclaim (different id) rejected; rename freezes the old
  namespace. Touches: `coven_test.witchy` + the differential harness.

## 7. Open questions / deferred

- **Org-membership promotion** — resolved by the decoupled promote gate (§4.5): a
  human member promotes with their own verified identity (any provider) + 2FA,
  matched against the namespace's recorded maintainers; their token need not derive
  to the org's namespace. The only residual detail is bootstrapping an org
  namespace's initial maintainer set, which maintainer-TOFU on first promote covers.
- **Rename / host-move / transfer / alias** — deferred by decision. Old namespace
  freezes; a later alias record maps old→new.
- **Anonymous local registry mode** — with no issuers configured there is no
  identity to derive. Keep a `local/<name>` dev-only escape or bare names for the
  fully-local store; never reachable on a remote registry.
- **Self-hosted JWKS trust** — operator must host-qualify the label; document the
  invariant prominently.
- **JWKS fetch hardening** — discovery is the layer's `Net` use; cache with a TTL,
  bound response size, and treat fetch failure as "issuer temporarily unverifiable"
  (publish refused, never silently downgraded). A witchy concern, not a native one.

## 8. The native seam (what cannot be witchy)

This RFC's only non-witchy surface is **one crypto intrinsic**:
`crypto.rsa_pkcs1_sha256_verify`, added to `std/crypto.witchy`'s existing native
seam beside `sha256`, `ed25519_verify`, and `ecdsa_p256_verify`. ES256 is already
there. RSA/EC signature math cannot be expressed in witchy (no byte access, no
field arithmetic), exactly the criterion that already lives at that seam.

Everything else is witchy in `projects/coven/`: discovery, JWT/JWKS parsing, the
declarative claim map, `derive_namespace`, `authorize_publish`/`authorize_promote`,
the namespace/maintainer stores, and the name validation. The dev IdP
(`src/idp.rs`) stays a Rust **test helper** — it mints provider-shaped tokens and
serves a JWKS for the differential tests — not part of the TCB or the request path.

**Byte-compatibility.** coven's signed record + provenance payload must stay
identical to the Rust differential-test oracle (the cross-verifier golden tests).
The namespace change only widens the record's `name` string; the signing-payload
structure is unchanged, so re-running the golden tests after Phase 3 is sufficient.

## 9. The disambiguation contract (and custom providers)

The whole model reduces to one principle: **coven never invents identity — it
transcribes each authority's existing identity system into one namespace,
qualified by which authority vouched.** A rune's global identity is a tuple:

```
(authority, subject, rune)      e.g.  (github, owner-id 1234567, http)
```

displayed as `github/insanitybit/http`. Each level is disambiguated by the level
above it, and the top is disambiguated by the operator:

- **authority** — disambiguated by the operator-curated, host-qualified label.
  Only the genuine issuer maps to `github`; a self-hosted instance gets its own
  label (`gitlab.acme-corp.com`). Authorities can't collide or self-assert their
  label.
- **subject** — disambiguated by the authority itself. GitHub already guarantees
  unique, stable account ids; coven just reads `repository_owner_id` out and does
  **no** within-authority disambiguation of its own.
- **rune** — disambiguated by the subject (the publisher's own choice, scoped to
  their prefix).

### Acceptance test for any provider

Built-in, corporate IdP, or fully custom — a provider slots in iff it hands coven
enough to disambiguate:

1. a **stable, unique subject id** within the authority (→ `owner_id`; supplies
   uniqueness + reclaim-resistance),
2. a **human-readable owner** (→ display label), and
3. an operator-assigned **authority label** (→ the prefix).

If yes, it's a config row with no new code. A provider that offers only a
*mutable* subject key (e.g. a reassignable email) can still be an authority, but
`id_stable: false` records the weaker reclaim guarantee.

### Who may add an authority

The registry **operator**, never a self-service publisher — that
operator-controlled root is the only thing standing between the model and the
`google/` grab. Operator-configured custom providers are first-class: a private
coven trusting a corporate IdP (Okta/Auth0/Keycloak), or a self-hosted forge with
a host-qualified label. Open "bring your own issuer" on a *shared* registry stays
off by default — it would re-anchor trust on issuer-domain ownership, the
DNS/expiry weakness this design exists to avoid. (Opt-in for private deployments,
where namespaces could be derived from the verified issuer domain.)
