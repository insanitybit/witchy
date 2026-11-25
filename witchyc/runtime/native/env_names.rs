//! Syntax accepted for native environment grant names.

pub fn valid(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > 1024 {
        return false;
    }
    name.iter().enumerate().all(|(index, &byte)| {
        byte == b'_' || byte.is_ascii_alphabetic() || (index != 0 && byte.is_ascii_digit())
    })
}

#[cfg(test)]
#[test]
fn recognizes_environment_names() {
    for (name, expected) in [
        (&b"_VAR0"[..], true),
        (b"_", true),
        (b"", false),
        (b"0VAR", false),
        (b"VAR.NAME", false),
        (b"VAR\xff", false),
    ] {
        assert_eq!(valid(name), expected);
    }
    assert!(valid(&[b'x'; 1024]));
    assert!(!valid(&[b'x'; 1025]));
}
