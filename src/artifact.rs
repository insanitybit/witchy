//! Source-derived metadata carried by distributable Witchy wasm modules.

use std::borrow::Cow;

use wasm_encoder::{CustomSection, Section as _};

use witchy_syntax::ast;
use witchy_caps::capabilities;
use witchy_wir::layout::{
    HostLayoutContract, HostLayoutPolicy, LayoutBundle, LayoutInterner,
};

const LAUNCH_SECTION: &str = "witchy.launch";
const LAUNCH_VERSION: u8 = 1;
const LAYOUT_SECTION: &str = "witchy.layouts";

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

const CONSOLE_RIGHTS: [(&str, u8); 2] = [("Read", 1 << 0), ("Write", 1 << 1)];
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

/// Append a validated canonical descriptor graph to a distributable module.
/// The bundle is the exact transport used by workers and structured host
/// adapters; the artifact layer never rebuilds a layout from a source name.
pub fn embed_layout_bundle(mut wasm: Vec<u8>, bundle: &LayoutBundle) -> Vec<u8> {
    CustomSection {
        name: Cow::Borrowed(LAYOUT_SECTION),
        data: Cow::Owned(bundle.canonical_bytes()),
    }
    .append_to(&mut wasm);
    wasm
}

/// Read and fully validate an artifact's specialized layout graph. Unknown
/// schemas, invalid digests, missing children, duplicates, and dangling roots
/// fail before callers can instantiate the module or select a host adapter.
pub fn layout_bundle(
    wasm: &[u8],
) -> Result<Option<(LayoutBundle, LayoutInterner)>, String> {
    let decoded = layout_bundle_payload(wasm)?
        .map(LayoutBundle::decode_canonical)
        .transpose()
        .map_err(|error| invalid_layout_contract(&error.to_string()))?;
    authenticate_artifact_import_layouts(wasm, decoded.as_ref())?;
    Ok(decoded)
}

fn authenticate_artifact_import_layouts(
    wasm: &[u8],
    decoded: Option<&(LayoutBundle, LayoutInterner)>,
) -> Result<(), String> {
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|error| invalid_layout_contract(&error.to_string()))?;
        let wasmparser::Payload::ImportSection(reader) = payload else { continue };
        for import in reader.into_imports() {
            let import = import.map_err(|error| invalid_layout_contract(&error.to_string()))?;
            if import.module != "witchy"
                || !matches!(import.ty, wasmparser::TypeRef::Func(_))
            {
                continue;
            }
            let info = witchy_wir::wir_prelude::abi_import_info(import.name).ok_or_else(|| {
                invalid_layout_contract(&format!(
                    "host import `{}` has no generated ABI metadata",
                    import.name,
                ))
            })?;
            info.specialized_layouts
                .authenticate(decoded.map(|(bundle, layouts)| (bundle, layouts)))
                .map_err(|error| {
                    invalid_layout_contract(&format!(
                        "host import `{}` layout contract rejected: {error}",
                        import.name,
                    ))
                })?;
        }
    }
    Ok(())
}

/// Authenticate one generated host-import layout contract against the exact
/// canonical descriptor graph carried by an artifact. This is the packaging
/// and trusted-link boundary used before a concrete adapter can be selected.
pub fn authenticate_host_layout_contract(
    wasm: &[u8],
    contract: HostLayoutContract<'_>,
) -> Result<HostLayoutPolicy, String> {
    let decoded = layout_bundle(wasm)?;
    contract
        .authenticate(decoded.as_ref().map(|(bundle, layouts)| (bundle, layouts)))
        .map_err(|error| invalid_layout_contract(&error.to_string()))
}

/// Authenticate the checked-in ABI metadata for one canonical `witchy` host
/// import. Every production import currently carries an explicit reject-all
/// contract; a future nonempty set must arrive with its real adapter.
pub fn import_layout_policy(wasm: &[u8], import: &str) -> Result<HostLayoutPolicy, String> {
    let info = witchy_wir::wir_prelude::abi_import_info(import)
        .ok_or_else(|| invalid_layout_contract(&format!("unknown host import `{import}`")))?;
    authenticate_host_layout_contract(wasm, info.specialized_layouts)
}

