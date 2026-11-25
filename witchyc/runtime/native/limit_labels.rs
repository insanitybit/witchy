//! Operator-facing labels for native runtime limits.

use core::ffi::CStr;

const FLAGS: [&[u8]; 9] = [
    b"--output-bytes",
    b"--output-bytes-per-connection",
    b"--deadline-ms",
    b"--net-bytes",
    b"--net-bytes-per-connection",
    b"--input-bytes",
    b"--fs-bytes",
    b"--fs-entries",
    b"--connection-deadline-ms",
];

fn index(flag: &[u8]) -> Option<usize> {
    FLAGS.iter().position(|&known| known == flag)
}

fn parse(input: &CStr) -> Option<(usize, &CStr)> {
    let bytes = input.to_bytes();
    let equal = bytes.iter().position(|&byte| byte == b'=')?;
    let flag = index(bytes.get(..equal)?)?;
    let suffix = input.to_bytes_with_nul().get(equal + 1..)?;
    // A suffix of a CStr retains its sole terminating NUL.
    let label = unsafe { CStr::from_bytes_with_nul_unchecked(suffix) };
    let text = label.to_bytes();
    if !(1..=48).contains(&text.len())
        || !text.iter().all(|&byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b' ' | b'.' | b'_' | b'-' | b'[' | b']')
        })
    {
        return None;
    }
    Some((flag, label))
}

// The launcher binds labels before any worker starts. argv lives through exit.
static mut LABELS: [Option<&'static CStr>; FLAGS.len()] = [None; FLAGS.len()];

pub(super) fn witchy_bind_limit_label(input: &'static CStr) -> bool {
    let Some((index, label)) = parse(input) else {
        return false;
    };
    let labels = unsafe { &mut *(&raw mut LABELS) };
    let Some(slot @ None) = labels.get_mut(index) else {
        return false;
    };
    *slot = Some(label);
    true
}

pub(super) fn witchy_limit_label(flag: &'static CStr) -> &'static CStr {
    let labels = unsafe { &*(&raw const LABELS) };
    index(flag.to_bytes())
        .and_then(|slot| labels.get(slot).copied().flatten())
        .unwrap_or(flag)
}
