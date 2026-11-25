//! Bounded UTF-8 launch arguments and guest selection.

use super::global::{Global, Table};
use core::ffi::CStr;

const MAX_WORDS: usize = 256;
const MAX_BYTES: usize = 65536;
const ARGS: u64 = 64;

// Launcher bindings and guest selections occur on one thread and never suspend.
/// The bytes of every bound word, one word after another. The guest reads
/// them by position, which a `Table` does not offer.
static mut BYTES: [u8; MAX_BYTES] = [0; MAX_BYTES];
/// How many of `BYTES` are in use.
static USED: Global<usize> = Global::new(0);
/// Where each bound word ends in `BYTES`.
static ENDS: Table<usize, MAX_WORDS> = Table::new(0);
/// Where the word the guest selected last starts and ends, if that word
/// exists. Words are only ever added, so the place stays the word's.
static SELECTED: Global<Option<(usize, usize)>> = Global::new(None);

fn bind(word: &[u8]) -> Option<()> {
    if !valid_utf8(word) {
        return None;
    }
    let used = USED.get();
    let end = used.checked_add(word.len())?;
    if ENDS.len() >= MAX_WORDS {
        return None;
    }
    // SAFETY: the runtime is one thread, and no other reference to the bytes
    // is live: this one ends with the copy below, and `byte` copies out.
    let storage = unsafe { (&mut *(&raw mut BYTES)).get_mut(used..end)? };
    storage.copy_from_slice(word);
    ENDS.push(end);
    USED.set(end);
    Some(())
}

/// Where the word starts and ends in `BYTES`.
fn word(index: usize) -> Option<(usize, usize)> {
    if index >= ENDS.len() {
        return None;
    }
    let start = index
        .checked_sub(1)
        .and_then(|i| ENDS.iter().nth(i))
        .unwrap_or(0);
    let end = ENDS.iter().nth(index)?;
    (start <= end && end <= MAX_BYTES).then_some((start, end))
}

/// The byte at `at`, which is within a bound word.
fn byte(at: usize) -> Option<u8> {
    // SAFETY: as `bind`: no reference to the bytes outlives a statement.
    unsafe { (&*(&raw const BYTES)).get(at).copied() }
}

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

pub(super) fn witchy_args_limits(text: &CStr, words: &mut u32, bytes: &mut u32) -> bool {
    let Some((count, len)) = parse_limits(text.to_bytes()) else {
        return false;
    };
    *words = count;
    *bytes = len;
    true
}

pub(super) fn witchy_args_bind(text: &CStr) -> bool {
    bind(text.to_bytes()).is_some()
}

#[unsafe(no_mangle)]
extern "C" fn witchy_args_fit(words: u32, bytes: u32) -> bool {
    ENDS.len() <= words as usize && USED.get() <= bytes as usize
}

fn check_grant(grant: u64) {
    if grant != ARGS {
        unsafe { core::arch::asm!("brk #0", options(noreturn, nostack)) }
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_args_count(grant: u64) -> u64 {
    check_grant(grant);
    ENDS.len() as u64
}

#[unsafe(no_mangle)]
extern "C" fn witchy_args_select(grant: u64, index: usize) -> u64 {
    check_grant(grant);
    if let Some((start, end)) = word(index) {
        SELECTED.set(Some((start, end)));
        (end - start) as u64
    } else {
        SELECTED.set(None);
        u64::MAX
    }
}

#[unsafe(no_mangle)]
extern "C" fn witchy_args_byte(grant: u64, index: usize) -> u64 {
    check_grant(grant);
    SELECTED
        .get()
        .and_then(|(start, end)| {
            byte(start.checked_add(index).filter(|&at| at < end)?)
        })
        .map_or(256, u64::from)
}
