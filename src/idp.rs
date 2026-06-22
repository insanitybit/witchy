//! Identity-provider (IdP) test tooling for trusted publishing.
//!
//! Trusted publishing means coven never accepts long-lived API tokens: a
//! publisher proves identity with a short-lived, signed **identity token** (an
//! OIDC ID token) minted per CI run. In production the issuer is a CI provider's
//! IdP (its published JWKS); locally we model it with an Ed25519 keypair and a
//! signed JSON envelope, structurally mirroring a JWT.
//!
//! This module is the **minting side only** — the two `witchyc` helper commands
//! a developer or the e2e harness uses to stand up a test IdP:
//!
//!   * `witchy coven-gen-issuer [--out DIR]` — generate an issuer signing key,
//!     printing its public key (hex) to register with `coven-serve
//!     --trust-issuer`.
//!   * `witchy coven-mint-token --issuer-key DIR --sub S [--claim k=v]...` — mint
//!     an identity token, printing the token JSON for `COVEN_ID_TOKEN`.
//!
//! Server-side verification + trust policy lives in the witchy coven
//! (`projects/coven`); this is the developer-facing key/token generator that
//! RFC-0004 keeps as a Rust helper (key generation stays in the toolchain). The
//! on-disk key format and the token JSON are byte-compatible with what the witchy
//! coven verifies — do not change either.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// An error from an IdP helper command.
#[derive(Debug)]
pub struct IdpError(pub String);

impl std::fmt::Display for IdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for IdpError {}

impl From<std::io::Error> for IdpError {
    fn from(e: std::io::Error) -> Self {
        IdpError(e.to_string())
    }
}

type IdpResult<T> = Result<T, IdpError>;

// ---------------------------------------------------------------------------
// Issuer key (an IdP's Ed25519 signing key — a JWKS stand-in)
// ---------------------------------------------------------------------------

/// An issuer (IdP) signing key. Lives at `<dir>/root.key` (the 32-byte seed,
/// hex), with the public half written alongside at `<dir>/root.pub`. The format
/// matches the witchy coven's registry keys exactly.
pub struct RegistryKey {
    signing: SigningKey,
}

impl RegistryKey {
    /// Load the issuer key, generating one on first use.
    pub fn load_or_create(dir: &Path) -> IdpResult<RegistryKey> {
        let key_path = dir.join("root.key");
        if key_path.exists() {
            let seed = read_seed(&key_path)?;
            return Ok(RegistryKey {
                signing: SigningKey::from_bytes(&seed),
            });
        }
        std::fs::create_dir_all(dir)?;
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| IdpError(format!("os rng: {e}")))?;
        let signing = SigningKey::from_bytes(&seed);
        write_private(&key_path, &seed)?;
        std::fs::write(dir.join("root.pub"), hex_encode(signing.verifying_key().as_bytes()))?;
        Ok(RegistryKey { signing })
    }

    pub fn sign(&self, msg: &[u8]) -> String {
        hex_encode(&self.signing.sign(msg).to_bytes())
    }

    pub fn public_hex(&self) -> String {
        hex_encode(self.signing.verifying_key().as_bytes())
    }
}

fn read_seed(path: &Path) -> IdpResult<[u8; 32]> {
    let text = std::fs::read_to_string(path)?;
    let bytes = hex_decode(text.trim()).ok_or_else(|| IdpError("corrupt root.key".into()))?;
    bytes
        .try_into()
        .map_err(|_| IdpError("root.key is not a 32-byte seed".into()))
}

