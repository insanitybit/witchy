//! Shared policy for environment variables supplied to an Exec program.

pub(crate) fn plain_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name[0].is_ascii_digit()
        && name
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'_')
}

pub(crate) fn loads_code(name: &[u8]) -> bool {
    include_str!("exec_env_refused.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .any(|entry| match entry.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix.as_bytes()),
            None => name == entry.as_bytes(),
        })
}
