use super::global::State;
use super::policy_grants::Grants;
use core::ffi::CStr;

const POLICY_GRANTS: u64 = 16384;

// The launcher binds grants before entry. Guest lookups run on its thread
// and cannot suspend inside an ABI operation. Each use is one call into
// `policy_grants`, which calls nothing outside itself, so no use can begin
// inside another.
static GRANTS: State<Grants> = State::new(Grants::new());

fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

pub(super) fn witchy_bind_policy_grant(text: &CStr) -> bool {
    GRANTS.with(|grants| grants.bind(text.to_bytes()).is_some())
}

#[unsafe(no_mangle)]
extern "C" fn witchy_grant_name(grant: u64, byte: u64) {
    let Ok(byte) = u8::try_from(byte) else { trap() };
    if grant != POLICY_GRANTS {
        trap()
    }
    GRANTS.with(|grants| grants.push(byte));
}

#[unsafe(no_mangle)]
extern "C" fn witchy_grant_lookup(grant: u64) -> u64 {
    if grant != POLICY_GRANTS {
        trap()
    }
    match GRANTS.with(|grants| grants.lookup()) {
        Some(value) => value,
        None => trap(),
    }
}
