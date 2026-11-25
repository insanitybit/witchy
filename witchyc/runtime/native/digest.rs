//! Public digest adapter for the selected platform crypto provider.

#[cfg(witchy_native_aws_lc)]
unsafe extern "C" {
    #[link_name = "aws_lc_fips_0_14_4_FIPS_mode"]
    pub(super) fn fips_mode() -> core::ffi::c_int;
    #[link_name = "aws_lc_fips_0_14_4_SHA256"]
    fn sha256(input: *const u8, length: usize, output: *mut u8) -> *mut u8;
    #[link_name = "aws_lc_fips_0_14_4_SHA384"]
    fn sha384(input: *const u8, length: usize, output: *mut u8) -> *mut u8;
    #[link_name = "aws_lc_fips_0_14_4_SHA512"]
    fn sha512(input: *const u8, length: usize, output: *mut u8) -> *mut u8;
}

#[cfg(all(target_os = "macos", not(witchy_native_aws_lc)))]
unsafe extern "C" {
    #[link_name = "CC_SHA256"]
    fn sha256(input: *const u8, length: u32, output: *mut u8) -> *mut u8;
    #[link_name = "CC_SHA384"]
    fn sha384(input: *const u8, length: u32, output: *mut u8) -> *mut u8;
    #[link_name = "CC_SHA512"]
    fn sha512(input: *const u8, length: u32, output: *mut u8) -> *mut u8;
}

/// The guest emitter supplies readable input and writable digest storage.
/// Hash into a local buffer so the provider never sees overlapping spans.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_public_digest(
    mode: u64,
    data: u64,
    length: u64,
    digest: u64,
    capacity: u64,
) -> u64 {
    if !matches!(mode, 256 | 384 | 512) {
        unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
    }
    if capacity != mode / 8 {
        return 2;
    }
    if length > 64 * 1024 * 1024 {
        return 3;
    }
    #[cfg(all(target_os = "linux", not(witchy_native_aws_lc)))]
    {
        let _ = (data, digest);
        return 4;
    }
    #[cfg(any(target_os = "macos", witchy_native_aws_lc))]
    {
        let input = if length == 0 {
            c"".as_ptr().cast()
        } else {
            data as *const u8
        };
        let mut output = [0u8; 64];
        #[cfg(witchy_native_aws_lc)]
        let length = {
            if unsafe { fips_mode() } == 0 {
                return 4;
            }
            length as usize
        };
        #[cfg(not(witchy_native_aws_lc))]
        let length = length as u32;
        let hashed = unsafe {
            match mode {
                256 => sha256(input, length, output.as_mut_ptr()),
                384 => sha384(input, length, output.as_mut_ptr()),
                _ => sha512(input, length, output.as_mut_ptr()),
            }
        };
        if hashed.is_null() {
            return 4;
        }
        unsafe {
            core::ptr::copy_nonoverlapping(output.as_ptr(), digest as *mut u8, capacity as usize)
        };
        0
    }
}

#[cfg(witchy_native_aws_lc)]
mod verify {
    use super::fips_mode;
    use core::{ffi::c_int, ptr::null_mut};

    use crate::pkix::ffi::{PublicKey, free_digest_context as free_context,
        new_digest_context as new_context, digest_verify_init as verify_init,
        digest_verify as verify};
    use crate::tls_signer::free_key;

    unsafe extern "C" {
        #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_pqdsa_new_raw_public_key"]
        fn new_key(nid: c_int, key: *const u8, len: usize) -> *mut PublicKey;
    }

    /// The guest emitter supplies readable spans for all nonempty inputs.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn witchy_public_mldsa_verify(
        suite: u64,
        key: u64,
        key_len: u64,
        message: u64,
        message_len: u64,
        signature: u64,
        signature_len: u64,
    ) -> u64 {
        // AWS-LC NIDs for the two raw public key encodings.
        let (algorithm, expected_key, expected_signature) = match suite {
            65 => (995, 1952, 3309),
            #[cfg(target_os = "macos")]
            87 => (996, 2592, 4627),
            _ => return 0,
        };
        if key_len != expected_key
            || signature_len != expected_signature
            || message_len > 64 * 1024 * 1024
            || unsafe { fips_mode() } == 0
        {
            return 0;
        }
        let key = unsafe { new_key(algorithm, key as *const u8, key_len as usize) };
        if key.is_null() {
            return 0;
        }
        let context = unsafe { new_context() };
        let input = if message_len == 0 {
            c"".as_ptr().cast()
        } else {
            message as *const u8
        };
        let verified = unsafe {
            let verified = !context.is_null()
                && verify_init(context, null_mut(), core::ptr::null(), null_mut(), key) == 1
                && verify(
                    context,
                    signature as *const u8,
                    signature_len as usize,
                    input,
                    message_len as usize,
                ) == 1;
            free_context(context);
            free_key(key);
            verified
        };
        u64::from(verified)
    }
}
