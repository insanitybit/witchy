//! Ed25519 signing for registry metadata — the cryptographic spine of coven's
//! tamper-evidence (the TUF "targets" role, here in local form).
//!
//! Content hashing already makes a rune's *source* tamper-evident. But the
//! *record* that asserts "this hash is the released version, with this
//! footprint" must itself be signed — otherwise an attacker with write access to
//! the registry could swap the source AND rewrite the record's hash to match.
//! Signing the record closes that: the record is signed by a key the attacker
//! does not hold, and the client pins the key's fingerprint (TOFU) so the key
//! cannot be silently swapped either.

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use super::{err, PmResult};

/// The registry's root signing key (private). Lives at `<registry>/root.key`;
/// the matching public key is written alongside at `<registry>/root.pub`.
pub struct RegistryKey {
    signing: SigningKey,
}

impl RegistryKey {
    /// Load the registry's root key, generating one on first use.
    pub fn load_or_create(registry_root: &Path) -> PmResult<RegistryKey> {
        let key_path = registry_root.join("root.key");
        if key_path.exists() {
            let seed = read_seed(&key_path)?;
            let signing = SigningKey::from_bytes(&seed);
            return Ok(RegistryKey { signing });
        }
        std::fs::create_dir_all(registry_root)?;
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| super::PmError(format!("os rng: {e}")))?;
        let signing = SigningKey::from_bytes(&seed);
        write_private(&key_path, &seed)?;
        std::fs::write(
            registry_root.join("root.pub"),
            hex_encode(signing.verifying_key().as_bytes()),
        )?;
        Ok(RegistryKey { signing })
    }

    pub fn sign(&self, msg: &[u8]) -> String {
        hex_encode(&self.signing.sign(msg).to_bytes())
    }

    pub fn public_hex(&self) -> String {
        hex_encode(self.signing.verifying_key().as_bytes())
    }

    /// A short, comparable fingerprint of the public key (for TOFU pinning).
    /// (The registry computes fingerprints via [`fingerprint_of`] over the public
    /// key; this convenience is exercised by tests.)
    #[allow(dead_code)]
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.public_hex())
    }
}

/// Verify `sig_hex` over `msg` under the public key `pub_hex`. Any malformed
/// input is a verification failure, never a panic.
pub fn verify(pub_hex: &str, msg: &[u8], sig_hex: &str) -> bool {
    let Some(pk_bytes) = hex_decode(pub_hex) else {
        return false;
    };
    let Ok(pk_arr): Result<[u8; 32], _> = pk_bytes.try_into() else {
        return false;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&pk_arr) else {
        return false;
    };
    let Some(sig_bytes) = hex_decode(sig_hex) else {
        return false;
    };
    let Ok(sig_arr): Result<[u8; 64], _> = sig_bytes.try_into() else {
        return false;
    };
    let sig = Signature::from_bytes(&sig_arr);
    vk.verify(msg, &sig).is_ok()
}

/// Fingerprint of a hex-encoded public key.
pub fn fingerprint_of(pub_hex: &str) -> String {
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

fn read_seed(path: &Path) -> PmResult<[u8; 32]> {
    let text = std::fs::read_to_string(path)?;
    let bytes = hex_decode(text.trim()).ok_or_else(|| super::PmError("corrupt root.key".into()))?;
    bytes
        .try_into()
        .map_err(|_| super::PmError("root.key is not a 32-byte seed".into()))
}

fn write_private(path: &PathBuf, seed: &[u8; 32]) -> PmResult<()> {
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

/// Read a registry's pinned public key (hex) from `<registry>/root.pub`.
pub fn read_public(registry_root: &Path) -> PmResult<String> {
    let p = registry_root.join("root.pub");
    let text = std::fs::read_to_string(&p)
        .map_err(|_| super::PmError("registry has no root.pub (no signed releases yet)".into()))?;
    let t = text.trim().to_string();
    if hex_decode(&t).map(|b| b.len() == 32).unwrap_or(false) {
        Ok(t)
    } else {
        err("registry root.pub is malformed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CTR: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "witchy-keys-{}-{}-{}",
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
    fn sign_and_verify_roundtrip() {
        let dir = tmp();
        let key = RegistryKey::load_or_create(&dir).unwrap();
        let sig = key.sign(b"hello");
        assert!(verify(&key.public_hex(), b"hello", &sig));
        // Tampered message fails.
        assert!(!verify(&key.public_hex(), b"hell0", &sig));
        // Wrong signature fails.
        assert!(!verify(&key.public_hex(), b"hello", "00"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_is_persistent() {
        let dir = tmp();
        let a = RegistryKey::load_or_create(&dir).unwrap().public_hex();
        let b = RegistryKey::load_or_create(&dir).unwrap().public_hex();
        assert_eq!(a, b, "reloading must yield the same key");
        assert_eq!(read_public(&dir).unwrap(), a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_is_stable_and_prefixed() {
        let dir = tmp();
        let key = RegistryKey::load_or_create(&dir).unwrap();
        assert!(key.fingerprint().starts_with("ed25519:"));
        assert_eq!(key.fingerprint(), fingerprint_of(&key.public_hex()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn different_key_different_signature() {
        let d1 = tmp();
        let d2 = tmp();
        let k1 = RegistryKey::load_or_create(&d1).unwrap();
        let k2 = RegistryKey::load_or_create(&d2).unwrap();
        let sig = k1.sign(b"msg");
        assert!(!verify(&k2.public_hex(), b"msg", &sig), "k2 must not verify k1's sig");
        let _ = std::fs::remove_dir_all(&d1);
        let _ = std::fs::remove_dir_all(&d2);
    }
}
