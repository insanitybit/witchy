//! Validation of the environment passed to an explicitly granted executable.
use super::global::State;
use core::ffi::CStr;

struct Names {
    names: [[u8; 64]; 16],
    lengths: [u8; 16],
    count: usize,
}

impl Names {
    const EMPTY: Self = Self {
        names: [[0; 64]; 16],
        lengths: [0; 16],
        count: 0,
    };

    fn contains(&self, name: &[u8]) -> bool {
        (0..self.count).any(|i| &self.names[i][..self.lengths[i] as usize] == name)
    }

    fn bind(&mut self, list: &[u8]) -> bool {
        self.count = 0;
        if list.is_empty() {
            return true;
        }
        for name in list.split(|&b| b == b',') {
            if self.count == 16
                || !crate::exec_env_policy::plain_name(name)
                || crate::exec_env_policy::loads_code(name)
                || self.contains(name)
            {
                return false;
            }
            self.names[self.count][..name.len()].copy_from_slice(name);
            self.lengths[self.count] = name.len() as u8;
            self.count += 1;
        }
        true
    }

    fn validate_values(&self, pending: &[u8]) -> bool {
        let mut names = [&[][..]; 16];
        let mut count = 0;
        let mut at = 0;
        while at < pending.len() {
            let Some(end) = pending[at..].iter().position(|&b| b == 0) else {
                return false;
            };
            let entry = &pending[at..at + end];
            let Some(equal) = entry.iter().position(|&b| b == b'=') else {
                return false;
            };
            let name = &entry[..equal];
            if count == 16
                || !self.contains(name)
                || names[..count].contains(&name)
                || entry.len() - equal - 1 > 4096
            {
                return false;
            }
            names[count] = name;
            count += 1;
            at += end + 1;
        }
        true
    }
}

// Configured at broker startup, before requests run on its single thread.
// Each use is one of `Names`' own methods, which call only the name policy.
static NAMES: State<Names> = State::new(Names::EMPTY);

pub(super) fn witchy_exec_env_names(list: &CStr) -> bool {
    NAMES.with(|names| names.bind(list.to_bytes()))
}

pub(super) fn witchy_exec_env_declared(name: &[u8]) -> bool {
    NAMES.with(|names| names.contains(name))
}

pub(super) fn valid_values(bytes: &[u8]) -> bool {
    NAMES.with(|names| names.validate_values(bytes))
}

pub(super) fn witchy_exec_plain(value: &[u8]) -> bool {
    if value.is_empty() || value.len() > 255 {
        return false;
    }
    value != b"."
        && value != b".."
        && value
            .iter()
            .all(|&b| b >= 0x21 && b != 0x7f && ![b'/', b'\\', b',', b':'].contains(&b))
}

#[cfg(test)]
mod tests {
    use super::Names;

    #[test]
    fn validates_names_and_code_loading_policy() {
        let mut names = Names::EMPTY;
        assert!(names.bind(b"LANG,TERM"));
        assert!(names.contains(b"TERM"));
        for value in [
            b"LANG,".as_slice(),
            b"LANG,LANG",
            b"DYLD_INSERT_LIBRARIES",
            b"1FOO",
            b"a",
        ] {
            assert!(!names.bind(value), "{value:?}");
        }
        assert!(names.bind(b""));
    }

    #[test]
    fn validates_pending_environment() {
        let mut names = Names::EMPTY;
        assert!(names.bind(b"LANG,TERM"));
        assert!(names.validate_values(b"LANG=en\0TERM=xterm\0"));
        for value in [
            b"LANG=en".as_slice(),
            b"LANG=a\0LANG=b\0",
            b"HOME=x\0",
            b"TERM\0",
        ] {
            assert!(!names.validate_values(value));
        }
        assert!(names.validate_values(b""));
    }
}
