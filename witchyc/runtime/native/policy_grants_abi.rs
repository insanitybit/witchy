use super::policy_grants::Grants;
use core::ffi::{CStr, c_char};

const POLICY_GRANTS: u64 = 16384;

// The launcher binds grants before entry. Guest lookups run on its thread
// and cannot suspend inside an ABI operation.
static mut GRANTS: Grants = Grants::new();

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_bind_policy_grant(text: *const c_char) -> bool {
    let text = unsafe { CStr::from_ptr(text) }.to_bytes();
    unsafe { (&mut *(&raw mut GRANTS)).bind(text).is_some() }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_grant_name(grant: u64, byte: u64) {
    let Ok(byte) = u8::try_from(byte) else { trap() };
    if grant != POLICY_GRANTS {
        trap()
    }
    unsafe { (&mut *(&raw mut GRANTS)).push(byte) };
}

#[unsafe(no_mangle)]
extern "C" fn witchy_grant_lookup(grant: u64) -> u64 {
    if grant != POLICY_GRANTS {
        trap()
    }
    match unsafe { (&mut *(&raw mut GRANTS)).lookup() } {
        Some(value) => value,
        None => trap(),
    }
}
