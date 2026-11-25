//! Native P-384 agreement through the pinned AWS-LC provider.

use core::{
    ffi::{c_int, c_void},
    ptr::{self, NonNull},
};

use super::{digest::fips_mode, p384_slots, pkix::ffi::{EcGroup, EcKey, P384, ec_group}};

const POINT_BYTES: usize = 97;
const SECRET_BYTES: usize = 48;
const UNCOMPRESSED: c_int = 4;
const INVALID_PEER: u64 = 1;
const WRONG_LENGTH: u64 = 2;
const PROVIDER_FAILURE: u64 = 4;

#[repr(transparent)]
struct EcPoint(c_void);

unsafe extern "C" {
    #[link_name = "aws_lc_fips_0_14_4_EC_KEY_new_by_curve_name"]
    fn key_new(curve: c_int) -> *mut EcKey;
    #[link_name = "aws_lc_fips_0_14_4_EC_KEY_free"]
    fn key_free(key: *mut EcKey);
    #[link_name = "aws_lc_fips_0_14_4_EC_KEY_generate_key_fips"]
    fn key_generate(key: *mut EcKey) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EC_KEY_get0_public_key"]
    fn key_public(key: *const EcKey) -> *const EcPoint;
    #[link_name = "aws_lc_fips_0_14_4_EC_POINT_new"]
    fn point_new(group: *const EcGroup) -> *mut EcPoint;
    #[link_name = "aws_lc_fips_0_14_4_EC_POINT_free"]
    fn point_free(point: *mut EcPoint);
    #[link_name = "aws_lc_fips_0_14_4_EC_POINT_oct2point"]
    fn point_parse(group: *const EcGroup, point: *mut EcPoint, bytes: *const u8,
        len: usize, context: *mut c_void) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_EC_POINT_point2oct"]
    fn point_write(group: *const EcGroup, point: *const EcPoint, format: c_int,
        output: *mut u8, len: usize, context: *mut c_void) -> usize;
    #[link_name = "aws_lc_fips_0_14_4_ECDH_compute_key"]
    fn compute(output: *mut c_void, len: usize, peer: *const EcPoint, private: *const EcKey,
        kdf: Option<unsafe extern "C" fn(*const c_void, usize, *mut c_void, *mut usize) -> *mut c_void>) -> c_int;
    #[link_name = "aws_lc_fips_0_14_4_OPENSSL_cleanse"]
    fn cleanse(bytes: *mut c_void, len: usize);
    #[link_name = "aws_lc_fips_0_14_4_ERR_clear_error"]
    fn clear_error();
}

pub(super) struct Key(NonNull<EcKey>);

// The provider object has one Rust owner. The slot table serializes access
// until the caller takes ownership, and no operation publishes a key pointer.
unsafe impl Send for Key {}

impl Drop for Key {
    fn drop(&mut self) {
        unsafe { key_free(self.0.as_ptr()) };
    }
}

struct Point(NonNull<EcPoint>);
impl Drop for Point {
    fn drop(&mut self) {
        unsafe { point_free(self.0.as_ptr()) };
    }
}

struct Shared([u8; SECRET_BYTES]);
impl Drop for Shared {
    fn drop(&mut self) {
        unsafe { cleanse(self.0.as_mut_ptr().cast(), self.0.len()) };
    }
}

struct ClearErrors;
impl Drop for ClearErrors {
    fn drop(&mut self) {
        unsafe { clear_error() };
    }
}

impl Key {
    fn generate() -> Result<Self, u64> {
        if unsafe { fips_mode() } != 1 {
            return Err(PROVIDER_FAILURE);
        }
        let key = Self(NonNull::new(unsafe { key_new(P384) }).ok_or(PROVIDER_FAILURE)?);
        if unsafe { key_generate(key.0.as_ptr()) } != 1 {
            return Err(PROVIDER_FAILURE);
        }
        Ok(key)
    }

    fn public(&self) -> Result<[u8; POINT_BYTES], u64> {
        let mut bytes = [0; POINT_BYTES];
        let key = self.0.as_ptr();
        let count = unsafe {
            point_write(ec_group(key), key_public(key), UNCOMPRESSED, bytes.as_mut_ptr(),
                bytes.len(), ptr::null_mut())
        };
        if count != bytes.len() || bytes[0] != UNCOMPRESSED as u8 {
            return Err(PROVIDER_FAILURE);
        }
        Ok(bytes)
    }

