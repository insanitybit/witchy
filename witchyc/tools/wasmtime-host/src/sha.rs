//! The engine's cryptography, through AWS-LC's FIPS module (`aws-lc-rs` with `fips`,
//! `aws-lc-fips-sys`; docs/decisions.md, 2026-11-20): SHA-256, SHA-384 and SHA-512 (FIPS 180-4),
//! HMAC-SHA-256 (FIPS 198-1, what a sealed secret signs with, tag for tag what the handwritten
//! version gave), ML-DSA-65 (the `witchy-2` suite), ML-DSA-87 verification (FIPS 204) and the randomness behind `Rand`.
//! ML-DSA-44 (`witchy-1`) is gone: the approved profile takes one strength step above entry
//! level. Building with the module makes no FIPS claim: a deployment must match its binary to
//! the module's certificate first.
use aws_lc_rs::signature::KeyPair;

/// The FIPS 204 sizes of ML-DSA-65: a seed, a public key and a signature.
pub const MLDSA65_SEED: usize = 32;
pub const MLDSA65_PUBLIC_KEY: usize = 1952;
pub const MLDSA65_SIGNATURE: usize = 3309;
pub const MLDSA87_PUBLIC_KEY: usize = 2592;
pub const MLDSA87_SIGNATURE: usize = 4627;

/// The provider is AWS-LC's FIPS module, in FIPS mode, or no operation runs: checked once,
/// before the first (the module's power-on self-tests have passed by then).
pub fn provider() -> Result<(), &'static str> {
    static CHECKED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *CHECKED.get_or_init(|| aws_lc_rs::try_fips_mode().is_ok()) { Ok(()) } else { Err("crypto provider is not in FIPS mode") }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, data).as_ref());
    out
}
/// SHA-384 (FIPS 180-4): the `witchy-2` profile's content digests, pins and key IDs.
pub fn sha384(data: &[u8]) -> [u8; 48] {
    let mut out = [0u8; 48];
    out.copy_from_slice(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA384, data).as_ref());
    out
}
pub fn sha512(data: &[u8]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out.copy_from_slice(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA512, data).as_ref());
    out
}
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(aws_lc_rs::hmac::sign(&aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, key), message).as_ref());
    out
}

/// An ML-DSA-65 signing key from its 32-byte seed (FIPS 204's xi, ML-DSA.KeyGen_internal),
/// the only form a signer's slot holds. Any other length is refused; every 32 bytes are a seed.
pub fn mldsa65_from_seed(seed: &[u8]) -> Result<aws_lc_rs::signature::PqdsaKeyPair, &'static str> {
    if seed.len() != MLDSA65_SEED { return Err("not an ML-DSA-65 seed (32 bytes)"); }
    aws_lc_rs::signature::PqdsaKeyPair::from_seed(&aws_lc_rs::signature::ML_DSA_65_SIGNING, seed).map_err(|_| "key rejected")
}
pub fn mldsa87_from_seed(seed: &[u8]) -> Result<aws_lc_rs::signature::PqdsaKeyPair, &'static str> {
    if seed.len() != MLDSA65_SEED { return Err("not an ML-DSA-87 seed (32 bytes)"); }
    aws_lc_rs::signature::PqdsaKeyPair::from_seed(&aws_lc_rs::signature::ML_DSA_87_SIGNING, seed).map_err(|_| "key rejected")
}
pub fn mldsa65_public_key(key: &aws_lc_rs::signature::PqdsaKeyPair) -> &[u8] { key.public_key().as_ref() }
/// A hedged ML-DSA-65 signature of the whole message, empty context (FIPS 204 ML-DSA.Sign):
/// AWS-LC draws the per-signature randomness from its own DRBG.
pub fn mldsa65_sign(key: &aws_lc_rs::signature::PqdsaKeyPair, message: &[u8]) -> Result<[u8; MLDSA65_SIGNATURE], ()> {
    let mut out = [0u8; MLDSA65_SIGNATURE];
    match key.sign(message, &mut out) { Ok(MLDSA65_SIGNATURE) => Ok(out), _ => Err(()) }
}
/// FIPS 204 ML-DSA.Verify (ML-DSA-65) with empty context. A key or signature of another length
/// (an ML-DSA-44 one included) is invalid.
pub fn mldsa65_verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    public_key.len() == MLDSA65_PUBLIC_KEY && signature.len() == MLDSA65_SIGNATURE
        && aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ML_DSA_65, public_key).verify(message, signature).is_ok()
}
/// Pure ML-DSA-87 with empty context and the raw FIPS 204 encodings.
pub fn mldsa87_verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    public_key.len() == MLDSA87_PUBLIC_KEY && signature.len() == MLDSA87_SIGNATURE
        && aws_lc_rs::signature::UnparsedPublicKey::new(&aws_lc_rs::signature::ML_DSA_87, public_key).verify(message, signature).is_ok()
}

