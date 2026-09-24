use std::collections::BTreeMap;

use witchy_syntax::ast::{Type, TypeDef, Variant};

use crate::layout::{
    ClosedTypeResolver, HostLayoutContract,
    HostLayoutContractError, HostLayoutDecision, HostLayoutPolicy,
    LAYOUT_SCHEMA_VERSION, LayoutBundle, LayoutError, LayoutId, LayoutInterner,
    LayoutTransportError,
    RcHeader, ReferenceKind, ResolvedNamed, ScalarKind, StorageClass,
};

struct TestResolver<'a> {
    definitions: BTreeMap<&'a str, &'a TypeDef>,
}

impl<'a> TestResolver<'a> {
    fn with_definitions(definitions: &'a [TypeDef]) -> Self {
        Self {
            definitions: definitions
                .iter()
                .map(|definition| (definition.name.as_str(), definition))
                .collect(),
        }
    }
}

impl ClosedTypeResolver for TestResolver<'_> {
    fn resolve_named<'a>(&'a self, name: &str, _arguments: &[Type]) -> Option<ResolvedNamed<'a>> {
        Some(match name {
            "Bool" => ResolvedNamed::Scalar(ScalarKind::Bool),
            "Int" => ResolvedNamed::Scalar(ScalarKind::Int),
            "Float" => ResolvedNamed::Scalar(ScalarKind::Float),
            "Duration" => ResolvedNamed::Scalar(ScalarKind::Duration),
            "List" => ResolvedNamed::PackedList { rc: RcHeader::Required },
            "String" | "Bytes" => ResolvedNamed::Reference(ReferenceKind::Owning),
            "Console" => ResolvedNamed::Reference(ReferenceKind::Capability),
            _ => {
                let definition = *self.definitions.get(name)?;
                if definition.packed && definition.variants.len() == 1 {
                    ResolvedNamed::PackedRecord(definition)
                } else {
                    ResolvedNamed::ClosedSum(definition)
                }
            }
        })
    }
}

fn named(name: &str) -> Type {
    Type::Named(name.to_owned(), Vec::new())
}

fn record(name: &str, packed: bool, fields: Vec<(&str, Type)>) -> TypeDef {
    let (field_names, field_types): (Vec<_>, Vec<_>) = fields
        .into_iter()
        .map(|(name, ty)| (name.to_owned(), ty))
        .unzip();
    TypeDef {
        name: name.to_owned(),
        must_consume: false,
        params: Vec::new(),
        variants: vec![Variant {
            name: name.to_owned(),
            line: 1,
            fields: field_types,
            field_names,
            field_lines: Vec::new(),
        }],
        derives: Vec::new(),
        sealed: false,
        is_capability: false,
        grantable: false,
        packed,
        partial_eq_derived: false,
        public_state_derived: false,
    }
}

#[test]
fn nominal_constructor_rejects_reference_and_capability_fields_at_exact_paths() {
    let definitions = [
        record("TextHolder", true, vec![("text", named("String"))]),
        record("Authority", true, vec![("console", named("Console"))]),
    ];
    let resolver = TestResolver::with_definitions(&definitions);
    let mut layouts = LayoutInterner::new();

    let string_error = layouts.intern_type(&named("TextHolder"), &resolver).unwrap_err();
    let LayoutError::ReferenceNotInline { path, kind, class } = string_error else {
        panic!("expected field reference rejection")
    };
    assert_eq!(path.to_string(), "TextHolder.text");
    assert_eq!(kind, ReferenceKind::Owning);
    assert_eq!(class, StorageClass::OwningReference);

    let capability_error = layouts.intern_type(&named("Authority"), &resolver).unwrap_err();
    let LayoutError::ReferenceNotInline { path, kind, class } = capability_error else {
        panic!("expected field capability rejection")
    };
    assert_eq!(path.to_string(), "Authority.console");
    assert_eq!(kind, ReferenceKind::Capability);
    assert_eq!(class, StorageClass::CapabilityReference);
}

#[test]
fn capability_type_definition_cannot_masquerade_as_a_packed_record() {
    let mut capability = record("Database", true, Vec::new());
    capability.is_capability = true;
    let definitions = [capability];
    let resolver = TestResolver::with_definitions(&definitions);
    let error = LayoutInterner::new()
        .intern_type(&named("Database"), &resolver)
        .unwrap_err();
    assert!(matches!(
        error,
        LayoutError::CapabilityDefinition { path, name }
            if path.to_string() == "Database" && name == "Database"
    ));
}

#[test]
fn canonical_decoder_rejects_truncation_trailing_schema_and_hash_attacks() {
    let resolver = TestResolver { definitions: BTreeMap::new() };
    let mut layouts = LayoutInterner::new();
    let id = layouts.intern_type(&named("Int"), &resolver).unwrap();
    let bytes = layouts.get(id).unwrap().canonical_bytes();

    assert!(matches!(
        layouts.decode_canonical(&bytes[..bytes.len() - 1]),
        Err(LayoutError::Decode { .. })
    ));
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
        layouts.decode_canonical(&trailing),
        Err(LayoutError::TrailingBytes { .. })
    ));
    let mut wrong_schema = bytes.clone();
    wrong_schema[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        layouts.decode_canonical(&wrong_schema),
        Err(LayoutError::UnsupportedSchema { found: 2 })
    );
    assert!(matches!(
        layouts.validate_canonical(LayoutId::from_bytes([0; 32]), &bytes),
        Err(LayoutError::DigestMismatch { .. })
    ));

    // Scalar canonical offset: magic + schema + kind/scalar + size-kind/size.
    let mut forged_alignment = bytes.clone();
    forged_alignment[15..19].copy_from_slice(&3u32.to_le_bytes());
    assert!(matches!(
        layouts.decode_canonical(&forged_alignment),
        Err(LayoutError::DescriptorInvariant(_))
    ));
}

