//! Native opaque P-384 TLS server signing through the pinned AWS-LC provider,
//! and the provider's private keys that the signer slots hold.

use core::{ffi::{c_int, c_void}, ptr::{self, NonNull, null, null_mut}};

use super::{
    digest::clear_error,
    pkix::{curve, ffi},
    signer_binding::witchy_native_secret_slot,
    signer_crypto::{slot, trap},
    tls_signature::{DER_CAPACITY, HASH_LEN, server_message},
};

#[repr(C)]
struct Cbs {
    data: *const u8,
    len: usize,
}

unsafe extern "C" {
    #[link_name = "aws_lc_fips_0_14_4_EVP_parse_private_key"]
    fn parse_key(bytes: *mut Cbs) -> *mut ffi::PublicKey;
    #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_free"]
    pub(super) fn free_key(key: *mut ffi::PublicKey);
    #[link_name = "aws_lc_fips_0_14_4_EC_KEY_check_fips"]
    fn check_ec_key(key: *const ffi::EcKey) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_DigestSignInit"]
    fn sign_init(ctx: *mut ffi::DigestContext, key_context: *mut *mut c_void,
        digest: *const ffi::Digest, engine: *mut c_void, key: *mut ffi::PublicKey) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_DigestSign"]
    fn sign(ctx: *mut ffi::DigestContext, output: *mut u8, size: *mut usize,
        message: *const u8, len: usize) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_pqdsa_new_raw_private_key"]
    fn pqdsa_private_key(nid: c_int, bytes: *const u8, len: usize) -> *mut ffi::PublicKey;
    #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_get_raw_public_key"]
    fn raw_public_key(key: *const ffi::PublicKey, out: *mut u8, len: *mut usize) -> c_int;
}

pub(super) const MAX_KEY_BYTES: usize = 4096;

/// What the provider hashes the message with before it signs.
pub(super) enum Prehash {
    /// Nothing: the key's own algorithm takes the whole message.
    None,
    Sha384,
}

/// A private key the provider holds. The only ways to get one are the two
/// functions here that make one, and none is ever freed, so every copy names
/// a live key for the life of the process.
#[derive(Clone, Copy)]
pub(super) struct PrivateKey(NonNull<ffi::PublicKey>);

impl PrivateKey {
    /// The input is the bounded PKCS#8 DER read from a launch descriptor.
    pub(super) fn from_pkcs8(bytes: &[u8]) -> Option<Self> {
        let mut input = Cbs { data: bytes.as_ptr(), len: bytes.len() };
        // SAFETY: the parser reads only the `bytes` that `input` spans and
        // keeps none of them. The key is checked before it is used, and it is
        // freed here, once, when it is not the one returned.
        let parsed = unsafe {
            let key = parse_key(&mut input);
            if key.is_null() {
                None
            } else if input.len == 0 && curve(key) == Some(ffi::P384)
                && check_ec_key(ffi::ec_key(key)) == 1
            {
                NonNull::new(key)
            } else {
                free_key(key);
                None
            }
        };
        clear_error();
        parsed.map(Self)
    }

    /// The ML-DSA-65 key that `seed` expands to.
    pub(super) fn mldsa65(seed: &[u8; 32]) -> Option<Self> {
        const MLDSA65: c_int = 995;
        // SAFETY: the call reads the 32 bytes of `seed` and keeps none of them.
        NonNull::new(unsafe { pqdsa_private_key(MLDSA65, seed.as_ptr(), seed.len()) }).map(Self)
    }

    /// Writes the raw public key to the front of `out`. Its length, or `None`
    /// when the key has no raw form or `out` is too short for it.
    pub(super) fn raw_public(self, out: &mut [u8]) -> Option<usize> {
        let mut length = out.len();
        // SAFETY: the key is live. The call writes at most the `length` bytes
        // it is given, which `out` has, and then sets `length` to the count.
        let done = unsafe { raw_public_key(self.0.as_ptr(), out.as_mut_ptr(), &mut length) } == 1;
        done.then_some(length)
    }

    /// Signs `message` and writes the signature to the front of `signature`.
    /// Its length, or `None` when the provider refuses or `signature` is too
    /// short. The provider's error queue is left as the provider leaves it.
    pub(super) fn sign(self, prehash: Prehash, message: &[u8], signature: &mut [u8]) -> Option<usize> {
        let mut written = signature.len();
        // SAFETY: the key is live, and the context is checked before it is
        // used and freed once. The provider reads `message` for its length,
        // and writes at most the `written` bytes it is given, which
        // `signature` has, before it sets `written` to the count.
        unsafe {
            let ctx = ffi::new_digest_context();
            if ctx.is_null() {
                return None;
            }
            let digest = match prehash {
                Prehash::None => null(),
                Prehash::Sha384 => ffi::sha384(),
            };
            let ok = sign_init(ctx, null_mut(), digest, null_mut(), self.0.as_ptr()) == 1
                && sign(ctx, signature.as_mut_ptr(), &mut written, message.as_ptr(), message.len()) == 1;
            ffi::free_digest_context(ctx);
            ok.then_some(written)
        }
    }
}

/// The emitter supplies the readable transcript and writable signature spans.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_tls_signer_sign(
    handle: u64, transcript: *const u8, len: usize, output: *mut u8, capacity: usize,
) -> u64 {
    let slot = slot(witchy_native_secret_slot(handle, 3));
    let key = match slot.key.get() {
        Some(key) if slot.key_kind.get() == 4 => key,
        _ => trap(),
    };
    if len != HASH_LEN || capacity < DER_CAPACITY {
        return 0;
    }

    // SAFETY: the emitter passes `len` readable bytes at `transcript`, and
    // `len` is `HASH_LEN` here.
    let message = server_message(unsafe { &*transcript.cast::<[u8; HASH_LEN]>() });
    let mut signature = [0; DER_CAPACITY];
    let written = key.sign(Prehash::Sha384, &message, &mut signature);
    clear_error();
    let Some(written) = written.filter(|written| (1..=signature.len()).contains(written)) else {
        return 0;
    };
    // SAFETY: the emitter passes `capacity` writable bytes at `output`, and
    // `written` is at most `DER_CAPACITY`, which is at most `capacity` here.
    unsafe { ptr::copy_nonoverlapping(signature.as_ptr(), output, written) };
    written as u64
}
