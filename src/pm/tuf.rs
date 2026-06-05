//! TUF (The Update Framework) snapshot + timestamp roles.
//!
//! Per-record Ed25519 signatures (the *targets* role) make each version's
//! metadata tamper-evident. But that alone can't stop a malicious mirror from
//! serving a *consistent but stale* view — an old snapshot that omits a security
//! fix (rollback), or a frozen one that never updates (freeze). The snapshot and
//! timestamp roles close that:
//!
//! - **snapshot**: a signed, version-numbered manifest of every current target's
//!   digest. A monotonically increasing version detects rollback/mix-and-match.
//! - **timestamp**: a short-lived signed pointer to the current snapshot
//!   (version + hash + expiry). Its expiry detects freeze attacks.
//!
//! Note: in this model all roles are signed by the registry root key (the key a
//! client pins via TOFU). A fully separated key hierarchy (distinct
//! snapshot/timestamp/targets keys delegated by root) is a further refinement;
//! the *protections* here are real.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::keys::{self, RegistryKey};

/// How long a freshly minted timestamp is valid (freeze-attack window).
pub const TIMESTAMP_TTL_SECS: u64 = 24 * 60 * 60;

/// The snapshot role's payload: every current target's digest, version-numbered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u64,
    pub created: u64,
    /// `"name@version"` -> digest of that record's signed payload.
    pub targets: BTreeMap<String, String>,
}

/// The timestamp role's payload: a short-lived pointer to the current snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timestamp {
    pub snapshot_version: u64,
    pub snapshot_hash: String,
    pub expires: u64,
}

/// A role payload plus its signature over the payload's canonical bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signed<T> {
    pub signed: T,
    pub sig: String,
}

/// Canonical bytes of a role payload (deterministic: structs have fixed field
/// order and `targets` is a sorted `BTreeMap`).
pub fn canonical<T: Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let d = h.finalize();
    let mut s = String::from("sha256:");
    for b in &d[..] {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The digest of a record's *signed payload* — what the snapshot pins, so a
/// record swapped to an older state (e.g. un-yanked) no longer matches.
pub fn target_digest(signing_payload: &str) -> String {
    sha256_hex(signing_payload.as_bytes())
}

pub fn sign<T: Serialize>(key: &RegistryKey, signed: T) -> Signed<T> {
    let sig = key.sign(&canonical(&signed));
    Signed { signed, sig }
}

pub fn verify_signed<T: Serialize>(pubkey_hex: &str, s: &Signed<T>) -> bool {
    keys::verify(pubkey_hex, &canonical(&s.signed), &s.sig)
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let dir = std::env::temp_dir().join(format!("witchy-tuf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let key = RegistryKey::load_or_create(&dir).unwrap();
        let snap = Snapshot {
            version: 3,
            created: 100,
            targets: BTreeMap::from([("a@1.0.0".into(), "sha256:ab".into())]),
        };
        let signed = sign(&key, snap);
        assert!(verify_signed(&key.public_hex(), &signed));
        // Tamper the payload -> signature no longer matches.
        let mut bad = signed;
        bad.signed.version = 4;
        assert!(!verify_signed(&key.public_hex(), &bad));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canonical_is_deterministic() {
        let a = Snapshot {
            version: 1,
            created: 0,
            targets: BTreeMap::from([("z@1".into(), "h1".into()), ("a@1".into(), "h2".into())]),
        };
        let b = a.clone();
        assert_eq!(canonical(&a), canonical(&b));
        assert!(sha256_hex(&canonical(&a)).starts_with("sha256:"));
    }
}