pub fn p256_verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> bool {
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};
    public_key.len() == 65 && public_key[0] == 4 && (8..=72).contains(&signature.len())
        && UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, public_key).verify(message, signature).is_ok()
}

pub fn rsa_pkcs1_sha256_verify(modulus: &[u8], exponent: &[u8], message: &[u8], signature: &[u8]) -> bool {
    use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
    (256..=1024).contains(&modulus.len()) && (1..=5).contains(&exponent.len())
        && signature.len() == modulus.len()
        && RsaPublicKeyComponents { n: modulus, e: exponent }
            .verify(&RSA_PKCS1_2048_8192_SHA256, message, signature).is_ok()
}
/// Fills `out` from the module's DRBG (SP 800-90A), seeded from the operating system.
pub fn fill_random(out: &mut [u8]) -> Result<(), ()> { aws_lc_rs::rand::fill(out).map_err(|_| ()) }

#[cfg(test)]
mod tests {
    fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
    /// Digests from Python's hashlib and hmac, including the padding boundaries.
    #[test]
    fn matches_known_digests() {
        assert_eq!(hex(&super::sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&super::hmac_sha256(b"key", b"The quick brown fox jumps over the lazy dog")), "f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8");
        assert_eq!(hex(&super::hmac_sha256(b"k3y", b"release 1.0")), "8acdbced357f9c42609ed9720f7be873f47874602804b6c6c2072267bbb0b3bc");
        for (length, expected) in [(55usize, "02779466cdec163811d078815c633f21901413081449002f24aa3e80f0b88ef7"),
            (56, "d4817aa5497628e7c77e6b606107042bbba3130888c5f47a375e6179be789fbb"), (64, "f5a5fd42d16a20302798ef6ed309979b43003d2320d9f0e8ea9831a92759fb4b")] {
            assert_eq!(hex(&super::sha256(&vec![0u8; length])), expected);
        }
        // RFC 4231 cases 2 and 6 (a key longer than the block is hashed first): the tags the
        // handwritten HMAC gave before the move to AWS-LC, and fixtures depend on.
        assert_eq!(hex(&super::hmac_sha256(b"Jefe", b"what do ya want for nothing?")), "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
        assert_eq!(hex(&super::hmac_sha256(&[0xaa; 131], b"Test Using Larger Than Block-Size Key - Hash Key First")), "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54");
        assert_eq!(hex(&super::sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    fn unhex(text: &str) -> Vec<u8> { (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect() }
    /// FIPS 180-4 SHA-384: the "abc" and two-block examples, the empty input, and the padding
    /// boundaries at 111, 112 and 128 bytes (Python's hashlib).
    #[test]
    fn sha384_matches_known_digests() {
        super::provider().unwrap();
        assert_eq!(hex(&super::sha384(b"abc")), "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7");
        assert_eq!(hex(&super::sha384(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")), "3391fdddfc8dc7393707a65b1b4709397cf8b1d162af05abfe8f450de5f36bc6b0455a8520bc4e6f5fe95b1fe3c8452b");
        assert_eq!(hex(&super::sha384(b"")), "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b");
        for (length, expected) in [(111usize, "435770712c611be7293a66dd0dc8d1450dc7ff7337bfe115bf058ef2eb9bed09cee85c26963a5bcc0905dc2df7cc6a76"),
            (112, "3e0cbf3aee0e3aa70415beae1bd12dd7db821efa446440f12132edffce76f635e53526a111491e75ee8e27b9700eec20"),
            (128, "f809b88323411f24a6f152e5e9d9d1b5466b77e0f3c7550f8b242c31b6e7b99bcb45bdecb6124bc23283db3b9fc4f5b3")] {
            assert_eq!(hex(&super::sha384(&vec![0u8; length])), expected);
        }
    }

    /// NIST ACVP ML-DSA-65 KeyGen (draft-celi-acvp-ml-dsa, Table 1, Path/ML-DSA-65/1, as Go's
    /// crypto/internal/fips140/mldsa tests carry it): the seed, and SHA-256 of the public key
    /// followed by the expanded private key.
    const KAT_SEED: &str = "464756A985E5DF03739D95DD309C1ED9C5B04254CC294E7E7EB9B9365EE15117";
    const KAT_KEY_HASH: &str = "AE95EA0DAA80199E7B4A74EB5A1B1DC6C3805BD01D2FA78D7C4FBA8C255AA13D";
    /// A signature over a witchy-2 `targets` object by that key, made by an independent FIPS 204
    /// implementation (Go 1.27.3 crypto/mldsa; tests/fixtures/crypto/mldsa65-kat/generate.go.txt).
    const KAT_PUBLIC_KEY: &[u8] = include_bytes!("../../../tests/fixtures/crypto/mldsa65-kat/public-key.bin");
    const KAT_MESSAGE: &[u8] = include_bytes!("../../../tests/fixtures/crypto/mldsa65-kat/message.bin");
    const KAT_SIGNATURE: &[u8] = include_bytes!("../../../tests/fixtures/crypto/mldsa65-kat/signature.bin");

    #[test]
    fn mldsa65_key_generation_matches_the_nist_vector() {
        use aws_lc_rs::encoding::AsRawBytes;
        super::provider().unwrap();
        let key = super::mldsa65_from_seed(&unhex(KAT_SEED)).unwrap();
        let private = key.private_key().as_raw_bytes().unwrap();
        let both: Vec<u8> = super::mldsa65_public_key(&key).iter().chain(private.as_ref()).copied().collect();
        assert_eq!(hex(&super::sha256(&both)), KAT_KEY_HASH.to_lowercase());
        // The independent implementation derived the same public key from the seed.
        assert_eq!(super::mldsa65_public_key(&key), KAT_PUBLIC_KEY);
        // A seed is 32 bytes exactly.
        assert!(super::mldsa65_from_seed(&[7; 31]).is_err());
        assert!(super::mldsa65_from_seed(&[7; 33]).is_err());
        assert!(super::mldsa65_from_seed(&[]).is_err());
    }

    #[test]
    fn mldsa65_verifies_the_independent_vector_and_nothing_near_it() {
        assert_eq!((KAT_PUBLIC_KEY.len(), KAT_SIGNATURE.len()), (super::MLDSA65_PUBLIC_KEY, super::MLDSA65_SIGNATURE));
        assert!(super::mldsa65_verify(KAT_PUBLIC_KEY, KAT_MESSAGE, KAT_SIGNATURE));
        // A changed message, signature or key; a truncated or extended signature or key.
        let mut message = KAT_MESSAGE.to_vec(); *message.last_mut().unwrap() ^= 1;
        assert!(!super::mldsa65_verify(KAT_PUBLIC_KEY, &message, KAT_SIGNATURE));
        let mut signature = KAT_SIGNATURE.to_vec(); signature[100] ^= 0x10;
        assert!(!super::mldsa65_verify(KAT_PUBLIC_KEY, KAT_MESSAGE, &signature));
        let mut key = KAT_PUBLIC_KEY.to_vec(); key[40] ^= 1;
        assert!(!super::mldsa65_verify(&key, KAT_MESSAGE, KAT_SIGNATURE));
        assert!(!super::mldsa65_verify(KAT_PUBLIC_KEY, KAT_MESSAGE, &KAT_SIGNATURE[..3308]));
        let longer: Vec<u8> = KAT_SIGNATURE.iter().copied().chain([0]).collect();
        assert!(!super::mldsa65_verify(KAT_PUBLIC_KEY, KAT_MESSAGE, &longer));
        assert!(!super::mldsa65_verify(&KAT_PUBLIC_KEY[..1951], KAT_MESSAGE, KAT_SIGNATURE));
        // The retired suite's sizes (ML-DSA-44: 1312 and 2420) are never taken as ML-DSA-65.
        assert!(!super::mldsa65_verify(&KAT_PUBLIC_KEY[..1312], KAT_MESSAGE, &KAT_SIGNATURE[..2420]));
    }

    /// Signing is hedged: two signatures of one message differ, and each verifies with the
    /// public key alone; another key's signature does not.
    #[test]
    fn mldsa65_signing_is_hedged_and_verifies() {
        let key = super::mldsa65_from_seed(&unhex(KAT_SEED)).unwrap();
        let (first, second) = (super::mldsa65_sign(&key, KAT_MESSAGE).unwrap(), super::mldsa65_sign(&key, KAT_MESSAGE).unwrap());
        assert_ne!(first, second);
        assert!(super::mldsa65_verify(KAT_PUBLIC_KEY, KAT_MESSAGE, &first) && super::mldsa65_verify(KAT_PUBLIC_KEY, KAT_MESSAGE, &second));
        let other = super::mldsa65_from_seed(&[1; 32]).unwrap();
        let theirs = super::mldsa65_sign(&other, KAT_MESSAGE).unwrap();
        assert!(!super::mldsa65_verify(KAT_PUBLIC_KEY, KAT_MESSAGE, &theirs));
        assert!(super::mldsa65_verify(super::mldsa65_public_key(&other), KAT_MESSAGE, &theirs));
    }
}
