//! Native standard-library modules — functions implemented in Rust because they
//! cannot be expressed in witchy itself (cryptography today; encoding, etc.
//! later). This is the *trusted, compiled-in* native set:
//!
//! - reached only through their module (`crypto.sha256`), never as a global
//!   builtin;
//! - **pure and stateless** — capability-gated host I/O (`read`, `now`, …) is
//!   not here; it lives in the interpreter, which threads host state;
//! - interpreter-only (native Rust can't run in the WASM sandbox tier).
//!
//! This is the static half of "Rust modules": adding a native stdlib function is
//! a registration here plus a typed signature in the module's `.witchy` stub —
//! no editing of the interpreter's builtin match. A future, explicitly
//! capability-gated `Native` capability would add *dynamically*-loaded FFI on
//! top of this registry.

use crate::value::{NativeError as RuntimeError, NativeValue as Value};
use witchy_syntax::intrinsics;

/// A native module function: pure and stateless, `(args) -> value`.
pub type NativeFn = fn(&[Value]) -> Result<Value, RuntimeError>;

/// Resolve a native-module function by its qualified name (`crypto.sha256`), or
/// `None` if `qualified` is not a native-module function.
pub fn lookup(qualified: &str) -> Option<NativeFn> {
    if intrinsics::lookup(qualified)
        .is_some_and(|spec| spec.runtime != intrinsics::IntrinsicRuntime::Native)
    {
        return None;
    }
    match qualified {
        "crypto.sha256" => Some(crypto::sha256),
        "crypto.rune_hash" => Some(crypto::rune_hash),
        "crypto.__ed25519_verify_status" => Some(crypto::ed25519_verify_status),
        "crypto.sign" => Some(crypto::sign),
        "crypto.public_key" => Some(crypto::public_key),
        "crypto.reveal" => Some(crypto::reveal),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.__ecdsa_p256_verify_status" => Some(crypto::ecdsa_p256_verify_status),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.__ecdsa_p256_verify_hex_status" => Some(crypto::ecdsa_p256_verify_hex_status),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.__rsa_pkcs1_sha256_verify_status" => Some(crypto::rsa_pkcs1_sha256_verify_status),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.sha512" => Some(crypto::sha512),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.sha3_256" => Some(crypto::sha3_256),
        #[cfg(not(target_arch = "wasm32"))]
        "crypto.hmac_sha256" => Some(crypto::hmac_sha256),
        intrinsics::COMPILER_FOOTPRINT => Some(compiler::footprint),
        intrinsics::COMPILER_DIFF => Some(compiler::diff),
        intrinsics::COMPILER_DOC => Some(compiler::doc),
        intrinsics::COMPILER_DOC_RESULT_JSON => Some(compiler::doc_result_json),
        intrinsics::ENCODING_UTF8_LOSSY => Some(encoding::utf8_lossy),
        intrinsics::ENCODING_HEX_ENCODE => Some(encoding::hex_encode),
        intrinsics::ENCODING_HEX_ENCODE_BYTES => Some(encoding::hex_encode_bytes),
        intrinsics::ENCODING_HEX_DECODE_LOSSY => Some(encoding::hex_decode_lossy),
        intrinsics::ENCODING_HEX_DECODE_BYTES_RAW => Some(encoding::hex_decode_bytes_raw),
        intrinsics::ENCODING_BASE64_ENCODE => Some(encoding::base64_encode),
        intrinsics::ENCODING_BASE64_ENCODE_BYTES => Some(encoding::base64_encode_bytes),
        intrinsics::ENCODING_BASE64URL_ENCODE_BYTES => Some(encoding::base64url_encode_bytes),
        intrinsics::ENCODING_HEX_TO_BASE64URL_LOSSY => Some(encoding::hex_to_base64url),
        intrinsics::ENCODING_BASE64_DECODE_LOSSY => Some(encoding::base64_decode_lossy),
        intrinsics::ENCODING_BASE64_DECODE_BYTES_RAW => Some(encoding::base64_decode_bytes_raw),
        intrinsics::ENCODING_BASE64URL_DECODE_LOSSY => Some(encoding::base64url_decode_lossy),
        intrinsics::ENCODING_BASE64URL_DECODE_BYTES_RAW => Some(encoding::base64url_decode_bytes_raw),
        intrinsics::ENCODING_BASE64URL_TO_HEX_LOSSY => Some(encoding::base64url_to_hex_lossy),
        "regex.match_spans" => Some(regexp::match_spans),
        intrinsics::STRING_FROM_CODE => Some(string::from_code),
        _ => None,
    }
}

fn type_error(msg: impl Into<String>) -> RuntimeError {
    RuntimeError { message: msg.into() }
}

/// The `crypto` module: hashing and signatures (sha2 / ed25519-dalek).
mod crypto {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;

