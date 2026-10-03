//! Portable precompiled Wasm.
//!
//! The file is the exact admitted module followed by ONE trailing custom
//! section, `witchy.source.v1`, holding the SHA-256 of the module prefix and
//! the closed source inputs that produced it. Other Wasm hosts ignore custom
//! sections. A Witchy host trusts nothing in the section: it re-admits the
//! source and requires the recompiled module to equal the prefix byte for
//! byte before deriving any grant from it.

use sha2::{Digest, Sha256};

use crate::trusted_publisher::source_inputs::{SourceInputs, PORTABLE_WASM_DOMAIN};

pub const SECTION_NAME: &str = "witchy.source.v1";
/// Ceiling on the section payload, checked before it is hashed or decoded.
pub const MAX_SECTION_BYTES: usize = 64 << 20;
const DIGEST_LEN: usize = 32;

/// An untrusted portable artifact split at its source section. `module` is a
/// claim to be checked against fresh admission, never an admission receipt.
#[derive(Debug)]
pub struct PortableArtifact<'a> {
    pub module: &'a [u8],
    pub source_inputs: SourceInputs,
}

/// Append the closed source that produced `wasm`. Inputs are bound to the
/// portable format, so a trusted-publisher payload cannot be transplanted.
pub fn seal(wasm: &[u8], inputs: &SourceInputs) -> Result<Vec<u8>, String> {
    if probe(wasm)?.is_some() {
        return Err(format!("module already carries a `{SECTION_NAME}` section"));
    }
    let mut inputs = inputs.clone();
    inputs.launcher_digest = Sha256::digest(PORTABLE_WASM_DOMAIN).into();
    inputs.validate_portable()?;
    let inputs = serde_json::to_vec(&inputs).map_err(|error| error.to_string())?;
    let mut payload = Vec::with_capacity(SECTION_NAME.len() + 1 + DIGEST_LEN + inputs.len());
    leb128(&mut payload, SECTION_NAME.len());
    payload.extend_from_slice(SECTION_NAME.as_bytes());
    payload.extend_from_slice(&Sha256::digest(wasm));
    payload.extend_from_slice(&inputs);
    if payload.len() > MAX_SECTION_BYTES {
        return Err(format!("embedded source exceeds the {MAX_SECTION_BYTES}-byte `{SECTION_NAME}` limit"));
    }
    let mut sealed = Vec::with_capacity(wasm.len() + payload.len() + 6);
    sealed.extend_from_slice(wasm);
    sealed.push(0);
    leb128(&mut sealed, payload.len());
    sealed.extend_from_slice(&payload);
    Ok(sealed)
}

/// What a sealed artifact discloses: every embedded file with contents and
/// their total size. Producers print this so publishing is never silent.
pub fn embedded_source_summary(inputs: &SourceInputs) -> String {
    let files: Vec<_> = inputs.files.iter()
        .filter_map(|file| file.contents.as_ref().map(|contents| (file.path.as_str(), contents.len())))
        .collect();
    let bytes: usize = files.iter().map(|(_, len)| len).sum();
    let paths: Vec<_> = files.iter().map(|(path, _)| *path).collect();
    format!("{} files ({}), {bytes} bytes", files.len(), paths.join(", "))
}

