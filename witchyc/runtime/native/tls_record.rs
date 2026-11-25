//! Native adapter for the pinned AWS-LC provider. The Witchy record layer
//! owns key epochs and nonce sequencing.

use super::digest::fips_mode;
use core::{ffi::{c_int, c_void}, mem::MaybeUninit, ptr};

#[repr(transparent)]
struct Algorithm(c_void);

#[repr(transparent)]
struct Context(c_void);

type Transform = unsafe extern "C" fn(
    *const Context, *mut u8, *mut usize, usize, *const u8, usize,
    *const u8, usize, *const u8, usize,
) -> c_int;

unsafe extern "C" {
    #[link_name = "aws_lc_fips_0_14_4_EVP_aead_aes_128_gcm_tls13"]
    fn aes128() -> *const Algorithm;
    #[link_name = "aws_lc_fips_0_14_4_EVP_aead_aes_256_gcm_tls13"]
    fn aes256() -> *const Algorithm;
    #[link_name = "aws_lc_fips_0_14_4_EVP_AEAD_CTX_new"]
    fn new_context(algorithm: *const Algorithm, key: *const u8, len: usize, tag_len: usize) -> *mut Context;
    #[link_name = "aws_lc_fips_0_14_4_EVP_AEAD_CTX_free"]
    fn free_context(context: *mut Context);
    #[link_name = "aws_lc_fips_0_14_4_EVP_AEAD_CTX_seal"]
    fn seal(context: *const Context, output: *mut u8, written: *mut usize, capacity: usize,
        nonce: *const u8, nonce_len: usize, input: *const u8, input_len: usize,
        aad: *const u8, aad_len: usize) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EVP_AEAD_CTX_open"]
    fn open(context: *const Context, output: *mut u8, written: *mut usize, capacity: usize,
        nonce: *const u8, nonce_len: usize, input: *const u8, input_len: usize,
        aad: *const u8, aad_len: usize) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_CRYPTO_memcmp"]
    fn compare(left: *const c_void, right: *const c_void, len: usize) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_OPENSSL_cleanse"]
    fn cleanse(bytes: *mut c_void, len: usize);
    #[link_name = "aws_lc_fips_0_14_4_ERR_clear_error"]
    fn clear_error();
}

const TAG_BYTES: usize = 16;
const MAX_CIPHERTEXT: usize = (1 << 14) + 256;

/// The emitter supplies readable input and a writable destination of the
/// stated lengths. Failed authentication must leave that destination intact.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_tls13_aead(
    mode: u64, key: *const u8, key_len: usize, nonce: *const u8, nonce_len: usize,
    header: *const u8, header_len: usize, input: *const u8, input_len: usize,
    output: *mut u8, output_len: usize,
) -> u64 {
    if input_len > MAX_CIPHERTEXT || output_len > MAX_CIPHERTEXT {
        return 3;
    }
    let (algorithm, key_bytes, transform, sealing): (unsafe extern "C" fn() -> *const Algorithm, usize, Transform, bool) = match mode {
        0 => (aes128, 16, seal as Transform, true),
        1 => (aes128, 16, open as Transform, false),
        2 => (aes256, 32, seal as Transform, true),
        3 => (aes256, 32, open as Transform, false),
        _ => return 4,
    };
    let correct_size = if sealing { input_len + TAG_BYTES == output_len }
        else { output_len + TAG_BYTES == input_len };
    if key_len != key_bytes || nonce_len != 12 || header_len != 5 || !correct_size {
        return 2;
    }
    if unsafe { fips_mode() } != 1 {
        return 4;
    }
    let context = unsafe { new_context(algorithm(), key, key_len, TAG_BYTES) };
    if context.is_null() {
        unsafe { clear_error() };
        return 4;
    }

    let mut scratch = MaybeUninit::<[u8; MAX_CIPHERTEXT]>::uninit();
    let scratch = scratch.as_mut_ptr().cast::<u8>();
    let input = if input_len == 0 { c"".as_ptr().cast() } else { input };
    let mut written = 0;
    let ok = unsafe {
        let status = transform(context, scratch, &mut written, output_len, nonce, nonce_len,
            input, input_len, header, header_len);
        free_context(context);
        clear_error();
        let ok = status == 1 && written == output_len;
        if ok && written != 0 {
            ptr::copy_nonoverlapping(scratch, output, written);
        }
        cleanse(scratch.cast(), output_len);
        ok
    };
    if ok { 0 } else { 4 }
}

/// Both inputs are the readable spans supplied by the emitter.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_verify_tag48(
    left: *const c_void, left_len: usize, right: *const c_void, right_len: usize,
) -> u64 {
    u64::from(left_len == 48 && right_len == 48
        && unsafe { fips_mode() == 1 && compare(left, right, 48) == 0 })
}
