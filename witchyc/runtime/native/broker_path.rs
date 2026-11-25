//! Pure path policy for directory grants. The broker resolves accepted paths
//! relative to open directory descriptors.

pub(super) fn permitted_name(name: &[u8], mode: u32) -> bool {
    let (Some(&first), Some(&last)) = (name.first(), name.last()) else {
        return false;
    };
    if name.len() > 255
        || first == b'.'
        || last == b'.'
        || !name
            .iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return false;
    }
    if mode != 2 {
        return true;
    }
    let exact = [b"witchy.toml".as_slice(), b"witchy.lock"];
    let suffix = [b".wy".as_slice(), b".policy", b".packages", b".grants.toml"];
    !exact.iter().any(|word| name.eq_ignore_ascii_case(word))
        && !suffix.iter().any(|word| {
            name.len()
                .checked_sub(word.len())
                .and_then(|at| name.get(at..))
                .is_some_and(|tail| tail.eq_ignore_ascii_case(word))
        })
}

pub(super) fn relative_path(path: &[u8], mode: u32) -> bool {
    path.iter().all(|&byte| byte != 0)
        && path.split(|byte| *byte == b'/').all(|component| {
            !component.is_empty()
                && component != b"."
                && component != b".."
                && (mode == 0 || permitted_name(component, mode))
        })
}

#[cfg(test)]
#[test]
fn data_paths_apply_policy_to_every_component() {
    assert!(relative_path(b"dir/file.csv", 2));
    assert!(relative_path(b"dir/a.WY", 1));
    assert!(relative_path(b"dir/non ASCII \xff", 0));
    for path in [
        b"dir/a.WY".as_slice(),
        b"WITCHY.LOCK/x",
        b"x.grants.TOML",
        b"/root",
        b"dir//x",
        b"a/..",
        b"a/.",
        b"x\0y",
        b"",
    ] {
        assert!(!relative_path(path, 2));
    }
}
