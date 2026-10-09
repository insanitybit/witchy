//! Stateless canonical regex: checked slices, with no staged pending payload.
use crate::runtime::GuestAddress;
use wasmtime::{Caller, Error, Linker, Result};
use super::super::{memory_of, read_slice_str, VmState};

pub(in crate::runtime) fn link<P: GuestAddress>(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "regex_match_spans_into", host_spans::<P>)?;
    linker.func_wrap("witchy", "regex_count_slices", host_count::<P>)?;
    linker.func_wrap("witchy", "regex_replace_all_into", host_replace_all::<P>)?;
    Ok(())
}

fn compile(pattern: &str) -> Result<::regex::Regex> {
    ::regex::Regex::new(pattern)
        .map_err(|error| Error::msg(format!("invalid regex pattern `{pattern}`: {error}")))
}

/// The bounded output slice `[output, output + capacity)`, checked against
/// memory before anything is computed or written.
fn output_range<P: GuestAddress>(memory: &[u8], output: P, capacity: i32) -> Result<std::ops::Range<usize>> {
    let start = output.offset();
    let capacity = usize::try_from(capacity).map_err(|_| Error::msg("negative regex output capacity"))?;
    let end = start.checked_add(capacity).ok_or_else(|| Error::msg("regex output slice overflow"))?;
    if end > memory.len() { return Err(Error::msg("regex output slice exceeds memory")); }
    Ok(start..end)
}

fn host_count<P: GuestAddress>(mut caller: Caller<'_, VmState>, pattern: P, pattern_len: i32,
    text: P, text_len: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    count(memory.data(&caller), pattern, pattern_len, text, text_len)
}

fn count<P: GuestAddress>(memory: &[u8], pattern: P, pattern_len: i32, text: P, text_len: i32) -> Result<i32> {
    let pattern = read_slice_str(memory, pattern, pattern_len)?;
    let text = read_slice_str(memory, text, text_len)?;
    i32::try_from(compile(&pattern)?.find_iter(&text).count())
        .map_err(|_| Error::msg("regex match count exceeds ABI size"))
}

fn host_replace_all<P: GuestAddress>(mut caller: Caller<'_, VmState>, pattern: P, pattern_len: i32,
    text: P, text_len: i32, replacement: P, replacement_len: i32,
    output: P, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    replace_all_into(memory.data_mut(&mut caller), pattern, pattern_len, text, text_len,
        replacement, replacement_len, output, capacity)
}

/// Every match replaced by the literal replacement (no `$1` expansion). The
/// result is the full byte count; insufficient capacity writes nothing.
///
/// The output can be far larger than its inputs (every empty match takes a
/// copy of the replacement), so the host never materializes it: the size is
/// counted from the match spans, and the pieces are written straight into
/// the guest's own buffer, which the guest allocated under its own limits.
fn replace_all_into<P: GuestAddress>(memory: &mut [u8], pattern: P, pattern_len: i32,
    text: P, text_len: i32, replacement: P, replacement_len: i32,
    output: P, capacity: i32) -> Result<i32> {
    let pattern = read_slice_str(memory, pattern, pattern_len)?;
    let text = read_slice_str(memory, text, text_len)?;
    let replacement = read_slice_str(memory, replacement, replacement_len)?;
    let range = output_range(memory, output, capacity)?;
    // At most one span per text byte plus one: linear in the input.
    let spans = compile(&pattern)?.find_iter(&text).map(|found| (found.start(), found.end())).collect::<Vec<_>>();
    let oversized = || Error::msg("regex replacement exceeds ABI size");
    let matched = spans.iter().map(|(start, end)| end - start).sum::<usize>();
    let required = spans.len().checked_mul(replacement.len())
        .and_then(|inserted| (text.len() - matched).checked_add(inserted))
        .ok_or_else(oversized)?;
    let required_abi = i32::try_from(required).map_err(|_| oversized())?;
    if required <= range.len() {
        let mut cursor = range.start;
        let mut write = |bytes: &[u8]| {
            memory[cursor..cursor + bytes.len()].copy_from_slice(bytes);
            cursor += bytes.len();
        };
        let mut kept = 0;
        for (start, end) in spans {
            write(&text.as_bytes()[kept..start]);
            write(replacement.as_bytes());
            kept = end;
        }
        write(&text.as_bytes()[kept..]);
    }
    Ok(required_abi)
}

fn host_spans<P: GuestAddress>(mut caller: Caller<'_, VmState>, pattern: P, pattern_len: i32,
    text: P, text_len: i32, output: P, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    spans_into(memory.data_mut(&mut caller), pattern, pattern_len, text, text_len, output, capacity)
}

fn spans_into<P: GuestAddress>(memory: &mut [u8], pattern: P, pattern_len: i32,
    text: P, text_len: i32, output: P, capacity: i32) -> Result<i32> {
    let pattern = read_slice_str(memory, pattern, pattern_len)?;
    let text = read_slice_str(memory, text, text_len)?;
    let range = output_range(memory, output, capacity)?;
    let (start, capacity) = (range.start, range.len());
    let regex = compile(&pattern)?;
    let mut encoded = String::new();
    for found in regex.find_iter(&text) {
        use std::fmt::Write;
        if !encoded.is_empty() { encoded.push(';'); }
        write!(encoded, "{},{}", found.start(), found.end()).expect("writing to String cannot fail");
    }
    let required = i32::try_from(encoded.len()).map_err(|_| Error::msg("regex span output exceeds ABI size"))?;
    if encoded.len() <= capacity {
        memory[start..start + encoded.len()].copy_from_slice(encoded.as_bytes());
    }
    Ok(required)
}

