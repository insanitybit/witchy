---
status: planned
note: Imported from docs/ under RFC-0001. Design accepted, not yet built.
---

# Coven Namespaces — Provider-Derived Identity Plan

Status: design + implementation plan. Not yet built. Supersedes the
author-chosen `ns/name` + per-namespace TOFU model in `rfcs/package-manager.md`
§8 for the *naming and trusted-publishing* surface; the rest of that spec
(footprint gate, two-phase publish, TUF, content addressing) is unchanged.

## 1. Goal

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
- **rune / module name** = the last segment (`http`). `Manifest::module_name()`
  already returns the last `/`-segment, so the in-language `import http` is
  unchanged.
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

## 4. Architecture

### 4.1 Declarative provider table (`issuers.json`)

Today `issuers.json` maps `issuer -> pubkey_hex`. Replace with a richer row:

```jsonc
"https://token.actions.githubusercontent.com": {
  "provider":       "github",
  "owner_claim":    "repository_owner",
  "owner_id_claim": "repository_owner_id",
  "id_stable":      true,
  "audience":       "coven-registry"
}
```

Verification keys are **not** stored here in the real model — they come from
OIDC discovery (§4.2). Representative rows:

| provider | owner_claim | owner_id_claim | id_stable |
|---|---|---|---|
| `github`   | `repository_owner` | `repository_owner_id` | true |
| `gitlab`   | `namespace_path`   | `namespace_id`        | true |
| `sigstore` | `email`            | `sub`                 | false (email reassignable) |

The provider label is operator-assigned per issuer. Self-hosted instances get
their own host-qualified label (e.g. `gitlab.acme-corp.com`) so they can never
mint another provider's namespace.

### 4.2 OIDC verification (uniform, written once)

1. Resolve the issuer's `<issuer>/.well-known/openid-configuration` →
   `jwks_uri`; fetch + cache the JWKS (rotating RS256/ES256 keys keyed by
   `kid`).
2. Verify the token signature by `kid`, plus `iss` is trusted, `aud` matches the
   row's audience, and `exp`/`iat` are valid.
3. Return the generic `Claims` (provider-specific values live in `extra`).

Nothing here is provider-specific — every compliant OIDC provider exposes
discovery + JWKS + audience.

### 4.3 `derive_namespace`

```rust
struct Ns { provider: String, owner: String, owner_id: String, id_stable: bool }

fn derive_namespace(cfg: &IssuerCfg, c: &Claims) -> PmResult<Ns> {
    Ok(Ns {
        provider:  cfg.provider.clone(),
        owner:     c.claim(&cfg.owner_claim).ok_or(/*…*/)?.to_string(),
        owner_id:  c.claim(&cfg.owner_id_claim).ok_or(/*…*/)?.to_string(),
        id_stable: cfg.id_stable,
    })
}
// string form of the namespace: format!("{}/{}", provider, owner)
```

### 4.4 `authorize_publish` (collapses the TOFU policy)

```rust
fn authorize_publish(&self, name: &str, claims: &Claims) -> PmResult<()> {
    let cfg  = self.issuer_cfg(&claims.iss)?;       // untrusted issuer -> reject
    let want = derive_namespace(&cfg, claims)?;
    let got  = namespace_of(name);                  // "provider/owner" from manifest

    // (1) may ONLY publish under the namespace the token proves
    if got != format!("{}/{}", want.provider, want.owner) {
        return err(format!("token authorizes `{}/{}`, not `{got}`",
                           want.provider, want.owner));
    }
    // (2) immutable-id consistency; first publish records the id
    match self.namespace_owner_id(got) {
        Some(id) if id != want.owner_id =>
            err(format!("`{got}` is bound to a different account id")),
        Some(_) => Ok(()),
        None    => self.bind_namespace_id(got, &want.owner_id),
    }
}
```

`BOUND_CLAIMS` and the `PublisherPolicy { issuer, claims }` TOFU struct are
removed. The per-namespace store shrinks to:

```jsonc
// namespaces.json   "provider/owner" -> owner_id
"github/insanitybit": "1234567"
```

### 4.5 `authorize_promote`

Promotion (human release step) keeps maintainer-TOFU but additionally requires
the promoter's token to derive to the *same* `provider/owner`. Separation of
duties holds because the CI subject (`repo:…/workflow`) ≠ the human subject.
Org-membership promotion (where the namespace owner is an org, not the promoting
human) is the one **open question** (§7); for now, personal namespaces work
directly and org promotion falls back to recorded maintainers.

### 4.6 Manifest validation (`Manifest::validate`)

For publishable runes: exactly three `/`-segments; each segment matches
`[a-z0-9._-]+`; `provider` is a known label; no additional `/`. Keep the
single-bare-segment escape hatch for local apps.

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
  regardless of naming.

## 6. Implementation phases

Each phase is independently landable and testable.

- **Phase 0 — name shape + manifest validation.** Three-segment publishable
  names; `validate()` rules; update fixtures/examples. No auth changes.
  Touches: `manifest.rs`, example/test manifests.
- **Phase 1 — real OIDC verification.** JWKS discovery + cache, RS256/ES256,
  audience + expiry, replacing the Ed25519 stand-in. Update `coven-gen-issuer` /
  `coven-mint-token` to RSA/EC keypairs + a JWKS endpoint so tests can mint
  real provider-shaped tokens. Touches: `trusted.rs`, `http.rs`, CLI; adds a
  JWT/JOSE dependency (latest).
- **Phase 2 — declarative claim-map + `derive_namespace`.** Richer `issuers.json`
  rows; `IssuerCfg`; `derive_namespace`. Touches: `trusted.rs`.
- **Phase 3 — `authorize_publish` collapse + `namespaces.json`.** Derive-equality
  + immutable-id consistency; remove `BOUND_CLAIMS` / `PublisherPolicy`.
  Touches: `trusted.rs`, `server.rs` (error strings only — call site unchanged).
- **Phase 4 — promote.** Promoter must derive to the same `provider/owner`;
  keep maintainer-TOFU. Touches: `trusted.rs`.
- **Phase 5 — surfacing + provenance.** Show the verified provider + id-stability
  in `add`/audit and in the signed record/provenance; lockfile carries the full
  namespaced name. Touches: `cli.rs`, `lockfile.rs`, `registry.rs`.
- **Phase 6 — multi-provider proof.** Add a second (GitLab-shaped) issuer in
  tests; e2e: two providers publish under non-colliding namespaces; rogue issuer
  rejected; reclaim (different id) rejected; rename freezes the old namespace.
  Touches: `tests/e2e.rs`.

## 7. Open questions / deferred

- **Org-membership promotion** — when the namespace owner is an org and a human
  member promotes, the human token's `owner` claim may differ from the org. Needs
  a membership claim mapping; deferred behind recorded maintainers.
- **Rename / host-move / transfer / alias** — deferred by decision. Old namespace
  freezes; a later alias record maps old→new.
- **Anonymous local registry mode** — with no issuers configured there is no
  identity to derive. Keep a `local/<name>` dev-only escape or bare names for the
  fully-local store; never reachable on a remote registry.
- **Self-hosted JWKS trust** — operator must host-qualify the label; document the
  invariant prominently.

## 8. Interop note

The witchy self-hosted coven (`projects/coven/`) mirrors the Rust trusted
publishing and must stay byte-compatible with the Rust verifiers (signed records,
TUF metadata). The namespace change only widens the record's `name` string; the
signing-payload structure is unchanged. Re-run the cross-verifier golden tests
after Phase 3, and mirror the model into `projects/coven/` after the Rust side
lands.

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
