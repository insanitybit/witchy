#![no_std]

#[unsafe(no_mangle)]
static witchy_entry_grants: u64 = 0;
#[unsafe(no_mangle)]
static witchy_launch_plan: u8 = 0;
#[unsafe(no_mangle)]
static witchy_secret_slots: u8 = 0;

unsafe extern "C" {
    fn _exit(status: core::ffi::c_int) -> !;
    fn witchy_pkix_verify(
        roots: *const u8, roots_len: usize, chain: *const u8, chain_len: usize,
        name: *const u8, name_len: usize, signed: *const u8, signed_len: usize,
        signature: *const u8, signature_len: usize,
    ) -> u64;
}

const ROOTS: &[u8] = include_bytes!("root.bundle");
const CHAIN: &[u8] = include_bytes!("leaf.bundle");
const SIGNED: &[u8] = include_bytes!("signed.bin");
const SIGNATURE: &[u8] = include_bytes!("signature.der");

fn verify(name: &[u8], signature: &[u8]) -> u64 {
    unsafe {
        witchy_pkix_verify(ROOTS.as_ptr(), ROOTS.len(), CHAIN.as_ptr(), CHAIN.len(),
            name.as_ptr(), name.len(), SIGNED.as_ptr(), SIGNED.len(),
            signature.as_ptr(), signature.len())
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_entry(_: u64) {
    let mut trailing = [0; SIGNATURE.len() + 1];
    trailing[..SIGNATURE.len()].copy_from_slice(SIGNATURE);
    for (name, signature, expected, status) in [
        (b"api.example.test".as_slice(), SIGNATURE, 1, 31),
        (b"other.example.test", SIGNATURE, 0, 32),
        (b".example.test", SIGNATURE, 0, 33),
        (b"api.example.test", trailing.as_slice(), 0, 34),
    ] {
        if verify(name, signature) != expected {
            unsafe { _exit(status) };
        }
    }
}