    fn shared(&self, peer: &[u8; POINT_BYTES], shared: &mut Shared) -> Result<(), u64> {
        if peer[0] != UNCOMPRESSED as u8 {
            return Err(INVALID_PEER);
        }
        let key = self.0.as_ptr();
        let group = unsafe { ec_group(key) };
        let point = Point(NonNull::new(unsafe { point_new(group) }).ok_or(PROVIDER_FAILURE)?);
        // The fixed uncompressed encoding excludes infinity. AWS-LC rejects
        // off-curve coordinates when it decodes the point.
        if unsafe { point_parse(group, point.0.as_ptr(), peer.as_ptr(), peer.len(), ptr::null_mut()) } != 1 {
            return Err(INVALID_PEER);
        }
        let written = unsafe {
            compute(shared.0.as_mut_ptr().cast(), shared.0.len(), point.0.as_ptr(), key, None)
        };
        if written != SECRET_BYTES as c_int {
            return Err(PROVIDER_FAILURE);
        }
        Ok(())
    }
}

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

fn require_rand(grant: u64) {
    if grant != 32 {
        trap();
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_p384_start(grant: u64) -> u64 {
    require_rand(grant);
    let _errors = ClearErrors;
    p384_slots::with(|slots| {
        let Some(key_slot) = slots.available() else { return 0 };
        let Ok(key) = Key::generate() else { return 0 };
        slots.insert(key_slot, key)
    })
}

/// The emitter supplies the writable output span for this call.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_p384_public(handle: u64, output: *mut u8, length: usize) -> u64 {
    if length != POINT_BYTES {
        return WRONG_LENGTH;
    }
    let _errors = ClearErrors;
    let public = p384_slots::with(|slots| slots.get(handle).ok_or(PROVIDER_FAILURE)?.public());
    match public {
        Ok(point) => {
            unsafe { ptr::copy_nonoverlapping(point.as_ptr(), output, point.len()) };
            0
        }
        Err(code) => code,
    }
}

/// The emitter supplies the spans. The key is consumed even on invalid input.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_p384_finish(
    handle: u64, peer: *const u8, peer_len: usize, output: *mut u8, length: usize,
) -> u64 {
    let Some(key) = p384_slots::with(|slots| slots.take(handle)) else { trap() };
    if peer_len != POINT_BYTES || length != SECRET_BYTES {
        return WRONG_LENGTH;
    }
    let _errors = ClearErrors;
    let mut shared = Shared([0; SECRET_BYTES]);
    let result = key.shared(unsafe { &*peer.cast::<[u8; POINT_BYTES]>() }, &mut shared);
    if let Err(code) = result {
        return code;
    }
    unsafe { ptr::copy_nonoverlapping(shared.0.as_ptr(), output, shared.0.len()) };
    0
}

#[unsafe(no_mangle)]
extern "C" fn witchy_p384_close(handle: u64) -> u64 {
    if handle != 0 && p384_slots::with(|slots| slots.take(handle)).is_none() {
        trap();
    }
    0
}

/// Both outputs stay intact until peer validation and agreement succeed.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_p384_agree(
    grant: u64, peer: *const u8, peer_len: usize, public: *mut u8, public_len: usize,
    secret: *mut u8, secret_len: usize,
) -> u64 {
    require_rand(grant);
    if peer_len != POINT_BYTES || public_len != POINT_BYTES || secret_len != SECRET_BYTES {
        return WRONG_LENGTH;
    }
    if public.addr() < secret.addr().saturating_add(secret_len)
        && secret.addr() < public.addr().saturating_add(public_len)
    {
        trap();
    }
    let _errors = ClearErrors;
    let key = match Key::generate() {
        Ok(key) => key,
        Err(code) => return code,
    };
    let point = match key.public() {
        Ok(point) => point,
        Err(code) => return code,
    };
    let mut shared = Shared([0; SECRET_BYTES]);
    if let Err(code) = key.shared(unsafe { &*peer.cast::<[u8; POINT_BYTES]>() }, &mut shared) {
        return code;
    }
    unsafe {
        ptr::copy_nonoverlapping(point.as_ptr(), public, point.len());
        ptr::copy_nonoverlapping(shared.0.as_ptr(), secret, shared.0.len());
    }
    0
}