fn write_private(path: &Path, seed: &[u8; 32]) -> IdpResult<()> {
    std::fs::write(path, hex_encode(seed))?;
    // Best-effort lock-down of the private key file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Identity tokens (a JWT stand-in)
// ---------------------------------------------------------------------------

/// The audience an identity token must be minted for — proves the token was
/// intended for *this* registry, not replayed elsewhere. Must match the witchy
/// coven's expected audience.
pub const AUDIENCE: &str = "coven-registry";

/// Identity-token claims (a subset of OIDC standard + CI-provider claims).
/// Serialized field order is load-bearing: it is the canonical signing payload
/// the witchy coven re-derives to verify, so do not reorder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Issuer, e.g. `https://token.actions.githubusercontent.com`.
    pub iss: String,
    /// Subject — the workload or human identity.
    pub sub: String,
    /// Audience — must equal [`AUDIENCE`].
    pub aud: String,
    /// Expiry (unix seconds).
    pub exp: u64,
    pub iat: u64,
    /// Provider claims (`repository`, `workflow_ref`, ...), sorted for
    /// deterministic signing.
    #[serde(default)]
    pub extra: BTreeMap<String, String>,
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

/// Mint an identity token — the IdP's job (a CI provider does this per run).
pub fn mint(issuer_key: &RegistryKey, claims: Claims) -> IdToken {
    let sig = issuer_key.sign(&canonical(&claims));
    IdToken { claims, sig }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Argument parsing (minimal, value-flag only)
// ---------------------------------------------------------------------------

const VALUE_FLAGS: &[&str] = &["--out", "--issuer-key", "--issuer", "--sub", "--ttl", "--claim"];

struct Args {
    values: BTreeMap<String, Vec<String>>,
}

fn parse_args(rest: &[String]) -> Args {
    let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut i = 0;
    while i < rest.len() {
        let tok = &rest[i];
        if VALUE_FLAGS.contains(&tok.as_str()) && i + 1 < rest.len() {
            values.entry(tok.clone()).or_default().push(rest[i + 1].clone());
            i += 2;
        } else {
            i += 1;
        }
    }
    Args { values }
}

impl Args {
    fn val(&self, flag: &str) -> Option<&str> {
        self.values.get(flag).and_then(|v| v.first()).map(|s| s.as_str())
    }
    fn vals(&self, flag: &str) -> Vec<String> {
        self.values.get(flag).cloned().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// `witchy coven-gen-issuer [--out DIR]` — generate an issuer (IdP) signing
/// keypair, printing its public key (hex) for registration with `coven-serve
/// --trust-issuer`. Models a CI provider's OIDC signing key.
pub fn gen_issuer(rest: &[String]) -> IdpResult<()> {
    let a = parse_args(rest);
    let dir = PathBuf::from(a.val("--out").unwrap_or("./issuer-key"));
    let key = RegistryKey::load_or_create(&dir)?;
    println!("{}", key.public_hex());
    eprintln!("issuer key written to {} (keep root.key secret)", dir.display());
    Ok(())
}

/// `witchy coven-mint-token --issuer-key DIR --sub S [--claim k=v]...` — mint a
/// short-lived identity token (the IdP's job — a CI provider does this per run).
/// Prints the token JSON for `COVEN_ID_TOKEN`.
pub fn mint_token(rest: &[String]) -> IdpResult<()> {
    let a = parse_args(rest);
    let key_dir = a
        .val("--issuer-key")
        .ok_or_else(|| IdpError("--issuer-key <dir> is required".into()))?;
    let key = RegistryKey::load_or_create(Path::new(key_dir))?;
    let iss = a.val("--issuer").unwrap_or("local-idp").to_string();
    let sub = a.val("--sub").unwrap_or("anonymous").to_string();
    let ttl: u64 = a.val("--ttl").and_then(|s| s.parse().ok()).unwrap_or(300);
    let now = now_unix();
    let mut extra = BTreeMap::new();
    for c in a.vals("--claim") {
        if let Some((k, v)) = c.split_once('=') {
            extra.insert(k.to_string(), v.to_string());
        }
    }
    let claims = Claims {
        iss,
        sub,
        aud: AUDIENCE.to_string(),
        exp: now + ttl,
        iat: now,
        extra,
    };
    let token = mint(&key, claims);
    println!(
        "{}",
        serde_json::to_string(&token).map_err(|e| IdpError(e.to_string()))?
    );
    Ok(())
}

/// A short, comparable fingerprint of a hex-encoded public key (for TOFU
/// pinning). Kept for parity with the registry's fingerprint scheme; the hash is
/// over the public-key hex string.
#[allow(dead_code)]
fn fingerprint_of(pub_hex: &str) -> String {
    let mut h = Sha256::new();
    h.update(pub_hex.as_bytes());
    let d = h.finalize();
    let mut s = String::from("ed25519:");
    for b in &d[..8] {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "witchy-idp-{}-{}-{}",
            std::process::id(),
            CTR.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn key_is_persistent_and_writes_public() {
        let dir = tmp();
        let a = RegistryKey::load_or_create(&dir).unwrap().public_hex();
        let b = RegistryKey::load_or_create(&dir).unwrap().public_hex();
        assert_eq!(a, b, "reloading must yield the same key");
        let pubfile = std::fs::read_to_string(dir.join("root.pub")).unwrap();
        assert_eq!(pubfile.trim(), a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn minted_token_is_signed_over_canonical_claims() {
        let dir = tmp();
        let key = RegistryKey::load_or_create(&dir).unwrap();
        let claims = Claims {
            iss: "gha".into(),
            sub: "repo:acme/x".into(),
            aud: AUDIENCE.into(),
            exp: 1000,
            iat: 500,
            extra: BTreeMap::from([("repository".into(), "acme/x".into())]),
        };
        let tok = mint(&key, claims);
        // The signature verifies over the canonical claims under the issuer key.
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let pk: [u8; 32] = hex_decode(&key.public_hex()).unwrap().try_into().unwrap();
        let vk = VerifyingKey::from_bytes(&pk).unwrap();
        let sig_bytes: [u8; 64] = hex_decode(&tok.sig).unwrap().try_into().unwrap();
        assert!(vk.verify(&canonical(&tok.claims), &Signature::from_bytes(&sig_bytes)).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_is_stable_and_prefixed() {
        let dir = tmp();
        let key = RegistryKey::load_or_create(&dir).unwrap();
        let fp = fingerprint_of(&key.public_hex());
        assert!(fp.starts_with("ed25519:"));
        assert_eq!(fp, fingerprint_of(&key.public_hex()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
