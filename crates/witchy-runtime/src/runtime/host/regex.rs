//! Stateless canonical regex: checked slices, with no staged pending payload.
use wasmtime::{Caller, Error, Linker, Result};
use super::super::{memory_of, read_slice_str, VmState};

pub(in crate::runtime) fn link(linker: &mut Linker<VmState>) -> Result<()> {
    linker.func_wrap("witchy", "regex_match_spans_into", host_spans)?;
    Ok(())
}

fn host_spans(mut caller: Caller<'_, VmState>, pattern: i32, pattern_len: i32,
    text: i32, text_len: i32, output: i32, capacity: i32) -> Result<i32> {
    let memory = memory_of(&mut caller)?;
    spans_into(memory.data_mut(&mut caller), pattern, pattern_len, text, text_len, output, capacity)
}

fn spans_into(memory: &mut [u8], pattern: i32, pattern_len: i32,
    text: i32, text_len: i32, output: i32, capacity: i32) -> Result<i32> {
    let pattern = read_slice_str(memory, pattern, pattern_len)?;
    let text = read_slice_str(memory, text, text_len)?;
    let start = usize::try_from(output).map_err(|_| Error::msg("negative regex output pointer"))?;
    let capacity = usize::try_from(capacity).map_err(|_| Error::msg("negative regex output capacity"))?;
    let end = start.checked_add(capacity).ok_or_else(|| Error::msg("regex output slice overflow"))?;
    if end > memory.len() { return Err(Error::msg("regex output slice exceeds memory")); }
    let regex = ::regex::Regex::new(&pattern)
        .map_err(|error| Error::msg(format!("invalid regex pattern `{pattern}`: {error}")))?;
    let mut encoded = String::new();
    let mut byte_cursor = 0;
    let mut char_cursor = 0usize;
    for found in regex.find_iter(&text) {
        use std::fmt::Write;
        char_cursor += text[byte_cursor..found.start()].chars().count();
        let begin = char_cursor;
        char_cursor += text[found.start()..found.end()].chars().count();
        byte_cursor = found.end();
        if !encoded.is_empty() { encoded.push(';'); }
        write!(encoded, "{begin},{char_cursor}").expect("writing to String cannot fail");
    }
    let required = i32::try_from(encoded.len()).map_err(|_| Error::msg("regex span output exceeds ABI size"))?;
    if encoded.len() <= capacity {
        memory[start..start + encoded.len()].copy_from_slice(encoded.as_bytes());
    }
    Ok(required)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_regex_bounded_writes_and_invalid_slices() {
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
        assert_eq!(&memory[32..39], b"0,1;1,2");
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
}
