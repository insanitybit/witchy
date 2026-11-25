//! Native file identity syntax shared by input and directory grants.

#[derive(Debug, PartialEq, Eq)]
#[repr(C)]
pub(super) struct Identity {
    pub(super) device: u64,
    pub(super) inode: u64,
}

fn decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u64, |value, &byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))
    })
}

pub(super) fn parse(text: &[u8]) -> Option<Identity> {
    let split = text.iter().position(|&byte| byte == b':')?;
    let (device, tail) = text.split_at_checked(split)?;
    Some(Identity {
        device: decimal(device)?,
        inode: decimal(tail.get(1..)?)?,
    })
}

/// The launcher provides a live NUL-terminated argument and writable output.
#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_parse_file_identity(
    text: *const core::ffi::c_char,
    out: *mut Identity,
) -> bool {
    let bytes = unsafe { core::ffi::CStr::from_ptr(text) }.to_bytes();
    let Some(identity) = parse(bytes) else {
        return false;
    };
    unsafe { out.write(identity) };
    true
}

#[cfg(test)]
#[test]
fn parses_exact_decimal_pair() {
    assert_eq!(
        parse(b"0001:18446744073709551615"),
        Some(Identity {
            device: 1,
            inode: u64::MAX
        })
    );
    for invalid in [
        b":1".as_slice(),
        b"1:",
        b"+1:2",
        b"1:-2",
        b"1:2:3",
        b"1:2 ",
        b"18446744073709551616:0",
    ] {
        assert_eq!(parse(invalid), None);
    }
}
