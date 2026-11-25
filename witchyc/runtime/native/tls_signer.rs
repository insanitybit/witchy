//! Native opaque P-384 TLS server signing through the pinned AWS-LC provider.

use core::{ffi::{c_int, c_void}, ptr::{self, NonNull, null_mut}};

use super::{
    pkix::{curve, ffi},
    signer_binding::witchy_native_secret_slot,
    signer_crypto::slot_ptr,
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
    pub(super) fn sign_init(ctx: *mut ffi::DigestContext, key_context: *mut *mut c_void,
        digest: *const ffi::Digest, engine: *mut c_void, key: *mut ffi::PublicKey) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_DigestSign"]
    pub(super) fn sign(ctx: *mut ffi::DigestContext, output: *mut u8, size: *mut usize,
        message: *const u8, len: usize) -> c_int;
}

pub(super) const MAX_KEY_BYTES: usize = 4096;

/// The input is the bounded PKCS#8 DER read from a launch descriptor.
pub(super) fn from_pkcs8(bytes: &[u8]) -> Option<NonNull<ffi::PublicKey>> {
    let mut input = Cbs { data: bytes.as_ptr(), len: bytes.len() };
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
    unsafe { ffi::clear_error() };
    parsed
}

/// The emitter supplies the readable transcript and writable signature spans.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_tls_signer_sign(
    handle: u64, transcript: *const u8, len: usize, output: *mut u8, capacity: usize,
) -> u64 {
    let index = witchy_native_secret_slot(handle, 3);
    let slot = unsafe { &*slot_ptr(index) };
    if slot.key_kind != 4 || slot.key.is_null() {
        unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
    }
    if len != HASH_LEN || capacity < DER_CAPACITY {
        return 0;
    }

    let message = server_message(unsafe { &*transcript.cast::<[u8; HASH_LEN]>() });
    let mut signature = [0; DER_CAPACITY];
    let mut written = signature.len();
    let ctx = unsafe { ffi::new_digest_context() };
    let success = unsafe {
        let ok = !ctx.is_null()
            && sign_init(ctx, null_mut(), ffi::sha384(), null_mut(), slot.key.cast()) == 1
            && sign(ctx, signature.as_mut_ptr(), &mut written, message.as_ptr(), message.len()) == 1;
        ffi::free_digest_context(ctx);
        ffi::clear_error();
        ok
    };
    if !success || !(1..=signature.len()).contains(&written) {
        return 0;
    }
    unsafe { ptr::copy_nonoverlapping(signature.as_ptr(), output, written) };
    written as u64
}
