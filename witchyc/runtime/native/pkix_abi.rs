//! Native adapter for the shared certificate verification policy.

use super::{digest::fips_mode, pkix};
use core::slice;

/// The emitter supplies five readable byte spans. Bounds are checked before
/// constructing Rust slices, and zero-length inputs are refused.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_pkix_verify(
    roots: *const u8, roots_len: usize, chain: *const u8, chain_len: usize,
    host: *const u8, host_len: usize, signed: *const u8, signed_len: usize,
    signature: *const u8, sig_len: usize,
) -> u64 {
    if !pkix::bounded(roots_len, chain_len, host_len, signed_len, sig_len)
        || fips_mode() != 1
    {
        return 0;
    }
    u64::from(unsafe {
        pkix::verify(
            slice::from_raw_parts(roots, roots_len),
            slice::from_raw_parts(chain, chain_len),
            slice::from_raw_parts(host, host_len),
            slice::from_raw_parts(signed, signed_len),
            slice::from_raw_parts(signature, sig_len),
        )
    })
}