/// Return the exact encoded layout payload after checking section uniqueness.
/// Descriptor validation remains in [`layout_bundle`] so trusted packaging can
/// digest the original bytes without inventing another encoding.
pub fn layout_bundle_payload(wasm: &[u8]) -> Result<Option<&[u8]>, String> {
    let mut found = None;
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        let payload = payload.map_err(|error| invalid_layout_contract(&error.to_string()))?;
        let wasmparser::Payload::CustomSection(section) = payload else { continue };
        if section.name() != LAYOUT_SECTION {
            continue;
        }
        if found.is_some() {
            return Err(invalid_layout_contract("duplicate section"));
        }
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
        "Console" => &CONSOLE_RIGHTS,
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

fn invalid_layout_contract(detail: &str) -> String {
    format!("invalid `{LAYOUT_SECTION}` metadata: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_encoder::{EntityType, ImportSection, Module, TypeSection};
    use witchy_syntax::ast::Type;
    use witchy_wir::layout::{
        ClosedTypeResolver, LayoutId, ResolvedNamed, ScalarKind,
    };

    struct ScalarResolver;

    impl ClosedTypeResolver for ScalarResolver {
        fn resolve_named<'a>(
            &'a self,
            name: &str,
            arguments: &[Type],
        ) -> Option<ResolvedNamed<'a>> {
            if name == "Int" && arguments.is_empty() {
                Some(ResolvedNamed::Scalar(ScalarKind::Int))
            } else {
                None
            }
        }
    }

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

    #[test]
    fn layout_bundle_round_trips_through_artifact_metadata() {
        let mut layouts = LayoutInterner::new();
        let int = layouts
            .intern_type(&Type::Named("Int".to_owned(), Vec::new()), &ScalarResolver)
            .unwrap();
        let bundle = LayoutBundle::from_interner(&layouts, [int]).unwrap();
        let wasm = embed_layout_bundle(Module::new().finish(), &bundle);
        wasmparser::validate(&wasm).expect("layout metadata keeps the module valid");

        let payload = layout_bundle_payload(&wasm).unwrap().expect("layout section");
        assert_eq!(payload, bundle.canonical_bytes());
        let (decoded, imported) = layout_bundle(&wasm).unwrap().expect("layout bundle");
        assert_eq!(decoded, bundle);
        assert!(imported.get(int).is_some());
    }

    #[test]
    fn artifact_authenticates_generated_host_layout_contracts() {
        let mut layouts = LayoutInterner::new();
        let int = layouts
            .intern_type(&Type::Named("Int".to_owned(), Vec::new()), &ScalarResolver)
            .unwrap();
        let bundle = LayoutBundle::from_interner(&layouts, [int]).unwrap();
        let wasm = embed_layout_bundle(Module::new().finish(), &bundle);
        let exact = HostLayoutContract {
            schema: witchy_wir::layout::LAYOUT_SCHEMA_VERSION,
            accepted: &[int],
        };
        let policy = authenticate_host_layout_contract(&wasm, exact).unwrap();
        assert_eq!(
            policy.decide(&layouts, int),
            witchy_wir::layout::HostLayoutDecision::Exact,
        );

        let unknown = LayoutId::from_bytes([0x51; 32]);
        let error = authenticate_host_layout_contract(
            &wasm,
            HostLayoutContract {
                schema: witchy_wir::layout::LAYOUT_SCHEMA_VERSION,
                accepted: &[unknown],
            },
        )
        .unwrap_err();
        assert!(error.contains("host accepts unknown artifact layout"), "{error}");

        let bare = Module::new().finish();
        let policy = import_layout_policy(&bare, "print").unwrap();
        assert_eq!(
            policy.decide(&LayoutInterner::new(), unknown),
            witchy_wir::layout::HostLayoutDecision::Reject,
        );

        let mut types = TypeSection::new();
        types.ty().function([], []);
        let mut imports = ImportSection::new();
        imports.import("witchy", "not_generated", EntityType::Function(0));
        let mut unknown_import = Module::new();
        unknown_import.section(&types);
        unknown_import.section(&imports);
        let error = layout_bundle(&unknown_import.finish()).unwrap_err();
        assert!(error.contains("has no generated ABI metadata"), "{error}");
    }

    #[test]
    fn duplicate_or_unknown_layout_metadata_fails_closed() {
        let empty = LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap();
        let wasm = embed_layout_bundle(Module::new().finish(), &empty);
        let duplicate = embed_layout_bundle(wasm, &empty);
        let error = layout_bundle(&duplicate).expect_err("duplicate metadata cannot be ignored");
        assert!(error.contains("duplicate section"), "{error}");

        let mut unknown = Module::new().finish();
        let mut payload = empty.canonical_bytes();
        payload[8..12].copy_from_slice(&2u32.to_le_bytes());
        CustomSection {
            name: Cow::Borrowed(LAYOUT_SECTION),
            data: Cow::Owned(payload),
        }
        .append_to(&mut unknown);
        let error = layout_bundle(&unknown).expect_err("unknown schema cannot be ignored");
        assert!(error.contains("unsupported layout schema 2"), "{error}");

        let policy = witchy_wir::layout::HostLayoutPolicy::new([
            LayoutId::from_bytes([1; 32]),
        ]);
        assert_eq!(
            policy.decide(&LayoutInterner::new(), LayoutId::from_bytes([2; 32])),
            witchy_wir::layout::HostLayoutDecision::Reject
        );
    }
}