    /// SHA-256 of a string's UTF-8 bytes, as 64 lowercase hex characters.
    pub fn sha256(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("crypto.sha256 expects a String"));
        };
        Ok(Value::Str(hex(&sha256_digest(s.as_bytes()))))
    }

    /// The canonical content hash of a rune's source tree — the package manager's
    /// content address. `paths` and `contents` are parallel lists, one entry per
    /// file (`witchy.toml` plus each `src/**/*.witchy`). Entries are sorted by
    /// path and each content is LF-normalized, then SHA-256'd with a u64 length
    /// prefix on *every* field so no concatenation is ambiguous — byte-identical
    /// to the store's hashing (`src/pm/store.rs`). Returns `sha256:<hex>`.
    pub fn rune_hash(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::List(paths), Value::List(contents)] = args else {
            return Err(type_error(
                "crypto.rune_hash expects (List(String), List(String))",
            ));
        };
        if paths.len() != contents.len() {
            return Err(type_error(
                "crypto.rune_hash: paths and contents differ in length",
            ));
        }
        let mut files: Vec<(&str, Vec<u8>)> = Vec::with_capacity(paths.len());
        for (p, c) in paths.iter().zip(contents.iter()) {
            let (Value::Str(path), Value::Str(content)) = (p, c) else {
                return Err(type_error("crypto.rune_hash: entries must be strings"));
            };
            files.push((path.as_str(), normalize_lf(content.as_bytes())));
        }
        files.sort_by(|a, b| a.0.cmp(b.0));
        let mut buf = Vec::new();
        for (path, bytes) in &files {
            buf.extend_from_slice(&(path.len() as u64).to_le_bytes());
            buf.extend_from_slice(path.as_bytes());
            buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
        Ok(Value::Str(format!("sha256:{}", hex(&sha256_digest(&buf)))))
    }

    /// LF-normalize so a CRLF checkout hashes identically (matches the store).
    fn normalize_lf(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                out.push(b'\n');
                i += 2;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    /// Verify an Ed25519 signature. Status codes: 1 valid, 0 invalid signature,
    /// -1 malformed public key, -3 malformed signature.
    pub fn ed25519_verify_status(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pk_hex), Value::Str(msg), Value::Str(sig_hex)] = args else {
            return Err(type_error(
                "crypto.__ed25519_verify_status expects (pubkey_hex, message, sig_hex) strings",
            ));
        };
        let Some(pk) = hex_decode(pk_hex) else {
            return Ok(Value::Int(-1));
        };
        if pk.len() != 32 {
            return Ok(Value::Int(-1));
        }
        let Some(sig) = hex_decode(sig_hex) else {
            return Ok(Value::Int(-3));
        };
        if sig.len() != 64 {
            return Ok(Value::Int(-3));
        }
        Ok(Value::Int(ed25519_verify_raw(&pk, msg.as_bytes(), &sig) as i64))
    }

    /// Normalize a secret's raw bytes to a 32-byte Ed25519 seed: accept the seed
    /// directly (32 bytes) or hex-encoded (64 chars). Anything else (e.g. a value
    /// secret like a token) is not a signing key.
    fn seed32(bytes: &[u8]) -> Result<[u8; 32], RuntimeError> {
        let raw = if bytes.len() == 32 {
            bytes.to_vec()
        } else if bytes.len() == 64 {
            hex_decode(&String::from_utf8_lossy(bytes))
                .ok_or_else(|| type_error("secret is not a valid hex Ed25519 seed"))?
        } else {
            return Err(type_error(
                "secret is not a signing key (need a 32-byte seed or 64 hex chars)",
            ));
        };
        raw.try_into()
            .map_err(|_| type_error("secret is not a 32-byte Ed25519 seed"))
    }

    /// Sign `message` with a `Secret`, returning the hex signature.
    pub fn sign(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Secret(bytes), Value::Str(msg)] = args else {
            return Err(type_error("crypto.sign expects (Secret, message)"));
        };
        Ok(Value::Str(hex(&ed25519_sign_raw(&seed32(bytes)?, msg.as_bytes()))))
    }

    /// The hex-encoded Ed25519 public key for a `Secret` — what a verifier checks
    /// signatures against (safe to publish).
    pub fn public_key(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Secret(bytes)] = args else {
            return Err(type_error("crypto.public_key expects a Secret"));
        };
        Ok(Value::Str(hex(&ed25519_public_raw(&seed32(bytes)?))))
    }

    /// Reveal a `Secret`'s raw bytes as a string — for value secrets (tokens,
    /// passwords) that must be handed to an external sink.
    pub fn reveal(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Secret(bytes)] = args else {
            return Err(type_error("crypto.reveal expects a Secret"));
        };
        Ok(Value::Str(String::from_utf8_lossy(bytes).into_owned()))
    }

    /// Verify an ECDSA P-256 / SHA-256 ("ES256", WebAuthn COSE alg -7) signature.
    /// `public_key`/`signature` are hex (a SEC1 uncompressed point `04||x||y`; an
    /// ASN.1-DER signature); `message` is the raw bytes the signature covers (the
    /// curve hashes it with SHA-256). Malformed input is a negative status; a bad
    /// signature is 0. Native-only: aws-lc-rs has no wasm32 build.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn ecdsa_p256_verify_status(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pk_hex), Value::Str(msg), Value::Str(sig_hex)] = args else {
            return Err(type_error(
                "crypto.__ecdsa_p256_verify_status expects (pubkey_hex, message, sig_hex) strings",
            ));
        };
        let Some(pk) = hex_decode(pk_hex) else {
            return Ok(Value::Int(-1));
        };
        if !is_sec1_p256_public_key(&pk) {
            return Ok(Value::Int(-1));
        }
        let Some(sig) = hex_decode(sig_hex) else {
            return Ok(Value::Int(-3));
        };
        if !is_der_ecdsa_signature(&sig) {
            return Ok(Value::Int(-3));
        }
        use aws_lc_rs::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
        Ok(Value::Int(
            UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, pk)
                .verify(msg.as_bytes(), &sig)
                .is_ok() as i64,
        ))
    }

    /// Verify an RSASSA-PKCS1-v1_5 / SHA-256 signature — JWT/OIDC "RS256". The
    /// `public_key` is the hex of a DER-encoded RSA public key (PKCS#1 `RSAPublicKey`);
    /// `signature` is hex; `message` is the raw signed bytes. Malformed input is a
    /// negative status; a bad signature is 0. (Native/aws-lc only.)
    #[cfg(not(target_arch = "wasm32"))]
    pub fn rsa_pkcs1_sha256_verify_status(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pk_hex), Value::Str(msg), Value::Str(sig_hex)] = args else {
            return Err(type_error(
                "crypto.__rsa_pkcs1_sha256_verify_status expects (pubkey_der_hex, message, sig_hex) strings",
            ));
        };
        let Some(pk) = hex_decode(pk_hex) else {
            return Ok(Value::Int(-1));
        };
        if !is_rsa_pkcs1_public_key(&pk) {
            return Ok(Value::Int(-1));
        }
        let Some(sig) = hex_decode(sig_hex) else {
            return Ok(Value::Int(-3));
        };
        if sig.is_empty() {
            return Ok(Value::Int(-3));
        }
        use aws_lc_rs::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
        Ok(Value::Int(
            UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, pk)
                .verify(msg.as_bytes(), &sig)
                .is_ok() as i64,
        ))
    }

    /// Like `ecdsa_p256_verify` but the message is also hex — for binary messages
    /// such as WebAuthn's `authenticatorData ‖ SHA256(clientDataJSON)`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn ecdsa_p256_verify_hex_status(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pk_hex), Value::Str(msg_hex), Value::Str(sig_hex)] = args else {
            return Err(type_error(
                "crypto.__ecdsa_p256_verify_hex_status expects (pubkey_hex, message_hex, sig_hex) strings",
            ));
        };
        let Some(pk) = hex_decode(pk_hex) else {
            return Ok(Value::Int(-1));
        };
        if !is_sec1_p256_public_key(&pk) {
            return Ok(Value::Int(-1));
        }
        let Some(msg) = hex_decode(msg_hex) else {
            return Ok(Value::Int(-2));
        };
        let Some(sig) = hex_decode(sig_hex) else {
            return Ok(Value::Int(-3));
        };
        if !is_der_ecdsa_signature(&sig) {
            return Ok(Value::Int(-3));
        }
        use aws_lc_rs::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_ASN1};
        Ok(Value::Int(
            UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, pk)
                .verify(&msg, &sig)
                .is_ok() as i64,
        ))
    }

    /// SHA-512 (FIPS 180-4) of a string's UTF-8 bytes, as 128 lowercase hex chars.
    /// Native-only (aws-lc-rs; not WASM-bridged).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn sha512(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("crypto.sha512 expects a String"));
        };
        let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA512, s.as_bytes());
        Ok(Value::Str(hex(d.as_ref())))
    }

    /// SHA3-256 (FIPS 202) of a string's UTF-8 bytes, as 64 lowercase hex chars.
    /// Native-only (aws-lc-rs; not WASM-bridged).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn sha3_256(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("crypto.sha3_256 expects a String"));
        };
        let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA3_256, s.as_bytes());
        Ok(Value::Str(hex(d.as_ref())))
    }

    /// HMAC-SHA256 (FIPS 198-1). `key` is hex (so binary keys are representable),
    /// `message` is raw bytes; returns the 64-hex-char tag. Native-only.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn hmac_sha256(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(key_hex), Value::Str(msg)] = args else {
            return Err(type_error("crypto.hmac_sha256 expects (key_hex, message) strings"));
        };
        let key_bytes =
            hex_decode(key_hex).ok_or_else(|| type_error("crypto.hmac_sha256: key is not valid hex"))?;
        use aws_lc_rs::hmac;
        let key = hmac::Key::new(hmac::HMAC_SHA256, &key_bytes);
        let tag = hmac::sign(&key, msg.as_bytes());
        Ok(Value::Str(hex(tag.as_ref())))
    }

    fn hex(bytes: &[u8]) -> String {
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
            .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
            .collect()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn is_sec1_p256_public_key(pk: &[u8]) -> bool {
        pk.len() == 65 && pk.first() == Some(&0x04)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn der_len(data: &[u8], pos: &mut usize) -> Option<usize> {
        let first = *data.get(*pos)?;
        *pos += 1;
        if first & 0x80 == 0 {
            return Some(first as usize);
        }
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 4 || *pos + count > data.len() {
            return None;
        }
        let mut len = 0usize;
        for _ in 0..count {
            len = len.checked_mul(256)?.checked_add(*data.get(*pos)? as usize)?;
            *pos += 1;
        }
        if len < 128 {
            return None;
        }
        Some(len)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn der_tlv<'a>(data: &'a [u8], pos: &mut usize, tag: u8) -> Option<&'a [u8]> {
        if *data.get(*pos)? != tag {
            return None;
        }
        *pos += 1;
        let len = der_len(data, pos)?;
        let end = pos.checked_add(len)?;
        let out = data.get(*pos..end)?;
        *pos = end;
        Some(out)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn der_positive_int(data: &[u8]) -> bool {
        if data.is_empty() {
            return false;
        }
        if data[0] & 0x80 != 0 {
            return false;
        }
        if data.len() > 1 && data[0] == 0 && data[1] & 0x80 == 0 {
            return false;
        }
        true
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn is_der_ecdsa_signature(sig: &[u8]) -> bool {
        let mut pos = 0;
        let Some(seq) = der_tlv(sig, &mut pos, 0x30) else {
            return false;
        };
        if pos != sig.len() {
            return false;
        }
        let mut inner = 0;
        let Some(r) = der_tlv(seq, &mut inner, 0x02) else {
            return false;
        };
        let Some(s) = der_tlv(seq, &mut inner, 0x02) else {
            return false;
        };
        inner == seq.len() && der_positive_int(r) && der_positive_int(s)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn is_rsa_pkcs1_public_key(der: &[u8]) -> bool {
        let mut pos = 0;
        let Some(seq) = der_tlv(der, &mut pos, 0x30) else {
            return false;
        };
        if pos != der.len() {
            return false;
        }
        let mut inner = 0;
        let Some(n) = der_tlv(seq, &mut inner, 0x02) else {
            return false;
        };
        let Some(e) = der_tlv(seq, &mut inner, 0x02) else {
            return false;
        };
        if inner != seq.len() || !der_positive_int(n) || !der_positive_int(e) {
            return false;
        }
        let modulus = if n.first() == Some(&0) { &n[1..] } else { n };
        if !(256..=1024).contains(&modulus.len()) || e.len() > 8 {
            return false;
        }
        let mut exp = 0u64;
        for b in e {
            exp = match exp.checked_mul(256).and_then(|v| v.checked_add(*b as u64)) {
                Some(v) => v,
                None => return false,
            };
        }
        exp > 1 && exp % 2 == 1
    }

    // Crypto primitives. On native they are FIPS-approved algorithms via aws-lc-rs;
    // the wasm playground (which cannot build aws-lc-rs and runs no security-critical
    // crypto) keeps the pure-Rust path. Outputs are byte-identical — both implement
    // FIPS 180-4 SHA-256 and RFC 8032 Ed25519 deterministically.

    #[cfg(not(target_arch = "wasm32"))]
    fn sha256_digest(data: &[u8]) -> [u8; 32] {
        let d = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, data);
        let mut out = [0u8; 32];
        out.copy_from_slice(d.as_ref());
        out
    }
    #[cfg(target_arch = "wasm32")]
    fn sha256_digest(data: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(data).into()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ed25519_verify_raw(pk: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        use aws_lc_rs::signature::{UnparsedPublicKey, ED25519};
        UnparsedPublicKey::new(&ED25519, pk).verify(msg, sig).is_ok()
    }
    #[cfg(target_arch = "wasm32")]
    fn ed25519_verify_raw(pk: &[u8], msg: &[u8], sig: &[u8]) -> bool {
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        (|| {
            let pk: [u8; 32] = pk.try_into().ok()?;
            let vk = VerifyingKey::from_bytes(&pk).ok()?;
            let sig: [u8; 64] = sig.try_into().ok()?;
            Some(vk.verify(msg, &Signature::from_bytes(&sig)).is_ok())
        })()
        .unwrap_or(false)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ed25519_sign_raw(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
        use aws_lc_rs::signature::Ed25519KeyPair;
        let kp = Ed25519KeyPair::from_seed_unchecked(seed).expect("32-byte ed25519 seed");
        let mut out = [0u8; 64];
        out.copy_from_slice(kp.sign(msg).as_ref());
        out
    }
    #[cfg(target_arch = "wasm32")]
    fn ed25519_sign_raw(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
        use ed25519_dalek::{Signer, SigningKey};
        SigningKey::from_bytes(seed).sign(msg).to_bytes()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn ed25519_public_raw(seed: &[u8; 32]) -> [u8; 32] {
        use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair};
        let kp = Ed25519KeyPair::from_seed_unchecked(seed).expect("32-byte ed25519 seed");
        let mut out = [0u8; 32];
        out.copy_from_slice(kp.public_key().as_ref());
        out
    }
    #[cfg(target_arch = "wasm32")]
    fn ed25519_public_raw(seed: &[u8; 32]) -> [u8; 32] {
        use ed25519_dalek::SigningKey;
        *SigningKey::from_bytes(seed).verifying_key().as_bytes()
    }
}

/// The `compiler` module: witchy's own toolchain, exposed to witchy. This is what
/// lets a (self-hosted) package manager compute a rune's capability footprint —
/// the heart of the supply-chain story — from within witchy.
mod compiler {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;
    use witchy_syntax::ast::{Item, Module};
    use witchy_syntax::intrinsics;

    /// Compute the capability footprint of witchy `source`, returned as JSON:
    /// `{"total":[..],"build":[..],"user_caps":[..],"entries":[{"name":..,"capabilities":[..],"brands":[..]}]}`,
    /// or `{"error":".."}` if the source does not parse. `build` is the build-time
    /// footprint — the build capabilities (`BuildOut`/`BuildRead`/…) the rune's
    /// `build` entrypoint demands, which a build tool gates separately from the
    /// runtime `total`. Pairs with `std/json`.
    pub fn footprint(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(src)] = args else {
            return Err(type_error("compiler.footprint expects a String"));
        };
        let json = match checked_source_only_module(src, "compiler.footprint") {
            Ok(module) => {
                let fp = witchy_caps::capabilities::analyze(&module);
                let total = arr(fp.total.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                let build = arr(fp.build.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                // RFC-0038: the grantable user-capability axis (bare policy tokens).
                let user_caps = arr(fp.user_caps.iter().cloned());
                let entries: Vec<String> = fp
                    .entries
                    .iter()
                    .map(|e| {
                        let caps =
                            arr(e.capabilities.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                        let brands = arr(e.brands.iter().cloned());
                        format!(
                            "{{\"name\":{},\"capabilities\":{},\"brands\":{}}}",
                            string(&e.name),
                            caps,
                            brands
                        )
                    })
                    .collect();
                format!(
                    "{{\"total\":{},\"build\":{},\"user_caps\":{},\"entries\":[{}]}}",
                    total,
                    build,
                    user_caps,
                    entries.join(",")
                )
            }
            Err(e) => format!("{{\"error\":{}}}", string(&e.to_string())),
        };
        Ok(Value::Str(json))
    }

    /// Render witchy `source` to Markdown API documentation — the same output as the
    /// `witchy doc` CLI: the module's public types, traits, trait implementations,
    /// and functions with their signatures and doc-comments. `name` titles the module
    /// heading. Lets a registry generate browsable docs from a rune's stored source,
    /// on either backend. This native
    /// source-string entry point deliberately rejects `comptime:` sources rather
    /// than rendering an under-approximation; the CLI expands source files before
    /// rendering. A parse/expansion-boundary error is returned as an HTML comment
    /// rather than trapping.
    pub fn doc(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(name), Value::Str(src)] = args else {
            return Err(type_error("compiler.doc expects (name, source) strings"));
        };
        let md = render_doc_result(name, src, "compiler.doc")
            .unwrap_or_else(|e| format!("<!-- doc error: {e} -->"));
        Ok(Value::Str(md))
    }

    /// Fallible docs as inspectable JSON for `compiler.try_doc`:
    /// `{"ok":"markdown"}` or `{"error":"message"}`.
    pub fn doc_result_json(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(name), Value::Str(src)] = args else {
            return Err(type_error(format!(
                "{} expects (name, source) strings",
                intrinsics::COMPILER_DOC_RESULT_JSON
            )));
        };
        let json = match render_doc_result(name, src, "compiler.try_doc") {
            Ok(md) => format!("{{\"ok\":{}}}", string(&md)),
            Err(e) => format!("{{\"error\":{}}}", string(&e)),
        };
        Ok(Value::Str(json))
    }

    /// Compare two witchy sources by capability footprint, as JSON:
    /// `{"widened":bool,"added":[..],"removed":[..],"build_added":[..],
    /// "build_removed":[..],"user_caps_added":[..],"user_caps_removed":[..]}` —
    /// the rights-precise block-on-widening gate (the package manager's core
    /// safety check), exposed to witchy. `{"error":".."}` if either source does
    /// not parse.
    pub fn diff(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(old_src), Value::Str(new_src)] = args else {
            return Err(type_error("compiler.diff expects (old_source, new_source) strings"));
        };
        let json = match (
            checked_source_only_module(old_src, "compiler.diff"),
            checked_source_only_module(new_src, "compiler.diff"),
        ) {
            (Ok(old), Ok(new)) => {
                let old_fp = witchy_caps::capabilities::analyze(&old);
                let new_fp = witchy_caps::capabilities::analyze(&new);
                let d = witchy_caps::capabilities::diff(&old_fp, &new_fp);
                let added = arr(d.added.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                let removed = arr(d.removed.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                let build_added = arr(d.build_added.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                let build_removed = arr(d.build_removed.iter().map(|(n, r)| witchy_caps::capabilities::show_cap(n, r)));
                let user_caps_added = arr(d.user_caps_added.iter().cloned());
                let user_caps_removed = arr(d.user_caps_removed.iter().cloned());
                format!(
                    "{{\"widened\":{},\"added\":{},\"removed\":{},\"build_added\":{},\"build_removed\":{},\"user_caps_added\":{},\"user_caps_removed\":{}}}",
                    d.widened(),
                    added,
                    removed,
                    build_added,
                    build_removed,
                    user_caps_added,
                    user_caps_removed
                )
            }
            (Err(e), _) | (_, Err(e)) => format!("{{\"error\":{}}}", string(&e.to_string())),
        };
        Ok(Value::Str(json))
    }

    fn parse_source_only_module(src: &str, op: &str) -> Result<Module, String> {
        let module = witchy_syntax::parser::parse_module(src).map_err(|e| e.to_string())?;
        if module.items.iter().any(|item| matches!(item, Item::Comptime(_))) {
            return Err(format!(
                "{op} does not support comptime source strings; use the source-file CLI path for expanded introspection"
            ));
        }
        Ok(module)
    }

    fn checked_source_only_module(src: &str, op: &str) -> Result<Module, String> {
        use std::collections::{HashSet, VecDeque};

        fn reject_comptime(module: &Module, op: &str) -> Result<(), String> {
            if module.items.iter().any(|item| matches!(item, Item::Comptime(_))) {
                Err(format!(
                    "{op} does not support comptime source strings; use the source-file CLI path for expanded introspection"
                ))
            } else {
                Ok(())
            }
        }

        fn no_comptime_expand(
            _name: &str,
            module: &mut Module,
            _siblings: &[(String, Module)],
        ) -> Result<(), String> {
            reject_comptime(module, "compiler source-string introspection")
        }

        let entry = parse_source_only_module(src, op)?;
        let mut modules: Vec<(String, Module)> = vec![("main".to_string(), entry.clone())];
        let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
        let mut queue: VecDeque<Module> = VecDeque::from([entry.clone()]);
        while let Some(module) = queue.pop_front() {
            for name in module.imports.clone() {
                if !loaded.insert(name.clone()) {
                    continue;
                }
                let Some(source) = witchy_syntax::linker::std_source(&name) else {
                    if let Some(suggestion) = witchy_syntax::linker::closest_std_module(&name) {
                        return Err(format!(
                            "unknown module `{name}` — did you mean `import {suggestion}`?"
                        ));
                    }
                    // Source-string introspection has no caller-provided module map.
                    // Project tools such as pm feed it one file at a time, so a
                    // non-std import is treated as a project-local module and keeps
                    // the historical source-only footprint shape. Source-file CLI
                    // gates own strict multi-module validation.
                    return Ok(entry);
                };
                let parsed = parse_source_only_module(source, op)?;
                queue.push_back(parsed.clone());
                modules.push((name, parsed));
            }
        }

        let linked = witchy_syntax::linker::link(modules, "main", no_comptime_expand)
            .map_err(|e| e.to_string())?;
        witchy_types::typeck::check(&linked).map_err(|e| e.to_string())?;
        // The public source-string footprint is the submitted module's footprint,
        // not the linked program's transitive std footprint. Linking/type-checking
        // above is the validity gate; analyzing `entry` preserves the established
        // JSON shape and unqualified entry names (`load`, not `main.load`).
        Ok(entry)
    }

    fn render_doc_result(name: &str, src: &str, op: &str) -> Result<String, String> {
        parse_source_only_module(src, op).and_then(|module| witchy_syntax::doc::render_module(name, src, &module))
    }

    /// A JSON string literal (quoted, with `"` and `\` escaped).
    fn string(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// A JSON array of strings.
    fn arr(items: impl Iterator<Item = String>) -> String {
        let parts: Vec<String> = items.map(|s| string(&s)).collect();
        format!("[{}]", parts.join(","))
    }
}

/// The `encoding` module: hex and base64 over strings or raw `Bytes`. The
/// byte-level codecs are native because witchy source cannot inspect raw bytes
/// without going through `std/bytes`. The `*_lossy` decoders are compatibility
/// text helpers; the `*_bytes_raw` decoders are the lossless binary primitives
/// guarded by validating `std/encoding.witchy` wrappers.
mod encoding {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;

    /// (bytes parity) `bytes.to_string`'s lossy UTF-8 normalization. The host has
    /// already read the input buffer through `String::from_utf8_lossy` (the SAME
    /// decode the interpreter's `bytes.to_string` applies — invalid sequences ->
    /// U+FFFD), so this is the identity on the decoded String; the effect is
    /// entirely the host read plus the `3*len` worst-case guest buffer the
    /// `$bytes_to_string` helper reserves. Compiled `bytes.to_string` used to be a
    /// raw identity that returned invalid bytes verbatim, diverging.
    pub fn utf8_lossy(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.utf8_lossy expects a String"));
        };
        Ok(Value::Str(s.clone()))
    }

    fn hex_string(bytes: &[u8]) -> String {
        use std::fmt::Write;
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            let _ = write!(out, "{b:02x}");
        }
        out
    }

    /// Lowercase hex of the input's UTF-8 bytes.
    pub fn hex_encode(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.hex_encode expects a String"));
        };
        Ok(Value::Str(hex_string(s.as_bytes())))
    }

    /// Lowercase hex of raw bytes.
    pub fn hex_encode_bytes(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Bytes(bytes)] = args else {
            return Err(type_error("encoding.hex_encode_bytes expects Bytes"));
        };
        Ok(Value::Str(hex_string(bytes)))
    }

    /// Strict hex → bytes: an even count of `0-9a-fA-F` digits, or `None` for any
    /// odd length or non-hex character. The ONE hex-to-bytes decoder for the
    /// encoding module — it never silently drops or truncates on bad input, so a
    /// caller reaching a raw byte-level primitive with malformed hex fails closed
    /// rather than receiving mangled crypto material. (BUG-276)
    fn hex_bytes(s: &str) -> Option<Vec<u8>> {
        if !s.len().is_multiple_of(2) {
            return None;
        }
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
            .collect()
    }

    /// Raw hex decode to text (lossy UTF-8 only for the *decoded* bytes, which may
    /// be arbitrary binary). The alphabet is decoded STRICTLY — a non-hex character
    /// or odd length is a loud error, never a silent drop. This is the byte-level
    /// primitive behind `encoding.hex_decode`, which validates first, so valid
    /// input is unaffected; a direct malformed call now fails closed. (BUG-276)
    pub fn hex_decode_lossy(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.hex_decode_lossy expects a String"));
        };
        let bytes = hex_bytes(s)
            .ok_or_else(|| type_error("encoding.hex_decode: input is not valid hex"))?;
        Ok(Value::Str(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Raw hex decode to bytes. Public witchy code reaches this through
    /// `encoding.hex_decode_bytes`, which validates before calling the primitive.
    pub fn hex_decode_bytes_raw(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.hex_decode_bytes_raw expects a String"));
        };
        let bytes = hex_bytes(s)
            .ok_or_else(|| type_error("encoding.hex_decode_bytes: input is not valid hex"))?;
        Ok(Value::Bytes(bytes))
    }

    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    fn base64_string(bytes: &[u8], alphabet: &[u8; 64], padding: bool) -> String {
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b0 = chunk[0] as u32;
            let b1 = *chunk.get(1).unwrap_or(&0) as u32;
            let b2 = *chunk.get(2).unwrap_or(&0) as u32;
            let n = (b0 << 16) | (b1 << 8) | b2;
            out.push(alphabet[(n >> 18 & 63) as usize] as char);
            out.push(alphabet[(n >> 12 & 63) as usize] as char);
            if chunk.len() > 1 {
                out.push(alphabet[(n >> 6 & 63) as usize] as char);
            } else if padding {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(alphabet[(n & 63) as usize] as char);
            } else if padding {
                out.push('=');
            }
        }
        out
    }

    /// base64url (no padding; `-`/`_`) of the bytes given as a HEX string. The hex
    /// indirection lets binary round-trip through witchy's UTF-8 strings — e.g. a
    /// WebAuthn `clientDataJSON.challenge` is base64url of the raw challenge bytes.
    /// The hex is decoded STRICTLY (BUG-276): a non-hex character or odd length is a
    /// loud error, never the silent-drop the old `filter_map` codec did.
    pub fn hex_to_base64url(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(hexs)] = args else {
            return Err(type_error("encoding.hex_to_base64url_lossy expects a hex String"));
        };
        let bytes = hex_bytes(hexs)
            .ok_or_else(|| type_error("encoding.hex_to_base64url: input is not valid hex"))?;
        Ok(Value::Str(base64_string(&bytes, B64URL, false)))
    }

    /// Standard base64 (with `=` padding) of the input's UTF-8 bytes.
    pub fn base64_encode(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64_encode expects a String"));
        };
        Ok(Value::Str(base64_string(s.as_bytes(), B64, true)))
    }

    /// Standard base64 (with `=` padding) of raw bytes.
    pub fn base64_encode_bytes(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Bytes(bytes)] = args else {
            return Err(type_error("encoding.base64_encode_bytes expects Bytes"));
        };
        Ok(Value::Str(base64_string(bytes, B64, true)))
    }

    /// base64url (no padding) of raw bytes.
    pub fn base64url_encode_bytes(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Bytes(bytes)] = args else {
            return Err(type_error("encoding.base64url_encode_bytes expects Bytes"));
        };
        Ok(Value::Str(base64_string(bytes, B64URL, false)))
    }

    /// Raw standard-base64 decode to text (lossy UTF-8). Padding and whitespace
    /// are tolerated; a non-alphabet byte stops decoding. Byte-level primitive
    /// behind the validating `encoding.base64_decode` wrapper.
    pub fn base64_decode_lossy(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64_decode_lossy expects a String"));
        };
        Ok(Value::Str(String::from_utf8_lossy(&base64_bytes(s, B64)).into_owned()))
    }

    /// Raw standard-base64 decode to bytes. Public witchy code reaches this
    /// through `encoding.base64_decode_bytes`, which validates first.
    pub fn base64_decode_bytes_raw(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64_decode_bytes_raw expects a String"));
        };
        Ok(Value::Bytes(base64_bytes(s, B64)))
    }

    fn base64_bytes(s: &str, alphabet: &[u8; 64]) -> Vec<u8> {
        let mut acc: u32 = 0;
        let mut nbits = 0;
        let mut bytes = Vec::new();
        for c in s.bytes() {
            if c == b'=' || c.is_ascii_whitespace() {
                continue;
            }
            let Some(v) = alphabet.iter().position(|&x| x == c) else {
                break;
            };
            acc = (acc << 6) | v as u32;
            nbits += 6;
            if nbits >= 8 {
                nbits -= 8;
                bytes.push((acc >> nbits & 0xff) as u8);
            }
        }
        bytes
    }

    /// Raw base64url decode (URL-safe `-`/`_`, padding/whitespace tolerated) to text
    /// (lossy UTF-8). Byte-level primitive behind the validating
    /// `encoding.base64url_decode` wrapper — the JSON header/payload of a JWT/OIDC token.
    pub fn base64url_decode_lossy(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64url_decode_lossy expects a String"));
        };
        Ok(Value::Str(String::from_utf8_lossy(&base64_bytes(s, B64URL)).into_owned()))
    }

    /// Raw base64url decode to bytes. Public witchy code reaches this through
    /// `encoding.base64url_decode_bytes`, which validates first.
    pub fn base64url_decode_bytes_raw(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64url_decode_bytes_raw expects a String"));
        };
        Ok(Value::Bytes(base64_bytes(s, B64URL)))
    }

    /// Raw base64url decode to a HEX string — for binary that must round-trip through a
    /// witchy String, e.g. a JWT's RS256 signature fed to `crypto.rsa_pkcs1_sha256_verify`.
    /// Byte-level primitive behind the validating `encoding.base64url_to_hex` wrapper.
    pub fn base64url_to_hex_lossy(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(s)] = args else {
            return Err(type_error("encoding.base64url_to_hex_lossy expects a String"));
        };
        Ok(Value::Str(hex_string(&base64_bytes(s, B64URL))))
    }
}

