//! Environment grants are snapshotted at binding time.

use super::env_names;
use core::{
    ffi::{CStr, c_char, c_void},
    ptr,
};

unsafe extern "C" {
    fn getenv(name: *const c_char) -> *const c_char;
    fn malloc(size: usize) -> *mut c_void;
}

const ENV: u64 = 128;

#[derive(Clone, Copy)]
struct Binding {
    name: [u8; 1025],
    len: usize,
    value: Option<&'static [u8]>,
}

struct Environment {
    entries: [Option<Binding>; 256],
    query: [u8; 1025],
    len: usize,
    selected: Option<&'static [u8]>,
}

static mut ENVIRONMENT: Environment = Environment {
    entries: [None; 256],
    query: [0; 1025],
    len: 0,
    selected: None,
};

fn matches(binding: &Binding, name: &[u8]) -> bool {
    binding.name.get(..binding.len) == Some(name)
}

fn snapshot(value: &[u8]) -> Option<&'static [u8]> {
    if value.is_empty() {
        return Some(&[]);
    }
    let target = unsafe { malloc(value.len()) }.cast::<u8>();
    if target.is_null() {
        return None;
    }
    // Immutable grant storage lives until process exit.
    unsafe {
        ptr::copy_nonoverlapping(value.as_ptr(), target, value.len());
        Some(core::slice::from_raw_parts(target, value.len()))
    }
}

// The launcher and guest share a single thread and these operations do not suspend.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_bind_env(option: *const c_char) -> bool {
    let text = unsafe { CStr::from_ptr(option) }.to_bytes();
    let (name, inline) = match text.iter().position(|&byte| byte == b'=') {
        Some(index) => {
            let Some((name, tail)) = text.split_at_checked(index) else {
                return false;
            };
            (name, tail.get(1..))
        }
        None => (text, None),
    };
    let env = unsafe { &mut *(&raw mut ENVIRONMENT) };
    if !env_names::valid(name)
        || env
            .entries
            .iter()
            .flatten()
            .any(|entry| matches(entry, name))
    {
        return false;
    }
    let Some(slot) = env.entries.iter_mut().find(|entry| entry.is_none()) else {
        return false;
    };
    let value = match inline {
        Some(value) => Some(value),
        None => {
            let raw = unsafe { getenv(option) };
            if raw.is_null() {
                None
            } else {
                Some(unsafe { CStr::from_ptr(raw) }.to_bytes())
            }
        }
    };
    let value = if let Some(value) = value {
        if value.len() > 65536 {
            return false;
        }
        let Some(copy) = snapshot(value) else {
            return false;
        };
        Some(copy)
    } else {
        None
    };
    let mut stored = [0; 1025];
    for (out, byte) in stored.iter_mut().zip(name) {
        *out = *byte;
    }
    *slot = Some(Binding {
        name: stored,
        len: name.len(),
        value,
    });
    true
}

fn check(valid: bool) {
    if !valid {
        unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_env_name_byte(grant: u64, byte: u64) {
    check(grant == ENV && byte <= 255);
    let env = unsafe { &mut *(&raw mut ENVIRONMENT) };
    if let Some(next) = env.query.get_mut(env.len) {
        *next = byte as u8;
        env.len += 1;
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_env_lookup(grant: u64) -> u64 {
    check(grant == ENV);
    let env = unsafe { &mut *(&raw mut ENVIRONMENT) };
    let len = core::mem::take(&mut env.len);
    env.selected = env.query.get(..len).and_then(|name| {
        env.entries
            .iter()
            .flatten()
            .find(|entry| matches(entry, name))
            .and_then(|entry| entry.value)
    });
    env.selected.map_or(u64::MAX, |value| value.len() as u64)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_env_value_byte(grant: u64, index: usize) -> u64 {
    check(grant == ENV);
    unsafe { ENVIRONMENT.selected }
        .and_then(|value| value.get(index))
        .map_or(256, |&byte| u64::from(byte))
}

/// The callback consumes the name synchronously and does not mutate bindings.
#[cfg(witchy_native_aws_lc)]
unsafe fn overlaps_secret(check: unsafe extern "C" fn(*const c_char) -> bool) -> bool {
    let env = unsafe { &*(&raw const ENVIRONMENT) };
    env.entries.iter().flatten().any(|entry| {
        // Each stored name has a reserved trailing NUL.
        unsafe { check(entry.name.as_ptr().cast()) }
    })
}

#[cfg(witchy_native_aws_lc)]
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_validate_secret_grants(
    required: u64,
    supplied: u64,
    stderr: *mut core::ffi::c_void,
) -> bool {
    unsafe extern "C" {
        fn witchy_secret_uses_env(name: *const c_char) -> bool;
        safe fn witchy_signers_bound() -> bool;
        fn fputs(text: *const c_char, stream: *mut core::ffi::c_void) -> core::ffi::c_int;
    }
    const SIGNER: u64 = 32768;
    const SECRET_STORE: u64 = 65536;
    const EXEC: u64 = 262144;
    const TLS_SIGNER: u64 = super::tls_grant::TLS_SIGNER;
    const WRITES: u64 = 2048 | 4096 | 8192;
    let error = if unsafe { overlaps_secret(witchy_secret_uses_env) } {
        c"native secret source is also granted through Env\n"
    } else if (required & TLS_SIGNER != 0) && (required | supplied) & (EXEC | WRITES) != 0 {
        c"TLS signers require a launch without Exec or writable file and directory grants\n"
    } else if (required & (SIGNER | TLS_SIGNER) != 0 && !witchy_signers_bound())
        || (required & SECRET_STORE != 0 && supplied & SECRET_STORE == 0)
        || (required & (SIGNER | SECRET_STORE) != 0 && required & EXEC != 0)
    {
        c"native secrets require all slot bindings and cannot be combined with Exec\n"
    } else {
        return true;
    };
    unsafe { fputs(error.as_ptr(), stderr) };
    false
}