/// Split a binary module at its `witchy.source.v1` section. `Ok(None)` means
/// the module carries no section. Once one is present, every malformation
/// fails closed: it must be unique, canonically encoded, last, sized within
/// [`MAX_SECTION_BYTES`], digest its prefix, and hold portable inputs.
pub fn probe(bytes: &[u8]) -> Result<Option<PortableArtifact<'_>>, String> {
    use wasmparser::{Encoding, Parser, Payload};
    let mut found = None;
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.map_err(|error| format!("not a valid wasm module: {error}"))? {
            Payload::Version { encoding: Encoding::Component, .. } => {
                return Err("a Wasm component is not a portable Witchy module".into());
            }
            Payload::CustomSection(reader) if reader.name() == SECTION_NAME => {
                if found.is_some() {
                    return Err(format!("portable Wasm carries more than one `{SECTION_NAME}` section"));
                }
                found = Some((reader.range(), reader.data()));
            }
            _ => {}
        }
    }
    let Some((range, data)) = found else { return Ok(None) };
    if range.end != bytes.len() {
        return Err(format!("`{SECTION_NAME}` must be the last section of portable Wasm"));
    }
    if range.len() > MAX_SECTION_BYTES {
        return Err(format!("`{SECTION_NAME}` exceeds its {MAX_SECTION_BYTES}-byte limit"));
    }
    let mut header = vec![0];
    leb128(&mut header, range.len());
    let start = range.start.checked_sub(header.len())
        .filter(|&start| bytes[start..range.start] == header[..])
        .ok_or_else(|| format!("`{SECTION_NAME}` section header is not canonically encoded"))?;
    let module = &bytes[..start];
    let (digest, inputs) = data.split_at_checked(DIGEST_LEN)
        .ok_or_else(|| format!("`{SECTION_NAME}` is truncated"))?;
    if digest != Sha256::digest(module).as_slice() {
        return Err(format!("`{SECTION_NAME}` digest does not match its module (artifact is corrupt)"));
    }
    let source_inputs: SourceInputs = serde_json::from_slice(inputs)
        .map_err(|error| format!("invalid portable source inputs: {error}"))?;
    source_inputs.validate_portable()?;
    Ok(Some(PortableArtifact { module, source_inputs }))
}

fn leb128(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trusted_publisher::source_inputs::SourceFile;

    const MODULE: &[u8] = b"\0asm\x01\0\0\0";

    fn inputs() -> SourceInputs {
        SourceInputs::captured("src/main.witchy".into(), Vec::new(), vec![
            SourceFile { path: "src/main.witchy".into(), contents: Some("fn main():\n    ()\n".into()) },
        ], None)
    }

    fn custom(name: &str, data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::new();
        leb128(&mut payload, name.len());
        payload.extend_from_slice(name.as_bytes());
        payload.extend_from_slice(data);
        let mut section = vec![0];
        leb128(&mut section, payload.len());
        section.extend_from_slice(&payload);
        section
    }

    #[test]
    fn source_section_round_trips_and_every_malformation_fails_closed() {
        let sealed = seal(MODULE, &inputs()).unwrap();
        let portable = probe(&sealed).unwrap().expect("sealed module carries its source");
        assert_eq!(portable.module, MODULE);
        assert_eq!(portable.source_inputs.entry, "src/main.witchy");
        assert!(probe(MODULE).unwrap().is_none(), "a bare module has no section");
        assert!(seal(&sealed, &inputs()).is_err(), "sealing twice must refuse");

        let section = &sealed[MODULE.len()..];
        let mut trusted = inputs();
        trusted.launcher_digest = Sha256::digest(b"some native launcher").into();
        let raw = |inputs: &SourceInputs| {
            let mut data = Sha256::digest(MODULE).to_vec();
            data.extend(serde_json::to_vec(inputs).unwrap());
            [MODULE, &custom(SECTION_NAME, &data)[..]].concat()
        };
        let transplanted = raw(&trusted);
        // A partial pass mask (here: release minus one pass) is no profile
        // `witchy compile` emits; the consumer refuses it before recompiling.
        let mut partial = inputs();
        partial.launcher_digest = Sha256::digest(PORTABLE_WASM_DOMAIN).into();
        partial.opt_mask = crate::trusted_publisher::source_inputs::release_opt_mask() & !1;
        let partial = raw(&partial);
        // Flip the payload byte of an ordinary custom section inside the prefix.
        let mut mutated = seal(&[MODULE, &custom("note", b"a")[..]].concat(), &inputs()).unwrap();
        mutated[MODULE.len() + 7] ^= 1;
        let mut oversized = MODULE.to_vec();
        oversized.extend(custom(SECTION_NAME, &vec![0; MAX_SECTION_BYTES]));
        for (label, bytes, expected) in [
            ("duplicate", [&sealed[..], section].concat(), "more than one"),
            ("not last", [&sealed[..], &custom("trailer", b"")[..]].concat(), "must be the last section"),
            ("prefix mutated", mutated, "digest does not match"),
            ("trusted inputs transplanted", transplanted, "compatibility mismatch"),
            ("arbitrary optimization mask", partial, "neither the debug nor the release profile"),
            ("oversized", oversized, "limit"),
        ] {
            let error = probe(&bytes).expect_err(label);
            assert!(error.contains(expected), "{label}: {error}");
        }
    }
}