#[test]
fn layout_bundle_rejects_unknown_versions_truncation_and_dangling_roots() {
    let resolver = TestResolver { definitions: BTreeMap::new() };
    let mut layouts = LayoutInterner::new();
    let int = layouts.intern_type(&named("Int"), &resolver).unwrap();
    let bundle = LayoutBundle::from_interner(&layouts, [int]).unwrap();
    let bytes = bundle.canonical_bytes();

    assert!(matches!(
        LayoutBundle::decode_canonical(&bytes[..bytes.len() - 1]),
        Err(LayoutTransportError::Invalid("truncated layout bundle"))
    ));
    let mut wrong_bundle = bytes.clone();
    wrong_bundle[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        LayoutBundle::decode_canonical(&wrong_bundle).unwrap_err(),
        LayoutTransportError::UnsupportedBundleVersion(u32::MAX)
    );
    let mut truncated_v2 = bytes.clone();
    truncated_v2[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert!(matches!(
        LayoutBundle::decode_canonical(&truncated_v2),
        Err(LayoutTransportError::Invalid("truncated layout bundle"))
    ));
    let mut wrong_schema = bytes.clone();
    wrong_schema[8..12].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        LayoutBundle::decode_canonical(&wrong_schema).unwrap_err(),
        LayoutTransportError::UnsupportedLayoutSchema(2)
    );

    let mut dangling = LayoutBundle::from_interner(&layouts, []).unwrap().canonical_bytes();
    dangling[16..20].copy_from_slice(&1u32.to_le_bytes());
    dangling.extend_from_slice(LayoutId::from_bytes([9; 32]).as_bytes());
    assert!(matches!(
        LayoutBundle::decode_canonical(&dangling),
        Err(LayoutTransportError::UnknownRoot(_))
    ));
}

#[test]
fn generated_host_contract_authenticates_only_bundle_backed_layouts() {
    let resolver = TestResolver { definitions: BTreeMap::new() };
    let mut layouts = LayoutInterner::new();
    let exact = layouts.intern_type(&named("Int"), &resolver).unwrap();
    let bundle = LayoutBundle::from_interner(&layouts, [exact]).unwrap();
    let contract = HostLayoutContract { schema: LAYOUT_SCHEMA_VERSION, accepted: &[exact] };
    let policy = contract.authenticate(Some((&bundle, &layouts))).unwrap();
    assert_eq!(policy.decide(&layouts, exact), HostLayoutDecision::Exact);

    assert_eq!(
        HostLayoutContract { schema: LAYOUT_SCHEMA_VERSION + 1, accepted: &[] }
            .authenticate(None)
            .unwrap_err(),
        HostLayoutContractError::UnsupportedSchema(LAYOUT_SCHEMA_VERSION + 1),
    );
    assert_eq!(
        contract.authenticate(None).unwrap_err(),
        HostLayoutContractError::MissingLayoutBundle,
    );

    let unknown = LayoutId::from_bytes([0x91; 32]);
    assert_eq!(
        HostLayoutContract { schema: LAYOUT_SCHEMA_VERSION, accepted: &[unknown] }
            .authenticate(Some((&bundle, &layouts)))
            .unwrap_err(),
        HostLayoutContractError::UnknownAcceptedLayout(unknown),
    );
    assert_eq!(
        contract
            .authenticate(Some((&bundle, &LayoutInterner::new())))
            .unwrap_err(),
        HostLayoutContractError::BundleInternerMismatch(exact),
    );
}

#[test]
fn host_layout_policy_cannot_authorize_capability_or_reference_storage() {
    let definitions = [record("Authority", true, vec![("console", named("Console"))])];
    let resolver = TestResolver::with_definitions(&definitions);
    let mut layouts = LayoutInterner::new();
    let error = layouts.intern_type(&named("Authority"), &resolver).unwrap_err();
    assert!(matches!(
        error,
        LayoutError::ReferenceNotInline {
            kind: ReferenceKind::Capability,
            class: StorageClass::CapabilityReference,
            ..
        }
    ));

    let fabricated = LayoutId::from_bytes([0xc0; 32]);
    let policy = HostLayoutPolicy::new([fabricated]);
    assert_eq!(
        policy.decide(&layouts, fabricated),
        HostLayoutDecision::Reject,
        "a digest without a validated scalar descriptor cannot select a host adapter"
    );
    let empty = LayoutBundle::from_interner(&layouts, []).unwrap();
    assert_eq!(
        HostLayoutContract { schema: LAYOUT_SCHEMA_VERSION, accepted: &[fabricated] }
            .authenticate(Some((&empty, &layouts)))
            .unwrap_err(),
        HostLayoutContractError::UnknownAcceptedLayout(fabricated),
        "a fabricated capability digest cannot enter generated ABI metadata"
    );
}
