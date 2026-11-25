//! Validation of the NUL-delimited arguments supplied to a granted program.

/// Returns the byte offsets of exactly `count` bounded arguments.
pub(super) fn offsets(pending: &[u8], count: usize, out: &mut [usize; 64]) -> bool {
    if count > 64 || pending.len() > (1 << 20) || (count == 0 && !pending.is_empty()) {
        return false;
    }
    let mut at = 0;
    for slot in out.iter_mut().take(count) {
        if at >= pending.len() {
            return false;
        }
        let Some(end) = pending[at..].iter().position(|&byte| byte == 0) else {
            return false;
        };
        if end > 4096 {
            return false;
        }
        *slot = at;
        at += end + 1;
    }
    at == pending.len()
}

#[cfg(test)]
#[test]
fn requires_exact_terminated_bounded_arguments() {
    let mut offsets_out = [0; 64];
    assert!(offsets(b"one\0two\0", 2, &mut offsets_out));
    assert_eq!(&offsets_out[..2], &[0, 4]);
    assert!(offsets(b"\0", 1, &mut offsets_out));
    assert!(!offsets(b"one\0two\0", 1, &mut offsets_out));
    assert!(!offsets(b"one", 1, &mut offsets_out));
    assert!(!offsets(b"", 1, &mut offsets_out));
    assert!(!offsets(b"", 65, &mut offsets_out));
}
