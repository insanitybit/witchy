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

use aws_lc_rs::encoding::{AsDer, Pkcs8V1Der};
use aws_lc_rs::rsa::{KeyPair as RsaKeyPair, KeySize, PrivateDecryptingKey};
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256};
use serde::{Deserialize, Serialize};

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
// Issuer key (an IdP's RSA signing key — models an OIDC provider's JWKS key)
// ---------------------------------------------------------------------------

/// An issuer (IdP) RSA signing key — a local stand-in for a real OIDC provider (a CI
/// system's OIDC signer, Google's account keys). Lives at `<dir>/root.key` (the PKCS#8
/// private key, hex), with the public key (DER PKCS#1, hex) at `<dir>/root.pub` — exactly
/// what `coven-serve --trust-issuer` registers and `verify_oidc` validates against.
pub struct RegistryKey {
    keypair: RsaKeyPair,
}

impl RegistryKey {
    /// Load the issuer key, generating an RSA-2048 keypair on first use.
    pub fn load_or_create(dir: &Path) -> IdpResult<RegistryKey> {
        let key_path = dir.join("root.key");
        let pkcs8 = if key_path.exists() {
            hex_decode(std::fs::read_to_string(&key_path)?.trim())
                .ok_or_else(|| IdpError("corrupt root.key".into()))?
        } else {
            std::fs::create_dir_all(dir)?;
            let private = PrivateDecryptingKey::generate(KeySize::Rsa2048)
                .map_err(|e| IdpError(format!("rsa keygen: {e}")))?;
            let der: Pkcs8V1Der = private.as_der().map_err(|e| IdpError(format!("rsa serialize: {e}")))?;
            let bytes = der.as_ref().to_vec();
            write_private(&key_path, &bytes)?;
            bytes
        };
        let keypair = RsaKeyPair::from_pkcs8(&pkcs8).map_err(|e| IdpError(format!("rsa load: {e}")))?;
        // The public key (DER PKCS#1, hex) — the form `verify_oidc` consumes.
        std::fs::write(dir.join("root.pub"), hex_encode(keypair.public_key().as_ref()))?;
        Ok(RegistryKey { keypair })
    }

    /// RS256 (RSASSA-PKCS1-v1_5 / SHA-256) signature over `msg`, as raw bytes.
    pub fn sign(&self, msg: &[u8]) -> IdpResult<Vec<u8>> {
        let mut sig = vec![0u8; self.keypair.public_modulus_len()];
        self.keypair
            .sign(&RSA_PKCS1_SHA256, &aws_lc_rs::rand::SystemRandom::new(), msg, &mut sig)
            .map_err(|e| IdpError(format!("rsa sign: {e}")))?;
        Ok(sig)
    }

    pub fn public_hex(&self) -> String {
        hex_encode(self.keypair.public_key().as_ref())
    }
}

