//! Independent transport decoding and native-language hash semantics. The
//! length-prefixed request is not the RuneHash digest preimage.
use super::*;
use sha2::{Digest, Sha256};

fn hex_digest(bytes: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(bytes);
    let digits = b"0123456789abcdef";
    digest.iter().flat_map(|byte| [digits[(byte >> 4) as usize], digits[(byte & 15) as usize]]).collect()
}
fn word(input: &[u8], cursor: &mut usize) -> Result<usize, InterpretError> {
    let end = cursor.checked_add(4).ok_or(InterpretError::ArithmeticTrap)?;
    let bytes = input.get(*cursor..end).ok_or(InterpretError::TypeMismatch("RuneHash truncated request"))?;
    *cursor = end;
    Ok(u32::from_le_bytes(bytes.try_into().expect("four-byte word")) as usize)
}
fn array<'a>(input: &'a [u8], cursor: &mut usize) -> Result<Vec<&'a str>, InterpretError> {
    let count = word(input, cursor)?;
    // Every element needs its length word, even an empty string. Refuse a
    // forged count before allocating storage proportional to that count.
    if count > input.len().saturating_sub(*cursor) / 4 {
        return Err(InterpretError::TypeMismatch("RuneHash truncated array"));
    }
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        let length = word(input, cursor)?;
        let end = cursor.checked_add(length).ok_or(InterpretError::ArithmeticTrap)?;
        let bytes = input.get(*cursor..end).ok_or(InterpretError::TypeMismatch("RuneHash truncated string"))?;
        let value = std::str::from_utf8(bytes).map_err(|_| InterpretError::TypeMismatch("RuneHash string is not UTF-8"))?;
        strings.push(value);
        *cursor = end;
    }
    Ok(strings)
}
fn rune_digest(input: &[u8]) -> Result<Vec<u8>, InterpretError> {
    let mut cursor = 0;
    let paths = array(input, &mut cursor)?;
    let contents = array(input, &mut cursor)?;
    if cursor != input.len() { return Err(InterpretError::TypeMismatch("RuneHash trailing request bytes")); }
    if paths.len() != contents.len() {
        return Err(InterpretError::TypeMismatch("crypto.rune_hash: paths and contents differ in length"));
    }
    // Sort indices stably, rather than sharing the runtime's native argument
    // reconstruction or preimage builder. Equal paths retain input order.
    let mut order: Vec<_> = (0..paths.len()).collect();
    order.sort_by_key(|index| paths[*index]);
    let mut digest = Sha256::new();
    for index in order {
        let path = paths[index].as_bytes();
        let normalized = contents[index].replace("\r\n", "\n");
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path);
        digest.update((normalized.len() as u64).to_le_bytes());
        digest.update(normalized.as_bytes());
    }
    let digits = b"0123456789abcdef";
    let mut result = b"sha256:".to_vec();
    for byte in digest.finalize() { result.push(digits[(byte >> 4) as usize]); result.push(digits[(byte & 15) as usize]); }
    Ok(result)
}

impl Engine<'_> {
    pub(super) fn crypto_hash_host_call(&mut self, name: &str, destinations: &[ValueId],
        arguments: &[HostCallArgument], values: &mut BTreeMap<ValueId, InterpreterValue>) -> Result<(), InterpretError>
    {
        let ([destination], [HostCallArgument::Buffer { place: input, .. }, HostCallArgument::Buffer { place: output, .. }])
            = (destinations, arguments) else { return Err(InterpretError::TypeMismatch("crypto hash slice ABI")); };
        let input = self.host_bytes(input, values)?;
        let (object, start, capacity) = self.slice_place(output, values)?;
        // Validate the ENTIRE caller-declared capacity, not just 64/71 bytes,
        // before decoding or writing. A short valid buffer remains untouched.
        self.check_range(object, start, capacity, true)?;
        let encoded = match name {
            crate::canonical_safewir::CRYPTO_SHA256_IMPORT => hex_digest(&input),
            crate::canonical_safewir::CRYPTO_RUNE_HASH_IMPORT => rune_digest(&input)?,
            _ => return Err(InterpretError::UnsupportedHostImport),
        };
        let required = encoded.len() as u64;
        if required <= capacity {
            for (offset, byte) in encoded.into_iter().enumerate() {
                let index = start.checked_add(offset as u64).ok_or(InterpretError::ArithmeticTrap)?;
                self.object_mut(object)?.cells.insert(CellKey::Element(index), InterpreterValue::Size(u64::from(byte)));
            }
        }
        values.insert(*destination, InterpreterValue::Size(required));
        Ok(())
    }
}
