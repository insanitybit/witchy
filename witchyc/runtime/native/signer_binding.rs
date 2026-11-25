//! Byte syntax for named native signer slots. The parser performs no I/O.

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Binding {
    pub(super) name_len: usize,
    pub(super) kind: u32,
    pub(super) role_start: usize,
    pub(super) role_len: usize,
    pub(super) source_start: usize,
    pub(super) environment: bool,
}

pub(super) fn parse(text: &[u8]) -> Option<Binding> {
    let name_len = text.iter().position(|&byte| byte == b':')?;
    let use_start = name_len + 1;
    let usage = text.get(use_start..)?;
    let (kind, role_start, role_len, source_start) = if usage.starts_with(b"reveal-bytes:") {
        (1, 0, 0, use_start + b"reveal-bytes:".len())
    } else if usage.starts_with(b"seal:") {
        (2, 0, 0, use_start + b"seal:".len())
    } else if usage.starts_with(b"sign/tls13/server-p384:") {
        (4, 0, 0, use_start + b"sign/tls13/server-p384:".len())
    } else {
        let prefix = b"sign/witchy-2/";
        let rest = usage.strip_prefix(prefix)?;
        let role_len = rest.iter().position(|&byte| byte == b':')?;
        let role = rest.get(..role_len)?;
        if role.is_empty()
            || role.len() > 32
            || !role
                .iter()
                .all(|&byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return None;
        }
        let role_start = use_start + prefix.len();
        (3, role_start, role_len, role_start + role_len + 1)
    };
    let source = text.get(source_start..)?;
    let (environment, source_start) = if let Some(name) = source.strip_prefix(b"env:") {
        if kind >= 3 || name.len() > 64 || !super::env_names::valid(name) {
            return None;
        }
        (true, source_start + 4)
    } else if source.starts_with(b"fd:") {
        (false, source_start + 3)
    } else {
        return None;
    };
    Some(Binding {
        name_len,
        kind,
        role_start,
        role_len,
        source_start,
        environment,
    })
}