fn write_private(path: &Path, der: &[u8]) -> IdpResult<()> {
    std::fs::write(path, hex_encode(der))?;
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

/// Mint a short-lived identity token as a compact RS256 JWT (`header.payload.signature`,
/// each base64url) — the shape a real OIDC provider / CI system emits. The standard claims
/// and the provider claims (`extra`) are FLATTENED to the payload's top level, matching a
/// GitHub Actions token, so the verifier reads `repository` directly.
pub fn mint(issuer_key: &RegistryKey, mut claims: Claims, kid: Option<&str>) -> IdpResult<String> {
    // The `kid` header names which JWKS key signed the token — a rotating provider sets
    // it so a verifier can select the matching key. Static issuers omit it.
    let header = match kid {
        Some(k) => b64url(format!(r#"{{"alg":"RS256","typ":"JWT","kid":"{k}"}}"#).as_bytes()),
        None => b64url(br#"{"alg":"RS256","typ":"JWT"}"#),
    };
    // A unique token id (`jti`) so the registry can enforce single-use — a replayed publish
    // token is refused. A real GitHub Actions OIDC token always carries one; ours does too.
    // (Left untouched if a caller set it explicitly, e.g. a determinism-sensitive test.)
    claims.extra.entry("jti".into()).or_insert_with(|| {
        let bytes: [u8; 16] = aws_lc_rs::rand::generate(&aws_lc_rs::rand::SystemRandom::new())
            .map(|r| r.expose())
            .unwrap_or([0u8; 16]);
        hex_encode(&bytes)
    });
    let payload = b64url(payload_json(&claims).as_bytes());
    let signing_input = format!("{header}.{payload}");
    let sig = issuer_key.sign(signing_input.as_bytes())?;
    Ok(format!("{signing_input}.{}", b64url(&sig)))
}

/// The JWT payload: standard claims plus the provider `extra` claims, all at the top level.
fn payload_json(c: &Claims) -> String {
    let mut obj = serde_json::Map::new();
    obj.insert("iss".into(), c.iss.clone().into());
    obj.insert("sub".into(), c.sub.clone().into());
    obj.insert("aud".into(), c.aud.clone().into());
    obj.insert("exp".into(), c.exp.into());
    obj.insert("iat".into(), c.iat.into());
    for (k, v) in &c.extra {
        obj.insert(k.clone(), serde_json::Value::String(v.clone()));
    }
    serde_json::Value::Object(obj).to_string()
}

/// base64url (no padding) — the JWT segment encoding.
fn b64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((*c.get(1).unwrap_or(&0) as u32) << 8)
            | (*c.get(2).unwrap_or(&0) as u32);
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        if c.len() > 1 {
            out.push(ALPHABET[(n >> 6 & 63) as usize] as char);
        }
        if c.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
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

const VALUE_FLAGS: &[&str] = &["--out", "--issuer-key", "--issuer", "--sub", "--ttl", "--claim", "--kid"];

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
    // A malformed `--ttl` must be a loud error, not a silent fall-back to the
    // default that would mint a token with an unintended lifetime (BUG-212).
    let ttl: u64 = match a.val("--ttl") {
        Some(s) => s
            .parse()
            .map_err(|_| IdpError(format!("--ttl expects a number of seconds, got `{s}`")))?,
        None => 300,
    };
    let now = now_unix();
    let mut extra = BTreeMap::new();
    // A `--claim` without a `key=value` shape (or an empty key) is rejected rather
    // than silently dropped, so an intended claim can never go missing (BUG-212).
    for c in a.vals("--claim") {
        match c.split_once('=') {
            Some((k, v)) if !k.is_empty() => {
                extra.insert(k.to_string(), v.to_string());
            }
            _ => return Err(IdpError(format!("--claim expects key=value, got `{c}`"))),
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
    let token = mint(&key, claims, a.val("--kid"))?;
    println!("{token}");
    Ok(())
}

/// `witchy coven-issuer-jwks --issuer-key DIR [--kid ID]` — emit the issuer's public key
/// as a JWKS document (`{"keys":[{...}]}`), the rotating form a real OIDC provider
/// publishes. `coven-serve --trust-issuer-jwks iss=<file>` consumes this; tokens minted
/// with the matching `--kid` then verify against it.
pub fn issuer_jwks(rest: &[String]) -> IdpResult<()> {
    let a = parse_args(rest);
    let key_dir = a
        .val("--issuer-key")
        .ok_or_else(|| IdpError("--issuer-key <dir> is required".into()))?;
    let key = RegistryKey::load_or_create(Path::new(key_dir))?;
    let kid = a.val("--kid").unwrap_or("key-1");
    let der = hex_decode(&key.public_hex()).ok_or_else(|| IdpError("corrupt public key".into()))?;
    let (n, e) = rsa_pkcs1_parts(&der)?;
    println!(
        r#"{{"keys":[{{"kty":"RSA","use":"sig","alg":"RS256","kid":"{}","n":"{}","e":"{}"}}]}}"#,
        kid,
        b64url(n),
        b64url(e),
    );
    Ok(())
}

/// Split a DER PKCS#1 RSAPublicKey (`SEQUENCE { INTEGER n, INTEGER e }`) into the modulus
/// and exponent bytes, stripping the ASN.1 sign byte so each is the unsigned big-endian
/// form a JWK `n`/`e` carries.
fn rsa_pkcs1_parts(der: &[u8]) -> IdpResult<(&[u8], &[u8])> {
    let bad = || IdpError("malformed RSA public key DER".into());
    let mut i = 0usize;
    if der.first().copied() != Some(0x30) {
        return Err(bad());
    }
    i += 1;
    let (_seq, after_seq) = der_len(der, i).ok_or_else(bad)?;
    i = after_seq;
    let n = der_integer(der, &mut i).ok_or_else(bad)?;
    let e = der_integer(der, &mut i).ok_or_else(bad)?;
    Ok((strip_sign(n), strip_sign(e)))
}

/// Read one DER INTEGER at `*i`, advancing `*i` past it; returns the content bytes.
fn der_integer<'a>(der: &'a [u8], i: &mut usize) -> Option<&'a [u8]> {
    if der.get(*i).copied() != Some(0x02) {
        return None;
    }
    *i += 1;
    let (len, after_len) = der_len(der, *i)?;
    *i = after_len;
    let body = der.get(*i..*i + len)?;
    *i += len;
    Some(body)
}

/// Read an ASN.1 DER length at `i`; returns (length, index just past the length bytes).
fn der_len(der: &[u8], i: usize) -> Option<(usize, usize)> {
    let first = *der.get(i)?;
    if first < 0x80 {
        Some((first as usize, i + 1))
    } else {
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 {
            return None;
        }
        let mut len = 0usize;
        for k in 0..count {
            len = (len << 8) | (*der.get(i + 1 + k)? as usize);
        }
        Some((len, i + 1 + count))
    }
}

/// Strip a leading ASN.1 sign byte (`0x00`) so the integer is the unsigned big-endian form.
fn strip_sign(b: &[u8]) -> &[u8] {
    if b.first() == Some(&0x00) && b.len() > 1 {
        &b[1..]
    } else {
        b
    }
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
    fn minted_token_is_a_verifiable_rs256_jwt() {
        let dir = tmp();
        let key = RegistryKey::load_or_create(&dir).unwrap();
        let claims = Claims {
            iss: "https://token.actions.githubusercontent.com".into(),
            sub: "repo:acme/x".into(),
            aud: AUDIENCE.into(),
            exp: 2_000_000_000,
            iat: 500,
            extra: BTreeMap::from([("repository".into(), "acme/x".into())]),
        };
        let token = mint(&key, claims, None).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "a compact JWT has three segments");

        // The RS256 signature over `header.payload` verifies under the issuer's public key
        // (DER PKCS#1) — the exact check coven_trust performs via verify_oidc.
        let pk_der = hex_decode(&key.public_hex()).unwrap();
        let sig = b64url_decode(parts[2]);
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let pubkey = aws_lc_rs::signature::UnparsedPublicKey::new(
            &aws_lc_rs::signature::RSA_PKCS1_2048_8192_SHA256,
            &pk_der,
        );
        assert!(pubkey.verify(signing_input.as_bytes(), &sig).is_ok(), "JWT signature verifies");

        // The provider claim is FLATTENED to the payload top level (like a real GH token).
        let payload = String::from_utf8(b64url_decode(parts[1])).unwrap();
        assert!(payload.contains("\"repository\":\"acme/x\""), "extra flattened: {payload}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// (BUG-212) `coven-mint-token` must REJECT a malformed `--ttl` (not silently
    /// default to 300) and a `--claim` without a `key=value` shape (not silently
    /// drop it). A well-formed invocation still succeeds.
    #[test]
    fn mint_token_rejects_malformed_ttl_and_claim() {
        let dir = tmp();
        let d = dir.to_str().unwrap().to_string();
        let args = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };

        // Well-formed: a numeric ttl and a key=value claim succeed.
        assert!(
            mint_token(&args(&["--issuer-key", &d, "--sub", "s", "--ttl", "600", "--claim", "repository=acme/x"]))
                .is_ok(),
            "a well-formed mint must succeed"
        );

        // `--ttl bananas` is not a number → error, not a defaulted lifetime.
        let bad_ttl = mint_token(&args(&["--issuer-key", &d, "--sub", "s", "--ttl", "bananas"]));
        assert!(bad_ttl.is_err(), "a non-numeric --ttl must be rejected");
        assert!(format!("{}", bad_ttl.unwrap_err()).contains("--ttl"), "the error names --ttl");

        // `--claim repository` (no `=`) → error, not a silently dropped claim.
        let bad_claim = mint_token(&args(&["--issuer-key", &d, "--sub", "s", "--claim", "repository"]));
        assert!(bad_claim.is_err(), "a --claim without key=value must be rejected");
        assert!(format!("{}", bad_claim.unwrap_err()).contains("--claim"), "the error names --claim");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Decode base64url (no padding) to bytes — for the test to read the JWT's segments.
    fn b64url_decode(s: &str) -> Vec<u8> {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let val = |c: u8| A.iter().position(|&x| x == c).unwrap() as u32;
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            let n = b.len() - i;
            let c0 = val(b[i]);
            let c1 = if n > 1 { val(b[i + 1]) } else { 0 };
            let c2 = if n > 2 { val(b[i + 2]) } else { 0 };
            let c3 = if n > 3 { val(b[i + 3]) } else { 0 };
            let triple = (c0 << 18) | (c1 << 12) | (c2 << 6) | c3;
            out.push((triple >> 16) as u8);
            if n > 2 {
                out.push((triple >> 8) as u8);
            }
            if n > 3 {
                out.push(triple as u8);
            }
            i += 4;
        }
        out
    }
}
