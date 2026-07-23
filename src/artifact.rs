//! Source-derived metadata carried by distributable Witchy wasm modules.

use std::borrow::Cow;

use wasm_encoder::{CustomSection, Section as _};

use witchy_syntax::ast;
use witchy_caps::capabilities;

const LAUNCH_SECTION: &str = "witchy.launch";
const LAUNCH_VERSION: u8 = 1;

const CAP_TAGS: [(&str, u8); 11] = [
    ("Console", 0),
    ("Clock", 1),
    ("Rand", 2),
    ("Env", 3),
    ("Secret", 4),
    ("SecretStore", 5),
    ("Dir", 6),
    ("File", 7),
    ("Net", 8),
    ("Exec", 9),
    ("Fetch", 10),
];

const DIR_RIGHTS: [(&str, u8); 2] = [("Read", 1 << 0), ("Write", 1 << 1)];
const NET_RIGHTS: [(&str, u8); 5] = [
    ("Connect", 1 << 0),
    ("Listen", 1 << 1),
    ("Tcp", 1 << 2),
    ("Udp", 1 << 3),
    ("Uds", 1 << 4),
];

/// Append the checked source module's root capability contract to its wasm.
/// Imports remain the executable authority floor; this section preserves
/// capability parameters that lowering can otherwise make operationally unused.
pub fn embed_launch_contract(mut wasm: Vec<u8>, module: &ast::Module) -> Vec<u8> {
    let caps = capabilities::run_grant(module);
    let mut data = Vec::with_capacity(2 + caps.len() * 2);
    data.push(LAUNCH_VERSION);
    data.push(u8::try_from(caps.len()).expect("host capability count fits in one byte"));
    for (name, rights) in caps {
        data.push(cap_tag(name).expect("the capability analyzer emits known host capabilities"));
        data.push(rights_mask(name, &rights));
    }
    CustomSection {
        name: Cow::Borrowed(LAUNCH_SECTION),
        data: Cow::Owned(data),
    }
    .append_to(&mut wasm);
    wasm
}

/// Read a Witchy launch contract. `None` is a legacy or external wasm module;
/// callers must retain import-derived classification for that compatibility path.
pub fn launch_contract(wasm: &[u8]) -> Result<Option<capabilities::CapSet>, String> {
    launch_contract_payload(wasm)?
        .map(decode_contract)
        .transpose()
}

/// Return the exact encoded `witchy.launch` payload after validating that the
/// module carries at most one section. Trusted executables digest these bytes,
/// rather than a reconstructed capability set, so packaging cannot silently
/// change or discard launch metadata that the runtime will consume.
pub fn launch_contract_payload(wasm: &[u8]) -> Result<Option<&[u8]>, String> {
    let mut found = None;
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|error| invalid_contract(&error.to_string()))?;
        let wasmparser::Payload::CustomSection(section) = payload else { continue };
        if section.name() != LAUNCH_SECTION {
            continue;
        }
        if found.is_some() {
            return Err(invalid_contract("duplicate section"));
        }
        decode_contract(section.data())?;
        found = Some(section.data());
    }
    Ok(found)
}

fn cap_tag(name: &str) -> Option<u8> {
    CAP_TAGS.iter().find_map(|(cap, tag)| (*cap == name).then_some(*tag))
}

fn tag_cap(tag: u8) -> Option<&'static str> {
    CAP_TAGS.iter().find_map(|(cap, candidate)| (*candidate == tag).then_some(*cap))
}

fn right_tags(cap: &str) -> &'static [(&'static str, u8)] {
    match cap {
        "Dir" | "File" => &DIR_RIGHTS,
        "Net" => &NET_RIGHTS,
        _ => &[],
    }
}

fn rights_mask(cap: &str, rights: &capabilities::Rights) -> u8 {
    right_tags(cap)
        .iter()
        .filter_map(|(right, bit)| rights.contains(right).then_some(*bit))
        .fold(0, |mask, bit| mask | bit)
}

fn decode_contract(data: &[u8]) -> Result<capabilities::CapSet, String> {
    if data.len() < 2 {
        return Err(invalid_contract("truncated header"));
    }
    if data[0] != LAUNCH_VERSION {
        return Err(invalid_contract(&format!("unsupported version {}", data[0])));
    }
    let count = usize::from(data[1]);
    if data.len() != 2 + count * 2 {
        return Err(invalid_contract("entry count does not match the payload length"));
    }

    let mut caps = capabilities::CapSet::new();
    for entry in data[2..].chunks_exact(2) {
        let cap = tag_cap(entry[0])
            .ok_or_else(|| invalid_contract(&format!("unknown capability tag {}", entry[0])))?;
        let tags = right_tags(cap);
        let valid_mask = tags.iter().fold(0, |mask, (_, bit)| mask | bit);
        if entry[1] & !valid_mask != 0 {
            return Err(invalid_contract(&format!("invalid rights for `{cap}`")));
        }
        let mut rights = capabilities::Rights::new();
        for &(right, bit) in tags {
            if entry[1] & bit != 0 {
                rights.insert(right);
            }
        }
        if caps.insert(cap, rights).is_some() {
            return Err(invalid_contract(&format!("duplicate `{cap}` entry")));
        }
    }
    Ok(caps)
}

fn invalid_contract(detail: &str) -> String {
    format!("invalid `{LAUNCH_SECTION}` metadata: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_encoder::Module;

    #[test]
    fn launch_contract_round_trips_capabilities_and_rights() {
        let module = witchy_syntax::parser::parse_module(
            "fn main(console: Console, root: Dir[Read], net: Net[Connect, Tcp], key: Secret, fetch: Fetch):\n    return\n",
        )
        .unwrap();
        let wasm = embed_launch_contract(Module::new().finish(), &module);
        wasmparser::validate(&wasm).expect("metadata keeps the module valid");

        let decoded = launch_contract(&wasm).unwrap().expect("launch section");
        assert_eq!(decoded, capabilities::run_grant(&module));
    }

    #[test]
    fn legacy_wasm_has_no_launch_contract() {
        let wasm = Module::new().finish();
        assert_eq!(launch_contract(&wasm).unwrap(), None);
    }

    #[test]
    fn unknown_contract_version_fails_closed() {
        let mut wasm = Module::new().finish();
        CustomSection {
            name: Cow::Borrowed(LAUNCH_SECTION),
            data: Cow::Borrowed(&[LAUNCH_VERSION + 1, 0]),
        }
        .append_to(&mut wasm);
        let error = launch_contract(&wasm).expect_err("unknown metadata cannot be ignored");
        assert!(error.contains("unsupported version"), "{error}");
    }
}
