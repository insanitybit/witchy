//! Operator-facing labels for native runtime limits.

use super::global::Global;
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
    let label = CStr::from_bytes_with_nul(suffix).ok()?;
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
static LABELS: Global<[Option<&'static CStr>; FLAGS.len()]> = Global::new([None; FLAGS.len()]);

pub(super) fn witchy_bind_limit_label(input: &'static CStr) -> bool {
    let Some((index, label)) = parse(input) else {
        return false;
    };
    let mut labels = LABELS.get();
    let Some(slot @ None) = labels.get_mut(index) else {
        return false;
    };
    *slot = Some(label);
    LABELS.set(labels);
    true
}

pub(super) fn witchy_limit_label(flag: &'static CStr) -> &'static CStr {
    let labels = LABELS.get();
    index(flag.to_bytes())
        .and_then(|slot| labels.get(slot).copied().flatten())
        .unwrap_or(flag)
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// A bound label belongs to a known flag, is the text after the first `=`,
    /// and is one to 48 plain bytes, for every argument of up to 61 bytes.
    #[kani::proof]
    #[kani::unwind(64)]
    fn a_label_is_short_plain_text() {
        let mut bytes: [u8; 62] = kani::any();
        bytes[61] = 0;
        let Ok(input) = CStr::from_bytes_until_nul(&bytes) else {
            return;
        };
        let parsed = parse(input);
        // `--fs-bytes=` and 49 bytes: one more than a label may have.
        kani::cover!(parsed.is_none() && bytes[10] == b'=' && input.to_bytes().len() == 60);
        if let Some((flag, label)) = parsed {
            let text = label.to_bytes();
            kani::cover!(text.len() == 48);
            kani::cover!(flag == 8);
            assert!(flag < FLAGS.len() && (1..=48).contains(&text.len()));
            let equal = FLAGS[flag].len();
            assert!(bytes[equal] == b'=' && text.as_ptr() == bytes[equal + 1..].as_ptr());
            let at: usize = kani::any();
            kani::assume(at < text.len());
            assert!(text[at] >= b' ' && text[at] < 0x7f && text[at] != b'=');
        }
    }
}