/// The `regex` module's engine: the Rust `regex` crate (RE2 semantics — linear
/// time, full alternation `|` and grouping `(...)`). This single native carries
/// the matching; the whole public `std/regex` API (matches/find/find_all/extract/
/// replace_all/split) is built in witchy on the spans it returns. Positions are
/// CHARACTER indices (witchy strings are char-indexed), converted from the
/// crate's byte offsets, so they feed `string.substring` directly. An invalid
/// pattern is a loud error on every backend, not a silent non-match.
mod regexp {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;

    /// Character offset of `byte` within `text` (a byte index from the crate).
    fn char_off(text: &str, byte: usize) -> i64 {
        text[..byte].chars().count() as i64
    }

    /// Every non-overlapping match as char-index spans, encoded `"s,e;s,e;..."`
    /// (empty string when there is no match). The host-bridged form used by the
    /// WASM backend (`host_regex_spans`) calls straight through to this.
    pub fn match_spans(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Str(pattern), Value::Str(text)] = args else {
            return Err(type_error("regex.match_spans expects (pattern, text) strings"));
        };
        let re = regex::Regex::new(pattern).map_err(|e| RuntimeError {
            message: format!("invalid regex pattern `{pattern}`: {e}"),
        })?;
        let mut out = String::new();
        for m in re.find_iter(text) {
            use std::fmt::Write;
            if !out.is_empty() {
                out.push(';');
            }
            let _ = write!(out, "{},{}", char_off(text, m.start()), char_off(text, m.end()));
        }
        Ok(Value::Str(out))
    }
}

