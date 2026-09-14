//! Reading the launch entry an admitted canonical artifact's bytes carry.
//!
//! Admission derives the entry from the validated SafeWIR signature of the
//! checked `main`, emits it as the `witchy.entry` custom section, and matches
//! that section exactly, so its bytes are part of the artifact's identity.
//! This module decodes that section for tooling and admission checks. Decoded
//! metadata is not authority: a launcher must retain an
//! `AdmittedCanonicalArtifact` or separately authenticate an admission
//! envelope. An ill-formed or duplicated section is an error, never a guessed
//! entry.

use crate::canonical_wasmir::{CanonicalEntry, ENTRY_SECTION};

/// The entry section name, for tooling that lists an artifact's sections.
pub const CANONICAL_ENTRY_SECTION: &str = ENTRY_SECTION;

/// Decode the launch-entry claim carried by the bytes. The result is
/// non-authoritative metadata. `None` is a module without an entry section.
pub fn canonical_entry_from_wasm(wasm: &[u8]) -> Result<Option<CanonicalEntry>, String> {
    let mut found = None;
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|error| format!("invalid `{ENTRY_SECTION}` metadata: {error}"))?;
        let wasmparser::Payload::CustomSection(section) = payload else { continue };
        if section.name() != ENTRY_SECTION {
            continue;
        }
        if found.is_some() {
            return Err(format!("invalid `{ENTRY_SECTION}` metadata: duplicate section"));
        }
        found = Some(CanonicalEntry::decode(section.data())?);
    }
    Ok(found)
}