#[cfg(test)]
#[test]
fn signer_binding_syntax_and_source_bounds() {
    let sign = b"targets:sign/witchy-2/release-1:fd:42";
    let binding = parse(sign).unwrap();
    assert_eq!(binding.kind, 3);
    assert_eq!(&sign[..binding.name_len], b"targets");
    assert_eq!(
        &sign[binding.role_start..][..binding.role_len],
        b"release-1"
    );
    assert_eq!(&sign[binding.source_start..], b"42");
    let reveal = b"token:reveal-bytes:env:_KEY0";
    let binding = parse(reveal).unwrap();
    assert!(binding.environment);
    assert_eq!(&reveal[binding.source_start..], b"_KEY0");
    let tls = b"issuer:sign/tls13/server-p384:fd:5";
    let binding = parse(tls).unwrap();
    assert_eq!(binding.kind, 4);
    assert!(!binding.environment);
    assert_eq!(&tls[binding.source_start..], b"5");
    for invalid in [
        &b"slot:sign/witchy-2/up:env:KEY"[..],
        b"slot:sign/tls13/server-p384:env:KEY",
        b"slot:sign/witchy-2/:fd:4",
        b"slot:sign/witchy-2/Upper:fd:4",
        b"slot:seal:env:1KEY",
        b"slot:seal:env:",
        b"slot:seal:unknown:4",
    ] {
        assert!(parse(invalid).is_none());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(super) enum StoreSource {
    Environment,
    Literal,
    Descriptor,
}

fn store_binding(text: &[u8]) -> Option<(usize, usize, StoreSource)> {
    let name_len = text
        .iter()
        .take_while(|&&byte| byte.is_ascii_alphanumeric() || byte == b'_')
        .count();
    if !(1..=64).contains(&name_len) {
        return None;
    }
    let suffix = text.get(name_len..)?;
    if suffix.is_empty() {
        Some((name_len, name_len, StoreSource::Environment))
    } else if suffix.starts_with(b"=") {
        Some((name_len, name_len + 1, StoreSource::Literal))
    } else if suffix.starts_with(b"@fd:") {
        Some((name_len, name_len + 4, StoreSource::Descriptor))
    } else {
        None
    }
}

#[cfg(not(test))]
struct StoreGrant {
    name: [u8; 65],
    len: usize,
    reveal: bool,
}

#[cfg(not(test))]
static mut STORE_GRANTS: [StoreGrant; 64] = [const {
    StoreGrant {
        name: [0; 65],
        len: 0,
        reveal: false,
    }
}; 64];

#[cfg(not(test))]
pub(super) fn store_lookup(name: &[u8], reveal: bool) -> core::ffi::c_int {
    let grants = unsafe { &*(&raw const STORE_GRANTS) };
    for (index, grant) in grants
        .iter()
        .enumerate()
        .take_while(|(_, grant)| grant.len != 0)
    {
        if grant.name.get(..grant.len) == Some(name) {
            return if reveal && !grant.reveal {
                -2
            } else {
                index as core::ffi::c_int
            };
        }
    }
    -1
}

/// The argument is borrowed through the synchronous OS/provider loading step.
#[cfg(not(test))]
pub(super) fn witchy_bind_store_secret(input: &core::ffi::CStr, reveal: bool) -> bool {
    let bytes = input.to_bytes();
    let Some((len, start, kind)) = store_binding(bytes) else {
        return false;
    };
    // The source starts at or before the terminating NUL.
    let Some(source) = input.to_bytes_with_nul().get(start..) else {
        return false;
    };
    let Some(name) = bytes.get(..len) else {
        return false;
    };
    if store_lookup(name, false) != -1 {
        return false;
    }
    let grants = unsafe { &mut *(&raw mut STORE_GRANTS) };
    let Some((index, grant)) = grants
        .iter_mut()
        .enumerate()
        .find(|(_, grant)| grant.len == 0)
    else {
        return false;
    };
    grant.name = [0; 65];
    for (out, &byte) in grant.name.iter_mut().zip(name) {
        *out = byte;
    }
    if !unsafe {
        super::signer_crypto::load_store_secret(
            index,
            grant.name.as_ptr().cast(),
            source.as_ptr().cast(),
            kind,
        )
    } {
        return false;
    }
    grant.len = len;
    grant.reveal = reveal;
    true
}

#[cfg(test)]
#[test]
fn store_binding_preserves_name_and_source_boundaries() {
    use StoreSource::*;
    assert_eq!(store_binding(b"0_KEY"), Some((5, 5, Environment)));
    assert_eq!(store_binding(b"key="), Some((3, 4, Literal)));
    assert_eq!(store_binding(b"key@fd:42"), Some((3, 7, Descriptor)));
    assert_eq!(store_binding(b"key:fd:42"), None);
    assert_eq!(store_binding(b"=value"), None);
    assert!(store_binding(&[b'a'; 64]).is_some());
    assert_eq!(store_binding(&[b'a'; 65]), None);
}

fn secret_slot(handle: u64, operation: u32) -> Option<usize> {
    let slot = handle / 16;
    let tag = handle % 16;
    let allowed = match operation {
        0 => tag == 0 && matches!(slot, 1..=64 | 129..=256),
        1 => tag <= 1 && matches!(slot, 1..=64 | 129..=256),
        2 => tag == 2 && matches!(slot, 1..=64),
        3 => tag == 3 && matches!(slot, 1..=64),
        _ => false,
    };
    allowed.then(|| (slot - 1) as usize)
}

#[cfg(not(test))]
fn trap() -> ! {
    unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
pub(super) extern "C" fn witchy_native_secret_slot(handle: u64, operation: u32) -> usize {
    secret_slot(handle, operation).unwrap_or_else(|| trap())
}

#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn witchy_secret_read(handle: u64, index: u64, reading: u64) -> u64 {
    if reading == 0 && matches!(handle, 8193 | 8194) {
        return handle - 8192;
    }
    let slot = witchy_native_secret_slot(handle, 0);
    let slot = unsafe { &*super::signer_crypto::slot_ptr(slot) };
    let bytes = slot.revealed;
    if bytes.is_null() {
        trap();
    }
    if reading == 0 {
        return 0;
    }
    if index >= slot.length as u64 {
        return 256;
    }
    // The slot holds a live allocation for all bytes below its length.
    u64::from(unsafe { bytes.add(index as usize).read() })
}

#[cfg(test)]
#[test]
fn secret_handles_admit_only_the_operation_slot_and_tag() {
    for slot in 0..=257 {
        for tag in 0..16 {
            let handle = slot * 16 + tag;
            let direct = (1..=64).contains(&slot);
            let issued = (129..=256).contains(&slot);
            assert_eq!(
                secret_slot(handle, 0).is_some(),
                (direct || issued) && tag == 0
            );
            assert_eq!(
                secret_slot(handle, 1).is_some(),
                (direct || issued) && tag <= 1
            );
            assert_eq!(secret_slot(handle, 2).is_some(), direct && tag == 2);
            assert_eq!(secret_slot(handle, 3).is_some(), direct && tag == 3);
        }
    }
    assert_eq!(secret_slot(u64::MAX, 0), None);
    assert_eq!(secret_slot(16, 3), None);
}