mod string {
    use super::{type_error, Value};
    use crate::value::NativeError as RuntimeError;

    /// The single character for a Unicode scalar value, as a UTF-8 string. An
    /// out-of-range or surrogate code point yields U+FFFD (the replacement
    /// character) rather than an error — callers (e.g. the JSON `\u` decoder)
    /// range-check themselves, and this must never trap mid-parse.
    pub fn from_code(args: &[Value]) -> Result<Value, RuntimeError> {
        let [Value::Int(n)] = args else {
            return Err(type_error("string.from_code expects an Int"));
        };
        let ch = u32::try_from(*n).ok().and_then(char::from_u32).unwrap_or('\u{FFFD}');
        Ok(Value::Str(ch.to_string()))
    }
}

#[cfg(test)]
mod intrinsic_catalog_tests {
    #[test]
    fn cataloged_native_runtime_hooks_exist() {
        use witchy_syntax::intrinsics::IntrinsicRuntime;

        for intrinsic in witchy_syntax::intrinsics::ALL {
            match intrinsic.runtime {
                IntrinsicRuntime::Native => assert!(
                    super::lookup(intrinsic.name).is_some(),
                    "intrinsic {} names a missing native runtime hook",
                    intrinsic.name
                ),
                IntrinsicRuntime::InterpreterBuiltin | IntrinsicRuntime::SourceFunction => assert!(
                    super::lookup(intrinsic.name).is_none(),
                    "non-native intrinsic {} leaked into the native runtime registry",
                    intrinsic.name
                ),
            }
        }
    }
}
