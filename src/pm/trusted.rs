//! Trusted Publishing — keyless, OIDC-style authentication for publish/promote.
//!
//! Long-lived API tokens are the single most-stolen supply-chain credential, so
//! coven does not use them. Instead a publisher proves identity with a
//! short-lived **identity token** (an OIDC ID token): a signed assertion from a
//! trusted issuer (a CI provider's IdP, or a human SSO) carrying claims about
//! *who* is publishing and *from where* (`repository`, `workflow_ref`, `ref`,
//! `sub`). The registry verifies the token against the issuer's published keys
//! and matches its claims to the namespace's **trust policy**. Nothing is ever
//! stored on the client — the CI mints a fresh token per run.
//!
//! Modeled locally without external dependencies: identity tokens are
//! Ed25519-signed JSON envelopes (standing in for an RS256/ES256 JWT) and a
//! trusted issuer is an Ed25519 public key (standing in for the issuer's JWKS).
//! The *structure, claims, and verification logic mirror real OIDC* — swapping
//! in JWT parsing + live JWKS fetch is a thin adapter at [`verify`] and
//! [`TrustStore::issuer_key`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::keys::{self, RegistryKey};
use super::{err, PmResult};

/// The audience an identity token must be minted for — proves the token was
/// intended for *this* registry, not replayed from elsewhere.
pub const AUDIENCE: &str = "coven-registry";

/// Identity-token claims (a subset of OIDC standard + CI-provider claims).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Issuer, e.g. `https://token.actions.githubusercontent.com`.
    pub iss: String,
    /// Subject — the workload or human identity, e.g.
    /// `repo:acme/http-client:ref:refs/heads/main`.
    pub sub: String,
    /// Audience — must equal [`AUDIENCE`].
    pub aud: String,
    /// Expiry (unix seconds) — identity tokens are short-lived.
    pub exp: u64,
    pub iat: u64,
    /// Provider claims: `repository`, `workflow_ref`, `ref`, `environment`,
    /// `actor`, … . Sorted for deterministic signing.
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
}

impl Claims {
    pub fn claim(&self, key: &str) -> Option<&str> {
        self.extra.get(key).map(|s| s.as_str())
    }
}

/// A signed identity token (a JWT stand-in: `claims` + an issuer signature).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdToken {
    pub claims: Claims,
    /// Ed25519 signature (hex) over the canonical claims, by the issuer's key.
    pub sig: String,
}

fn canonical(claims: &Claims) -> Vec<u8> {
    serde_json::to_vec(claims).unwrap_or_default()
}

/// Mint an identity token — the IdP's job (a CI provider does this
/// automatically per run; modeled here). The `issuer_key` is the IdP's signing
/// key whose public half the registry trusts.
pub fn mint(issuer_key: &RegistryKey, claims: Claims) -> IdToken {
    let sig = issuer_key.sign(&canonical(&claims));
    IdToken { claims, sig }
}

/// Verify an identity token: signature against the trusted issuer's public key,
/// audience, and expiry. Returns the verified claims. (Real OIDC additionally
/// checks `nbf`, key rotation, etc.; the shape is identical.)
pub fn verify(issuer_pubkey_hex: &str, token: &IdToken, now: u64) -> PmResult<Claims> {
    if !keys::verify(issuer_pubkey_hex, &canonical(&token.claims), &token.sig) {
        return err("identity token signature is invalid (untrusted or forged)");
    }
    if token.claims.aud != AUDIENCE {
        return err(format!(
            "identity token audience `{}` is not `{AUDIENCE}` (wrong registry / replay)",
            token.claims.aud
        ));
    }
    if token.claims.exp <= now {
        return err("identity token has expired");
    }
    Ok(token.claims.clone())
}

// ---------------------------------------------------------------------------
// Trust policies
// ---------------------------------------------------------------------------

/// Which CI identity may *stage* releases for a namespace.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PublisherPolicy {
    pub issuer: String,
    /// Required claim matches (e.g. `repository` -> `acme/http-client`,
    /// `workflow_ref` -> `acme/http-client/.github/workflows/release.yml@...`).
    #[serde(default)]
    pub claims: BTreeMap<String, String>,
}

/// A human identity authorized to *promote* (release) for a namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintainerId {
    pub issuer: String,
    pub sub: String,
}

