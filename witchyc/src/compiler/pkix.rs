//! In-memory PKIX and TLS signature policy shared by both targets.

#[path = "pkix/ffi.rs"]
pub(crate) mod ffi;

use core::ptr::{NonNull, null_mut};

const MAX_ROOTS: usize = 512 * 1024;
const MAX_CHAIN: usize = 64 * 1024;
const MAX_SIGNED: usize = 256;
const MAX_NAME: usize = 253;
const MIN_SIGNATURE: usize = 8;
const MAX_SIGNATURE: usize = 104;

macro_rules! owned {
    ($name:ident, $raw:ty, $free:path) => {
        struct $name(NonNull<$raw>);
        impl Drop for $name {
            fn drop(&mut self) {
                unsafe { $free(self.0.as_ptr()) };
            }
        }
    };
}

owned!(Certificate, ffi::X509, ffi::free_certificate);
owned!(Store, ffi::Store, ffi::free_store);
owned!(Context, ffi::Context, ffi::free_context);
owned!(Stack, ffi::Stack, ffi::free_stack);
owned!(DigestContext, ffi::DigestContext, ffi::free_digest_context);

fn certificates<const N: usize>(mut bytes: &[u8]) -> Option<[Option<Certificate>; N]> {
    let mut chain = [const { None }; N];
    for slot in &mut chain {
        if bytes.is_empty() { break; }
        let ([a, b, c], tail) = bytes.split_first_chunk::<3>()?;
        let len = usize::from(*a) << 16 | usize::from(*b) << 8 | usize::from(*c);
        if len == 0 { return None; }
        let (der, rest) = tail.split_at_checked(len)?;
        let mut cursor = der.as_ptr();
        let parsed = unsafe { ffi::parse(null_mut(), &mut cursor, len.try_into().ok()?) };
        let cert = Certificate(NonNull::new(parsed)?);
        if cursor != der.as_ptr().wrapping_add(len) { return None; }
        *slot = Some(cert);
        bytes = rest;
    }
    (bytes.is_empty() && chain.first()?.is_some()).then_some(chain)
}

fn dns_name(host: &[u8]) -> bool {
    host.len() <= MAX_NAME && host.split(|&byte| byte == b'.').all(|label| {
        label.len() <= 63
            && label.first().is_some_and(u8::is_ascii_alphanumeric)
            && label.last().is_some_and(u8::is_ascii_alphanumeric)
            && label.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
    })
}

/// The adapters can refuse these sizes before projecting guest pointers.
pub(crate) fn bounded(roots: usize, chain: usize, host: usize, signed: usize, sig: usize) -> bool {
    (1..=MAX_ROOTS).contains(&roots) && (1..=MAX_CHAIN).contains(&chain)
        && (1..=MAX_NAME).contains(&host) && (9..=MAX_SIGNED).contains(&signed)
        && (MIN_SIGNATURE..=MAX_SIGNATURE).contains(&sig)
}

/// Verify using a caller-provided trust store, DNS identity and Unix time.
/// The caller must first check the selected provider is in FIPS mode.
pub(crate) fn verify(roots: &[u8], chain: &[u8], host: &[u8], signed: &[u8], sig: &[u8]) -> bool {
    if !bounded(roots.len(), chain.len(), host.len(), signed.len(), sig.len()) || !dns_name(host) {
        return false;
    }
    let Some((timestamp, message)) = signed.split_first_chunk::<8>() else { return false; };
    let seconds = i64::from_be_bytes(*timestamp);
    if !(0..=253402300799).contains(&seconds) {
        return false;
    }
    let result = verify_with_store(roots, chain, host, seconds, message, sig).is_some();
    unsafe { ffi::clear_error() };
    result
}

