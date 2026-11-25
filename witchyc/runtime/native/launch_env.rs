//! Environment grants are snapshotted at binding time.

use super::env_names;
use super::global::{Global, Table};
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

const BINDINGS: usize = 256;

/// The granted names, each with the value it had when it was bound.
static ENTRIES: Table<Binding, BINDINGS> = Table::new(Binding {
    name: [0; 1025],
    len: 0,
    value: None,
});
/// The name the guest is spelling, a byte at a time.
static QUERY: Table<u8, 1025> = Table::new(0);
/// The value of the name the guest looked up last.
static SELECTED: Global<Option<&'static [u8]>> = Global::new(None);

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
pub(super) fn witchy_bind_env(option: &CStr) -> bool {
    let text = option.to_bytes();
    let (name, inline) = match text.iter().position(|&byte| byte == b'=') {
        Some(index) => {
            let Some((name, tail)) = text.split_at_checked(index) else {
                return false;
            };
            (name, tail.get(1..))
        }
        None => (text, None),
    };
    if !env_names::valid(name) || ENTRIES.iter().any(|entry| matches(&entry, name)) {
        return false;
    }
    if ENTRIES.len() == BINDINGS {
        return false;
    }
    let value = match inline {
        Some(value) => Some(value),
        None => {
            let raw = unsafe { getenv(option.as_ptr()) };
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
    ENTRIES.push(Binding {
        name: stored,
        len: name.len(),
        value,
    })
}

fn check(valid: bool) {
    if !valid {
        unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_env_name_byte(grant: u64, byte: u64) {
    check(grant == ENV && byte <= 255);
    // A byte past the longest name is dropped.
    QUERY.push(byte as u8);
}

#[unsafe(no_mangle)]
extern "C" fn witchy_env_lookup(grant: u64) -> u64 {
    check(grant == ENV);
    let mut query = [0; 1025];
    let mut len = 0;
    for (out, byte) in query.iter_mut().zip(QUERY.iter()) {
        *out = byte;
        len += 1;
    }
    QUERY.clear();
    let selected = query.get(..len).and_then(|name| {
        ENTRIES
            .iter()
            .find(|entry| matches(entry, name))
            .and_then(|entry| entry.value)
    });
    SELECTED.set(selected);
    selected.map_or(u64::MAX, |value| value.len() as u64)
}

#[unsafe(no_mangle)]
extern "C" fn witchy_env_value_byte(grant: u64, index: usize) -> u64 {
    check(grant == ENV);
    SELECTED
        .get()
        .and_then(|value| value.get(index))
        .map_or(256, |&byte| u64::from(byte))
}

/// The callback consumes the name synchronously and does not mutate bindings.
#[cfg(witchy_native_aws_lc)]
fn overlaps_secret(check: fn(&CStr) -> bool) -> bool {
    ENTRIES.iter().any(|entry| {
        // Each stored name has a reserved trailing NUL.
        CStr::from_bytes_until_nul(&entry.name).is_ok_and(check)
    })
}

/// `stderr` is the launcher's diagnostic stream.
#[cfg(witchy_native_aws_lc)]
pub(super) fn witchy_validate_secret_grants(
    required: u64,
    supplied: u64,
    stderr: super::sys::Stream,
) -> bool {
    use super::signer_crypto::witchy_secret_uses_env;
    unsafe extern "C" {
        safe fn witchy_signers_bound() -> bool;
    }
    const SIGNER: u64 = 32768;
    const SECRET_STORE: u64 = 65536;
    const EXEC: u64 = 262144;
    const TLS_SIGNER: u64 = super::tls_grant::TLS_SIGNER;
    const WRITES: u64 = 2048 | 4096 | 8192;
    let error = if overlaps_secret(witchy_secret_uses_env) {
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
    stderr.put(error);
    false
}