#[cfg(feature = "self-checks")]
#[doc(hidden)]
pub mod tests {
    use super::*;
    pub fn canonical_regex_bounded_writes_and_invalid_slices() {
        let mut memory = [b'!'; 80];
        memory[0] = b'.';
        let text = "\u{e9}\u{1f642}".as_bytes();
        memory[8..8 + text.len()].copy_from_slice(text);
        let original = memory;
        assert_eq!(spans_into(&mut memory, 0, 1, 8, 6, 32, 0).unwrap(), 7);
        assert_eq!(memory, original);
        assert_eq!(spans_into(&mut memory, 0, 1, 8, 6, 32, 6).unwrap(), 7);
        assert_eq!(memory, original, "short capacity writes nothing");
        assert_eq!(spans_into(&mut memory, 0, 1, 8, 6, 32, 7).unwrap(), 7);
        assert_eq!(&memory[32..39], b"0,2;2,6", "spans are byte offsets");
        assert_eq!(memory[31], b'!');
        assert_eq!(memory[39], b'!');
        for (pattern, plen, text, tlen, output, cap) in [
            (-1, 1, 8, 6, 32, 7), (0, -1, 8, 6, 32, 7),
            (0, 1, 79, 2, 32, 7), (0, 1, 8, 6, 32, -1),
            (0, 1, 8, 6, -1, 0), (0, 1, 8, 6, 79, 2),
            (0, 1, 8, 6, 81, 0), (0, 1, 8, 1, 32, 7),
        ] {
            let before = memory;
            assert!(spans_into(&mut memory, pattern, plen, text, tlen, output, cap).is_err());
            assert_eq!(memory, before, "invalid slices never write");
        }
        memory[0] = b'[';
        let before = memory;
        assert!(spans_into(&mut memory, 0, 1, 8, 6, 32, 7).unwrap_err().to_string().contains("invalid regex pattern"));
        assert_eq!(memory, before);
        memory[0] = 255;
        assert!(spans_into(&mut memory, 0, 1, 8, 6, 32, 7).is_err());
    }

    pub fn canonical_regex_count_and_replace_bounded_writes() {
        let mut memory = [b'!'; 96];
        memory[0..3].copy_from_slice(b"a+b");
        memory[8..18].copy_from_slice(b"xaab-ab-ac");
        memory[24..26].copy_from_slice(b"<>");
        assert_eq!(count(&memory, 0, 3, 8, 10).unwrap(), 2);
        let original = memory;
        // "x<>-<>-ac" is 9 bytes: a short buffer writes nothing.
        assert_eq!(replace_all_into(&mut memory, 0, 3, 8, 10, 24, 2, 48, 8).unwrap(), 9);
        assert_eq!(memory, original, "short capacity writes nothing");
        assert_eq!(replace_all_into(&mut memory, 0, 3, 8, 10, 24, 2, 48, 9).unwrap(), 9);
        assert_eq!(&memory[48..57], b"x<>-<>-ac");
        assert_eq!(memory[57], b'!');
        // The replacement is literal: `$0` is not expanded.
        memory[24..26].copy_from_slice(b"$0");
        assert_eq!(replace_all_into(&mut memory, 0, 3, 8, 10, 24, 2, 64, 16).unwrap(), 9);
        assert_eq!(&memory[64..73], b"x$0-$0-ac");
        let before = memory;
        assert!(replace_all_into(&mut memory, 0, 3, 8, 10, 24, 2, 90, 16).is_err(), "output past memory");
        assert!(replace_all_into(&mut memory, 0, 3, 8, 10, 24, -1, 48, 9).is_err(), "negative slice");
        assert_eq!(memory, before, "invalid slices never write");
        memory[0] = b'[';
        assert!(count(&memory, 0, 1, 8, 10).unwrap_err().to_string().contains("invalid regex pattern"));
    }

    pub fn canonical_regex_replace_sizes_empty_matches_without_writing() {
        // An empty pattern matches before, between, and after every byte:
        // "ab" with "xy" is "xyaxybxy".
        let mut memory = [b'!'; 64];
        memory[8..10].copy_from_slice(b"ab");
        memory[16..18].copy_from_slice(b"xy");
        assert_eq!(replace_all_into(&mut memory, 0, 0, 8, 2, 16, 2, 32, 8).unwrap(), 8);
        assert_eq!(&memory[32..40], b"xyaxybxy");
        assert_eq!(memory[40], b'!');
        // Ten bytes, each surrounded by a ten-byte replacement, report their
        // full size against a short buffer and write nothing.
        memory[8..18].copy_from_slice(b"0123456789");
        memory[24..34].copy_from_slice(b"ABCDEFGHIJ");
        let before = memory;
        assert_eq!(replace_all_into(&mut memory, 0, 0, 8, 10, 24, 10, 40, 16).unwrap(), 10 + 11 * 10);
        assert_eq!(memory, before);
    }
}
