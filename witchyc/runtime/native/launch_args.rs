//! Bounded UTF-8 launch arguments and guest selection.

use core::ffi::{CStr, c_char};

const MAX_WORDS: usize = 256;
const MAX_BYTES: usize = 65536;
const ARGS: u64 = 64;

struct Arguments {
    bytes: [u8; MAX_BYTES],
    ends: [usize; MAX_WORDS],
    count: usize,
    used: usize,
    selected: Option<usize>,
}

impl Arguments {
    const EMPTY: Self = Self {
        bytes: [0; MAX_BYTES],
        ends: [0; MAX_WORDS],
        count: 0,
        used: 0,
        selected: None,
    };

    fn bind(&mut self, word: &[u8]) -> Option<()> {
        if !valid_utf8(word) {
            return None;
        }
        let end = self.used.checked_add(word.len())?;
        let slot = self.ends.get_mut(self.count)?;
        let storage = self.bytes.get_mut(self.used..end)?;
        storage.copy_from_slice(word);
        *slot = end;
        self.used = end;
        self.count += 1;
        Some(())
    }

    fn word(&self, index: usize) -> Option<&[u8]> {
        if index >= self.count {
            return None;
        }
        let start = index
            .checked_sub(1)
            .and_then(|i| self.ends.get(i))
            .copied()
            .unwrap_or(0);
        self.bytes.get(start..*self.ends.get(index)?)
    }
}

// Launcher bindings and guest selections occur on one thread and never suspend.
static mut ARGUMENTS: Arguments = Arguments::EMPTY;

fn limit(bytes: &[u8], max: u32) -> Option<u32> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0u32, |value, &byte| {
        if !byte.is_ascii_digit() {
            return None;
        }
        let next = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
        (next <= max).then_some(next)
    })
}

fn parse_limits(bytes: &[u8]) -> Option<(u32, u32)> {
    let split = bytes.iter().position(|&byte| byte == b':')?;
    Some((
        limit(bytes.get(..split)?, MAX_WORDS as u32)?,
        limit(bytes.get(split + 1..)?, MAX_BYTES as u32)?,
    ))
}

// This standalone runtime object links directly with the platform linker.
// Decode byte classes here without a dependency on Rust's libcore archive.
pub(super) fn valid_utf8(mut bytes: &[u8]) -> bool {
    while let Some((&first, rest)) = bytes.split_first() {
        let (count, low, high) = match first {
            0x00..=0x7f => {
                bytes = rest;
                continue;
            }
            0xc2..=0xdf => (1, 0x80, 0xbf),
            0xe0 => (2, 0xa0, 0xbf),
            0xe1..=0xec | 0xee..=0xef => (2, 0x80, 0xbf),
            0xed => (2, 0x80, 0x9f),
            0xf0 => (3, 0x90, 0xbf),
            0xf1..=0xf3 => (3, 0x80, 0xbf),
            0xf4 => (3, 0x80, 0x8f),
            _ => return false,
        };
        let Some((tail, after)) = rest.split_at_checked(count) else {
            return false;
        };
        let Some((&second, remaining)) = tail.split_first() else {
            return false;
        };
        if !(low..=high).contains(&second) || !remaining.iter().all(|byte| byte & 0xc0 == 0x80) {
            return false;
        }
        bytes = after;
    }
    true
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_args_limits(
    text: *const c_char,
    words: *mut u32,
    bytes: *mut u32,
) -> bool {
    let Some((count, len)) = parse_limits(unsafe { CStr::from_ptr(text) }.to_bytes()) else {
        return false;
    };
    unsafe {
        words.write(count);
        bytes.write(len);
    }
    true
}

#[unsafe(no_mangle)]
unsafe extern "C" fn witchy_args_bind(text: *const c_char) -> bool {
    let bytes = unsafe { CStr::from_ptr(text) }.to_bytes();
    unsafe { (&mut *(&raw mut ARGUMENTS)).bind(bytes).is_some() }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_args_fit(words: u32, bytes: u32) -> bool {
    let args = unsafe { &*(&raw const ARGUMENTS) };
    args.count <= words as usize && args.used <= bytes as usize
}

fn check_grant(grant: u64) {
    if grant != ARGS {
        unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_args_count(grant: u64) -> u64 {
    check_grant(grant);
    unsafe { ARGUMENTS.count as u64 }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_args_select(grant: u64, index: usize) -> u64 {
    check_grant(grant);
    let args = unsafe { &mut *(&raw mut ARGUMENTS) };
    if let Some(word) = args.word(index) {
        let len = word.len();
        args.selected = Some(index);
        len as u64
    } else {
        args.selected = None;
        u64::MAX
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_args_byte(grant: u64, index: usize) -> u64 {
    check_grant(grant);
    let args = unsafe { &*(&raw const ARGUMENTS) };
    args.selected
        .and_then(|i| args.word(i))
        .and_then(|word| word.get(index))
        .map_or(256, |&byte| u64::from(byte))
}
