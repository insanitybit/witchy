//! Public digest adapter for the selected platform crypto provider.

#[cfg(witchy_native_aws_lc)]
unsafe extern "C" {
    // The call takes no argument and reads only the provider's own state.
    #[link_name = "aws_lc_fips_0_14_4_FIPS_mode"]
    pub(super) safe fn fips_mode() -> core::ffi::c_int;
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

/// Empties the provider's queue of errors.
#[cfg(witchy_native_aws_lc)]
pub(super) fn clear_error() {
    // SAFETY: the call takes no argument and writes only the provider's own state.
    unsafe { super::pkix::ffi::clear_error() }
}

/// No bytes, at an address that holds one. C code is handed this where the
/// guest's span is empty and its address may be anything.
#[cfg(any(target_os = "macos", witchy_native_aws_lc))]
fn empty() -> &'static [u8] {
    c"".to_bytes()
}

/// Writes the digest `mode` names of `input` to the front of `output`. False
/// when the provider gives none. `input` is at most 64 MiB.
#[cfg(any(target_os = "macos", witchy_native_aws_lc))]
fn hash(mode: u64, input: &[u8], output: &mut [u8; 64]) -> bool {
    #[cfg(witchy_native_aws_lc)]
    let length = input.len();
    #[cfg(not(witchy_native_aws_lc))]
    let length = input.len() as u32;
    // SAFETY: each function reads `length` bytes of `input`, writes its digest,
    // which is at most the 64 bytes `output` has, and keeps neither pointer.
    let hashed = unsafe {
        match mode {
            256 => sha256(input.as_ptr(), length, output.as_mut_ptr()),
            384 => sha384(input.as_ptr(), length, output.as_mut_ptr()),
            _ => sha512(input.as_ptr(), length, output.as_mut_ptr()),
        }
    };
    !hashed.is_null()
}

#[inline(always)]
fn span_error(mode: u64, length: u64, capacity: u64) -> Option<u64> {
    if capacity != mode / 8 {
        return Some(2);
    }
    if length > 64 * 1024 * 1024 {
        return Some(3);
    }
    None
}

#[cfg(kani)]
#[kani::proof]
fn admitted_digest_spans_fit_provider_and_scratch_buffer() {
    let mode: u64 = kani::any();
    let length: u64 = kani::any();
    let capacity: u64 = kani::any();
    kani::assume(matches!(mode, 256 | 384 | 512));
    let width = match mode {
        256 => 32,
        384 => 48,
        _ => 64,
    };
    match span_error(mode, length, capacity) {
        None => {
            assert_eq!(capacity, width);
            assert!(capacity <= 64);
            assert!(length <= 1 << 26);
            assert_eq!(u64::from(length as u32), length);
            assert!(length <= isize::MAX as u64);
        }
        Some(2) => assert_ne!(capacity, width),
        Some(3) => {
            assert_eq!(capacity, width);
            assert!(length > 1 << 26);
        }
        Some(_) => unreachable!(),
    }
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
    if let Some(error) = span_error(mode, length, capacity) {
        return error;
    }
    #[cfg(all(target_os = "linux", not(witchy_native_aws_lc)))]
    {
        let _ = (data, digest);
        return 4;
    }
    #[cfg(any(target_os = "macos", witchy_native_aws_lc))]
    {
        let input = if length == 0 {
            empty()
        } else {
            // SAFETY: the emitter passes `length` readable bytes at `data`.
            // Nothing writes them while this borrow lives: the digest goes to
            // a local, and to the guest only after `hash` has returned.
            unsafe { core::slice::from_raw_parts(data as *const u8, length as usize) }
        };
        let mut output = [0u8; 64];
        #[cfg(witchy_native_aws_lc)]
        if fips_mode() == 0 {
            return 4;
        }
        if !hash(mode, input, &mut output) {
            return 4;
        }
        // SAFETY: the emitter passes `capacity` writable bytes at `digest`.
        // `capacity` is 32, 48 or 64 here, and `output` has 64.
        unsafe {
            core::ptr::copy_nonoverlapping(output.as_ptr(), digest as *mut u8, capacity as usize)
        };
        0
    }
}

#[cfg(kani)]
#[kani::proof]
fn rejected_digest_spans_do_not_access_guest_addresses() {
    let mode: u64 = kani::any();
    let length: u64 = kani::any();
    let capacity: u64 = kani::any();
    kani::assume(matches!(mode, 256 | 384 | 512));
    kani::assume(capacity != mode / 8 || length > 1 << 26);
    // Rejected requests must return even when neither address is usable.
    let result = unsafe { witchy_public_digest(mode, 0, length, 0, capacity) };
    assert_eq!(result, if capacity != mode / 8 { 2 } else { 3 });
}

#[cfg(witchy_native_aws_lc)]
mod verify {
    use super::{empty, fips_mode};
    use core::{ffi::c_int, ptr::null_mut, slice};

    use crate::pkix::ffi::{PublicKey, free_digest_context as free_context,
        new_digest_context as new_context, digest_verify_init as verify_init,
        digest_verify as verify};
    use crate::tls_signer::free_key;

    unsafe extern "C" {
        #[link_name = "aws_lc_fips_0_14_4_EVP_PKEY_pqdsa_new_raw_public_key"]
        fn new_key(nid: c_int, key: *const u8, len: usize) -> *mut PublicKey;
    }

    /// Whether `signature` is the provider's signature of `message` under the
    /// raw public key `key` of the parameter set `algorithm` names.
    fn verified(algorithm: c_int, key: &[u8], message: &[u8], signature: &[u8]) -> bool {
        // SAFETY: each span is read for its own length and none is kept. The
        // key object is checked before it is used, and so is `context`. Both
        // are freed once, here, and freeing a null context does nothing.
        unsafe {
            let key = new_key(algorithm, key.as_ptr(), key.len());
            if key.is_null() {
                return false;
            }
            let context = new_context();
            let verified = !context.is_null()
                && verify_init(context, null_mut(), core::ptr::null(), null_mut(), key) == 1
                && verify(
                    context,
                    signature.as_ptr(),
                    signature.len(),
                    message.as_ptr(),
                    message.len(),
                ) == 1;
            free_context(context);
            free_key(key);
            verified
        }
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
            || fips_mode() == 0
        {
            return 0;
        }
        // SAFETY: the emitter passes readable spans of these lengths. The key
        // and the signature have the nonzero lengths checked above, and an
        // empty message is not taken from the guest's address.
        let (key, message, signature) = unsafe {
            (
                slice::from_raw_parts(key as *const u8, key_len as usize),
                if message_len == 0 {
                    empty()
                } else {
                    slice::from_raw_parts(message as *const u8, message_len as usize)
                },
                slice::from_raw_parts(signature as *const u8, signature_len as usize),
            )
        };
        u64::from(verified(algorithm, key, message, signature))
    }
}
