//! Named scalar grants bound by the launcher and queried by guest code.

const NAME_LIMIT: usize = 128;

#[derive(Clone, Copy)]
struct Grant {
    name: [u8; NAME_LIMIT],
    len: usize,
    value: u64,
}

pub struct Grants {
    entries: [Option<Grant>; 64],
    pending: [u8; NAME_LIMIT + 1],
    len: usize,
}

impl Grants {
    pub const fn new() -> Self {
        Self {
            entries: [None; 64],
            pending: [0; NAME_LIMIT + 1],
            len: 0,
        }
    }

    pub fn bind(&mut self, text: &[u8]) -> Option<()> {
        let equal = text.iter().position(|&byte| byte == b'=')?;
        let (name, value) = text.split_at_checked(equal)?;
        let value = value.get(1..)?;
        if !(3..=NAME_LIMIT).contains(&name.len()) {
            return None;
        }
        let dot = name.iter().position(|&byte| byte == b'.')?;
        if dot == 0
            || dot + 1 == name.len()
            || name
                .iter()
                .enumerate()
                .any(|(i, byte)| i != dot && !byte.is_ascii_alphanumeric() && *byte != b'_')
        {
            return None;
        }
        if self.find(name).is_some() {
            return None;
        }
        let value = parse_value(value)?;
        let entry = self.entries.iter_mut().find(|entry| entry.is_none())?;
        let mut stored = [0; NAME_LIMIT];
        for (target, source) in stored.iter_mut().zip(name) {
            *target = *source;
        }
        *entry = Some(Grant {
            name: stored,
            len: name.len(),
            value,
        });
        Some(())
    }

    fn find(&self, name: &[u8]) -> Option<u64> {
        self.entries
            .iter()
            .flatten()
            .find_map(|entry| (entry.name.get(..entry.len) == Some(name)).then_some(entry.value))
    }

    pub fn push(&mut self, byte: u8) {
        if let Some(next) = self.pending.get_mut(self.len) {
            *next = byte;
            self.len += 1;
        }
    }

    pub fn lookup(&mut self) -> Option<u64> {
        let len = core::mem::take(&mut self.len);
        self.find(self.pending.get(..len)?)
    }
}

fn parse_value(bytes: &[u8]) -> Option<u64> {
    match bytes {
        b"true" => return Some(1),
        b"false" => return Some(0),
        _ => {}
    }
    let digits = bytes.strip_prefix(b"+").unwrap_or(bytes);
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u64, |value, &digit| {
        if !digit.is_ascii_digit() {
            return None;
        }
        value.checked_mul(10)?.checked_add(u64::from(digit - b'0'))
    })
}

#[cfg(test)]
#[test]
fn rejects_bad_bindings_and_resets_oversized_queries() {
    let mut grants = Grants::new();
    assert_eq!(grants.bind(b"app.limit=+18446744073709551615"), Some(()));
    assert_eq!(grants.bind(b"app.limit=0"), None);
    assert_eq!(grants.bind(b"app.other=18446744073709551616"), None);
    assert_eq!(grants.bind(b"app.part.other=true"), None);
    assert_eq!(grants.bind(b"app.enabled=true"), Some(()));
    for byte in [b'a'; NAME_LIMIT + 3] {
        grants.push(byte);
    }
    assert_eq!(grants.lookup(), None);
    for &byte in b"app.limit" {
        grants.push(byte);
    }
    assert_eq!(grants.lookup(), Some(u64::MAX));
    assert_eq!(grants.lookup(), None);
}