fn verify_with_store(roots: &[u8], chain: &[u8], host: &[u8], seconds: i64,
    message: &[u8], sig: &[u8]) -> Option<()> {
    let roots = certificates::<256>(roots)?;
    let peer = certificates::<8>(chain)?;
    let leaf = peer[0].as_ref()?.0.as_ptr();
    // The context borrows store and chain. Declare it last so it drops
    // first. The store holds its own certificate refs.
    unsafe {
        let store = Store(NonNull::new(ffi::new_store())?);
        for root in roots.iter().flatten() {
            if ffi::add_cert(store.0.as_ptr(), root.0.as_ptr()) != 1 { return None; }
        }
        let intermediates = Stack(NonNull::new(ffi::new_stack())?);
        for cert in peer[1..].iter().flatten() {
            if ffi::push(intermediates.0.as_ptr(), cert.0.as_ptr().cast()) == 0 { return None; }
        }

        let context = Context(NonNull::new(ffi::new_context())?);
        let ctx = context.0.as_ptr();
        if ffi::init_context(ctx, store.0.as_ptr(), leaf, intermediates.0.as_ptr()) != 1
            || ffi::set_purpose(ctx, ffi::SSL_SERVER) != 1
        {
            return None;
        }

        let params = NonNull::new(ffi::parameters(ctx))?.as_ptr();
        ffi::set_time(params, seconds);
        // AWS-LC's default wildcard handling refuses partial DNS labels.
        ffi::set_host_flags(params, ffi::NEVER_CHECK_SUBJECT);
        if ffi::set_host(params, host.as_ptr().cast(), host.len()) != 1
            || ffi::verify_chain(ctx) != 1
            || ffi::key_usage(leaf) & ffi::DIGITAL_SIGNATURE == 0
            || !strong_path(ctx)
        {
            return None;
        }

        let key = NonNull::new(ffi::public_key(leaf))?;
        if curve(key.as_ptr()) != Some(ffi::P384) { return None; }
        verify_signature(key, message, sig)
    }
}

unsafe fn verify_signature(key: NonNull<ffi::PublicKey>, message: &[u8], sig: &[u8]) -> Option<()> {
    unsafe {
        let digest = DigestContext(NonNull::new(ffi::new_digest_context())?);
        let ctx = digest.0.as_ptr();
        if ffi::digest_verify_init(ctx, null_mut(), ffi::sha384(), null_mut(), key.as_ptr()) != 1 {
            return None;
        }
        (ffi::digest_verify(ctx, sig.as_ptr(), sig.len(), message.as_ptr(), message.len()) == 1).then_some(())
    }
}

unsafe fn strong_path(context: *mut ffi::Context) -> bool {
    // The context owns the verified chain. The last element is its trust anchor.
    unsafe {
        let chain = ffi::verified_chain(context);
        if chain.is_null() { return false; }
        let count = ffi::count(chain);
        if count < 2 { return false; }
        for index in 0..count {
            let cert = ffi::element(chain, index).cast::<ffi::X509>();
            if cert.is_null() { return false; }
            let key = ffi::public_key(cert);
            if key.is_null() || !strong_key(key) { return false; }
            if index + 1 == count { break; }
            let mut digest = 0;
            if ffi::signature_info(cert, &mut digest, null_mut(), null_mut(), null_mut()) != 1
                || !matches!(digest, ffi::SHA256 | ffi::SHA384 | ffi::SHA512)
            {
                return false;
            }
        }
        true
    }
}

unsafe fn strong_key(key: *const ffi::PublicKey) -> bool {
    unsafe {
        match ffi::key_id(key) {
            ffi::RSA | ffi::RSA_PSS => (2048..=8192).contains(&ffi::key_bits(key)),
            ffi::EC => matches!(curve(key), Some(ffi::P256 | ffi::P384 | ffi::P521)),
            _ => false,
        }
    }
}

pub(crate) unsafe fn curve(key: *const ffi::PublicKey) -> Option<core::ffi::c_int> {
    unsafe {
        let ec = NonNull::new(ffi::ec_key(key))?;
        let group = NonNull::new(ffi::ec_group(ec.as_ptr()).cast_mut())?;
        Some(ffi::curve_name(group.as_ptr()))
    }
}