/// A namespace's trust policy: who may stage (a CI workload) and who may promote
/// (human maintainers). Machines stage; humans release — separation of duties
/// with no long-lived secret anywhere.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NamespacePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<PublisherPolicy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maintainers: Vec<MaintainerId>,
}

/// The claims a publisher policy binds on (TOFU) and later enforces — the repo
/// and the exact workflow, exactly like PyPI/npm trusted publishers.
const BOUND_CLAIMS: &[&str] = &["repository", "workflow_ref"];

fn namespace_of(name: &str) -> &str {
    name.split('/').next().unwrap_or(name)
}

/// Persistent trust state for a registry: the set of trusted issuers (a JWKS
/// stand-in) and the per-namespace policies. Lives alongside the registry data.
pub struct TrustStore {
    root: PathBuf,
}

impl TrustStore {
    pub fn new(root: PathBuf) -> TrustStore {
        TrustStore { root }
    }

    fn issuers_path(&self) -> PathBuf {
        self.root.join("issuers.json")
    }
    fn policies_path(&self) -> PathBuf {
        self.root.join("trust.json")
    }

    fn load_issuers(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(self.issuers_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Register a trusted issuer (its public key). Models pinning a provider's
    /// JWKS; in production this is admin configuration.
    pub fn add_issuer(&self, issuer: &str, pubkey_hex: &str) -> PmResult<()> {
        std::fs::create_dir_all(&self.root)?;
        let mut issuers = self.load_issuers();
        issuers.insert(issuer.to_string(), pubkey_hex.to_string());
        let text = serde_json::to_string_pretty(&issuers)
            .map_err(|e| super::PmError(format!("issuers: {e}")))?;
        std::fs::write(self.issuers_path(), text)?;
        Ok(())
    }

    /// The trusted public key for an issuer, if any.
    pub fn issuer_key(&self, issuer: &str) -> Option<String> {
        self.load_issuers().get(issuer).cloned()
    }

    fn load_policies(&self) -> BTreeMap<String, NamespacePolicy> {
        std::fs::read_to_string(self.policies_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_policies(&self, policies: &BTreeMap<String, NamespacePolicy>) -> PmResult<()> {
        std::fs::create_dir_all(&self.root)?;
        let text = serde_json::to_string_pretty(policies)
            .map_err(|e| super::PmError(format!("trust: {e}")))?;
        std::fs::write(self.policies_path(), text)?;
        Ok(())
    }

    /// The trust policy for a namespace, if one has been bound. (Exposed for
    /// inspection/audit tooling and exercised by tests.)
    #[allow(dead_code)]
    pub fn policy(&self, namespace: &str) -> Option<NamespacePolicy> {
        self.load_policies().get(namespace).cloned()
    }

    /// Verify an identity token against the trusted issuers and return its
    /// claims. The first step of any trusted action.
    pub fn verify_token(&self, token: &IdToken, now: u64) -> PmResult<Claims> {
        let key = self.issuer_key(&token.claims.iss).ok_or_else(|| {
            super::PmError(format!("issuer `{}` is not trusted by this registry", token.claims.iss))
        })?;
        verify(&key, token, now)
    }

    /// Authorize a publish (stage) by a verified CI identity. The first trusted
    /// publish to a namespace *binds* the policy (issuer + repository +
    /// workflow); later publishes must match it. Returns the canonical publisher
    /// identity string for provenance.
    pub fn authorize_publish(&self, name: &str, claims: &Claims) -> PmResult<()> {
        let ns = namespace_of(name).to_string();
        let mut policies = self.load_policies();
        let entry = policies.entry(ns.clone()).or_default();
        match &entry.publisher {
            None => {
                let mut bind = BTreeMap::new();
                for k in BOUND_CLAIMS {
                    if let Some(v) = claims.claim(k) {
                        bind.insert((*k).to_string(), v.to_string());
                    }
                }
                entry.publisher = Some(PublisherPolicy {
                    issuer: claims.iss.clone(),
                    claims: bind,
                });
                self.save_policies(&policies)
            }
            Some(p) => {
                if p.issuer != claims.iss {
                    return err(format!(
                        "namespace `{ns}` trusts issuer `{}`, not `{}`",
                        p.issuer, claims.iss
                    ));
                }
                for (k, v) in &p.claims {
                    if claims.claim(k) != Some(v.as_str()) {
                        return err(format!(
                            "identity not authorized to publish to `{ns}`: claim `{k}` is `{}`, policy requires `{v}`",
                            claims.claim(k).unwrap_or("<missing>")
                        ));
                    }
                }
                Ok(())
            }
        }
    }

    /// Authorize a promote (release) by a verified human identity. The first
    /// promoter becomes a maintainer (TOFU); later promotes must be a registered
    /// maintainer. (The second factor is enforced separately by the registry.)
    pub fn authorize_promote(&self, name: &str, claims: &Claims) -> PmResult<()> {
        let ns = namespace_of(name).to_string();
        let mut policies = self.load_policies();
        let entry = policies.entry(ns.clone()).or_default();
        let id = MaintainerId {
            issuer: claims.iss.clone(),
            sub: claims.sub.clone(),
        };
        if entry.maintainers.is_empty() {
            entry.maintainers.push(id);
            self.save_policies(&policies)
        } else if entry.maintainers.contains(&id) {
            Ok(())
        } else {
            err(format!("`{}` is not a maintainer of namespace `{ns}`", claims.sub))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "witchy-trusted-{}-{}",
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn claims(iss: &str, sub: &str, repo: &str, wf: &str, exp: u64) -> Claims {
        Claims {
            iss: iss.into(),
            sub: sub.into(),
            aud: AUDIENCE.into(),
            exp,
            iat: 0,
            extra: BTreeMap::from([
                ("repository".into(), repo.into()),
                ("workflow_ref".into(), wf.into()),
            ]),
        }
    }

    #[test]
    fn mint_and_verify_roundtrip() {
        let dir = tmp();
        let issuer = RegistryKey::load_or_create(&dir).unwrap();
        let tok = mint(&issuer, claims("gha", "repo:acme/x", "acme/x", "wf.yml", 100));
        // Good key + within expiry -> ok.
        assert!(verify(&issuer.public_hex(), &tok, 50).is_ok());
        // Expired -> rejected.
        assert!(verify(&issuer.public_hex(), &tok, 200).is_err());
        // Wrong issuer key -> rejected.
        let other_dir = tmp();
        let other = RegistryKey::load_or_create(&other_dir).unwrap();
        assert!(verify(&other.public_hex(), &tok, 50).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other_dir);
    }

    #[test]
    fn untrusted_issuer_is_rejected() {
        let dir = tmp();
        let store = TrustStore::new(dir.join("reg"));
        let issuer = RegistryKey::load_or_create(&dir.join("idp")).unwrap();
        let tok = mint(&issuer, claims("gha", "s", "acme/x", "wf", 100));
        // No issuers registered yet.
        assert!(store.verify_token(&tok, 50).is_err());
        // Register the issuer -> now trusted.
        store.add_issuer("gha", &issuer.public_hex()).unwrap();
        assert!(store.verify_token(&tok, 50).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn publish_policy_binds_then_enforces_repo_and_workflow() {
        let dir = tmp();
        let store = TrustStore::new(dir.clone());
        // First publish from acme/x via release.yml binds the namespace.
        let c1 = claims("gha", "s", "acme/x", "release.yml", 100);
        store.authorize_publish("acme/widget", &c1).unwrap();
        // Same repo+workflow -> ok.
        store.authorize_publish("acme/widget", &c1).unwrap();
        // Different repository -> rejected (namespace squat / wrong source).
        let c2 = claims("gha", "s", "evil/x", "release.yml", 100);
        assert!(store.authorize_publish("acme/widget", &c2).is_err());
        // Different workflow -> rejected (a non-release workflow can't publish).
        let c3 = claims("gha", "s", "acme/x", "ci.yml", 100);
        assert!(store.authorize_publish("acme/widget", &c3).is_err());
        // Different issuer -> rejected.
        let c4 = claims("gitlab", "s", "acme/x", "release.yml", 100);
        assert!(store.authorize_publish("acme/widget", &c4).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn promote_maintainers_tofu() {
        let dir = tmp();
        let store = TrustStore::new(dir.clone());
        let alice = claims("sso", "alice", "", "", 100);
        store.authorize_promote("acme/widget", &alice).unwrap(); // claims maintainer
        store.authorize_promote("acme/widget", &alice).unwrap(); // still ok
        let mallory = claims("sso", "mallory", "", "", 100);
        assert!(store.authorize_promote("acme/widget", &mallory).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
