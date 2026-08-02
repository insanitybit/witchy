use std::collections::BTreeMap;

use witchy_syntax::ast::{Expr, Item, Module, Type, TypeDef, Variant};

use crate::layout::{
    ClosedTypeResolver, FieldKind, HeaderLayout, LAYOUT_SCHEMA_VERSION, LayoutError, LayoutId,
    LayoutInterner, LayoutKind, LayoutSize, OperationShape, OwnershipPosition, RcHeader,
    ReferenceKind, ResolvedNamed, ScalarKind, StorageClass,
};

struct TestResolver<'a> {
    definitions: BTreeMap<&'a str, &'a TypeDef>,
}

impl<'a> TestResolver<'a> {
    fn new(module: &'a Module) -> Self {
        let definitions = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Type(definition) => Some((definition.name.as_str(), definition)),
                _ => None,
            })
            .collect();
        Self { definitions }
    }

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
                if definition.packed {
                    ResolvedNamed::PackedRecord(definition)
                } else {
                    ResolvedNamed::ClosedSum(definition)
                }
            }
        })
    }
}

struct MismatchResolver<'a> {
    normal: TestResolver<'a>,
    trigger: &'a str,
    wrong: &'a TypeDef,
}

impl ClosedTypeResolver for MismatchResolver<'_> {
    fn resolve_named<'a>(&'a self, name: &str, arguments: &[Type]) -> Option<ResolvedNamed<'a>> {
        if name == self.trigger {
            Some(ResolvedNamed::PackedRecord(self.wrong))
        } else {
            self.normal.resolve_named(name, arguments)
        }
    }
}

fn named(name: &str) -> Type {
    Type::Named(name.to_owned(), Vec::new())
}

fn nominal(name: &str, arguments: Vec<Type>) -> Type {
    Type::Named(name.to_owned(), arguments)
}

fn parse(source: &str) -> Module {
    witchy_syntax::parser::parse_module(source).unwrap()
}

fn record(name: &str, packed: bool, fields: Vec<(&str, Type)>) -> TypeDef {
    let (field_names, field_types): (Vec<_>, Vec<_>) = fields
        .into_iter()
        .map(|(name, ty)| (name.to_owned(), ty))
        .unzip();
    TypeDef {
        name: name.to_owned(),
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
    }
}

fn sum(name: &str, arities: &[usize]) -> TypeDef {
    TypeDef {
        name: name.to_owned(),
        params: Vec::new(),
        variants: arities
            .iter()
            .enumerate()
            .map(|(index, arity)| Variant {
                name: format!("V{index}"),
                line: index as u32 + 1,
                fields: vec![named("Int"); *arity],
                field_names: Vec::new(),
                field_lines: Vec::new(),
            })
            .collect(),
        derives: Vec::new(),
        sealed: false,
        is_capability: false,
        grantable: false,
        packed: false,
        partial_eq_derived: false,
    }
}

#[test]
fn scalar_widths_and_schema_are_explicit() {
    let resolver = TestResolver { definitions: BTreeMap::new() };
    let mut layouts = LayoutInterner::new();
    let cases = [
        ("Bool", ScalarKind::Bool, 1, 1),
        ("Int", ScalarKind::Int, 8, 8),
        ("Float", ScalarKind::Float, 8, 8),
        ("Duration", ScalarKind::Duration, 8, 8),
    ];
    for (name, kind, size, alignment) in cases {
        let id = layouts.intern_type(&named(name), &resolver).unwrap();
        let descriptor = layouts.get(id).unwrap();
        assert_eq!(descriptor.schema_version(), LAYOUT_SCHEMA_VERSION);
        assert_eq!(descriptor.size(), LayoutSize::Fixed(size));
        assert_eq!(descriptor.alignment(), alignment);
        assert_eq!(descriptor.kind(), &LayoutKind::Scalar(kind));
        assert_eq!(descriptor.operations().drop(), &OperationShape::None);
    }
    assert_eq!(LAYOUT_SCHEMA_VERSION, 1);
}

#[test]
fn resolved_nominal_fields_drive_padding_and_generic_substitution() {
    let module = parse(
        "type Boxed(a) packed:\n    enabled: Bool\n    value: a\n\n\
         type Outer packed:\n    first: Bool\n    inner: Boxed(Int)\n",
    );
    let resolver = TestResolver::new(&module);
    let mut layouts = LayoutInterner::new();
    let boxed_id = layouts
        .intern_type(&nominal("Boxed", vec![named("Int")]), &resolver)
        .unwrap();
    let boxed = layouts.get(boxed_id).unwrap();
    assert_eq!(boxed.size(), LayoutSize::Fixed(16));
    assert_eq!(boxed.fields()[0].offset(), 0);
    assert_eq!(boxed.fields()[0].kind(), FieldKind::Scalar(ScalarKind::Bool));
    assert_eq!(boxed.fields()[1].offset(), 8);
    assert_eq!(boxed.fields()[1].kind(), FieldKind::Scalar(ScalarKind::Int));

    let outer_id = layouts.intern_type(&named("Outer"), &resolver).unwrap();
    let outer = layouts.get(outer_id).unwrap();
    assert_eq!(outer.size(), LayoutSize::Fixed(24));
    assert_eq!(outer.alignment(), 8);
    assert_eq!(outer.fields()[1].offset(), 8);
    assert_eq!(outer.fields()[1].kind(), FieldKind::Inline(boxed_id));
}

#[test]
fn layout_ids_are_order_independent_and_have_a_frozen_vector() {
    let resolver = TestResolver { definitions: BTreeMap::new() };
    let tuple = Type::Tuple(vec![named("Bool"), named("Int")]);
    let mut first = LayoutInterner::new();
    first.intern_type(&named("Bool"), &resolver).unwrap();
    first.intern_type(&named("Int"), &resolver).unwrap();
    let first_tuple = first.intern_type(&tuple, &resolver).unwrap();

    let mut second = LayoutInterner::new();
    second.intern_type(&named("Int"), &resolver).unwrap();
    second.intern_type(&named("Bool"), &resolver).unwrap();
    let second_tuple = second.intern_type(&tuple, &resolver).unwrap();

    assert_eq!(first_tuple, second_tuple);
    assert_eq!(first.get(first_tuple).unwrap().layout_id(), first_tuple);
    assert_eq!(LayoutId::from_bytes(*first_tuple.as_bytes()), first_tuple);
    assert_eq!(
        first_tuple.to_hex(),
        "9c38936f3395f08d396e54858f505ad4ec76ea984f7645ab7c14db172b129e23"
    );
}

#[test]
fn nominal_names_do_not_change_physical_identity_but_shapes_do() {
    let definitions = [
        record("Point", true, vec![("x", named("Int")), ("y", named("Int"))]),
        record("Vector", true, vec![("dx", named("Int")), ("dy", named("Int"))]),
        record("Flags", true, vec![("a", named("Bool")), ("b", named("Bool"))]),
    ];
    let resolver = TestResolver::with_definitions(&definitions);
    let mut layouts = LayoutInterner::new();
    let point = layouts.intern_type(&named("Point"), &resolver).unwrap();
    let vector = layouts.intern_type(&named("Vector"), &resolver).unwrap();
    let flags = layouts.intern_type(&named("Flags"), &resolver).unwrap();
    assert_eq!(point, vector);
    assert_ne!(point, flags);
}

#[test]
fn packed_list_records_header_stride_ownership_and_operations() {
    let resolver = TestResolver { definitions: BTreeMap::new() };
    let pair = Type::Tuple(vec![named("Bool"), named("Int")]);
    let list_type = nominal("List", vec![pair]);
    let mut layouts = LayoutInterner::new();
    let list = layouts.intern_type(&list_type, &resolver).unwrap();
    let descriptor = layouts.get(list).unwrap();
    let LayoutKind::PackedList { element, rc } = descriptor.kind() else {
        panic!("expected packed list")
    };
    assert_eq!(*rc, RcHeader::Required);
    assert_eq!(descriptor.size(), LayoutSize::Dynamic { base: 8, stride: 16 });
    assert_eq!(descriptor.alignment(), 8);
    assert_eq!(descriptor.ownership(), &[OwnershipPosition::RootBuffer]);
    assert_eq!(
        descriptor.header(),
        HeaderLayout::PackedList {
            rc: RcHeader::Required,
            length_offset: 0,
            capacity_offset: 4,
            data_offset: 8,
        }
    );
    assert_eq!(
        descriptor.operations().serialization(),
        &OperationShape::PackedElements { element: *element, stride: 16 }
    );
}

#[test]
fn closed_sum_uses_a_fixed_tag_and_aligned_payload_band() {
    let definitions = [sum("Choice", &[0, 1]), sum("Wide", &[0; 257])];
    let resolver = TestResolver::with_definitions(&definitions);
    let mut layouts = LayoutInterner::new();
    let choice = layouts.intern_type(&named("Choice"), &resolver).unwrap();
    let descriptor = layouts.get(choice).unwrap();
    assert_eq!(descriptor.size(), LayoutSize::Fixed(16));
    assert_eq!(descriptor.alignment(), 8);
    assert!(descriptor.variant_layouts()[0].fields().is_empty());
    assert_eq!(descriptor.variant_layouts()[1].fields()[0].offset(), 8);
    assert_eq!(
        descriptor.variant_layouts()[1].fields()[0].kind(),
        FieldKind::Scalar(ScalarKind::Int)
    );
    let OperationShape::Variants { tag, .. } = descriptor.operations().copy() else {
        panic!("expected variant operation")
    };
    assert_eq!(*tag, ScalarKind::Tag8);

    let wide = layouts.intern_type(&named("Wide"), &resolver).unwrap();
    let OperationShape::Variants { tag, .. } = layouts.get(wide).unwrap().operations().copy() else {
        panic!("expected variant operation")
    };
    assert_eq!(*tag, ScalarKind::Tag16);
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
fn nominal_constructor_rejects_resolver_mismatch_at_the_nested_field() {
    let definitions = [
        record("Inner", true, vec![("value", named("Int"))]),
        record("Wrong", true, vec![("value", named("Bool"))]),
        record("Outer", true, vec![("child", named("Inner"))]),
    ];
    let normal = TestResolver::with_definitions(&definitions);
    let resolver = MismatchResolver {
        normal,
        trigger: "Inner",
        wrong: &definitions[1],
    };
    let error = LayoutInterner::new()
        .intern_type(&named("Outer"), &resolver)
        .unwrap_err();
    let LayoutError::ResolvedTypeMismatch { path, expected, actual } = error else {
        panic!("expected resolver mismatch")
    };
    assert_eq!(path.to_string(), "Outer.child");
    assert_eq!(expected, "Inner");
    assert_eq!(actual, "Wrong");
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
fn canonical_bytes_round_trip_and_import_with_child_validation() {
    let definitions = [record(
        "Point",
        true,
        vec![("x", named("Int")), ("y", named("Int"))],
    )];
    let resolver = TestResolver::with_definitions(&definitions);
    let mut source = LayoutInterner::new();
    let int_id = source.intern_type(&named("Int"), &resolver).unwrap();
    let point_id = source.intern_type(&named("Point"), &resolver).unwrap();
    let int_bytes = source.get(int_id).unwrap().canonical_bytes();
    let point_bytes = source.get(point_id).unwrap().canonical_bytes();

    let empty = LayoutInterner::new();
    assert!(matches!(
        empty.decode_canonical(&point_bytes),
        Err(LayoutError::UnknownLayout(_))
    ));

    let mut imported = LayoutInterner::new();
    imported.import_canonical(int_id, &int_bytes).unwrap();
    imported.validate_canonical(point_id, &point_bytes).unwrap();
    imported.import_canonical(point_id, &point_bytes).unwrap();
    assert_eq!(imported.get(point_id).unwrap().canonical_bytes(), point_bytes);
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
fn reflection_integration_exposes_logical_fields_and_no_physical_descriptor_state() {
    let module = parse("type Point packed:\n    x: Int\n    enabled: Bool\n");
    let resolver = TestResolver::new(&module);
    let mut layouts = LayoutInterner::new();
    let layout_id = layouts.intern_type(&named("Point"), &resolver).unwrap();
    let descriptor = layouts.get(layout_id).unwrap();
    let reflected = witchy_syntax::reflect::module_type_info_exprs(&module).unwrap();

    assert_eq!(reflected.len(), 1);
    let Expr::Ctor { name, args } = &reflected[0] else {
        panic!("reflection must produce meta.TypeInfo")
    };
    assert_eq!(name, "meta.TypeInfo");
    assert_eq!(args[0], Expr::Str("Point".to_owned()));
    let Expr::List(fields) = &args[3] else {
        panic!("record reflection must expose logical fields")
    };
    assert_eq!(fields.len(), 2);
    let reflected_debug = format!("{reflected:?}");
    assert!(reflected_debug.contains("x") && reflected_debug.contains("enabled"));
    assert!(!reflected_debug.contains(&layout_id.to_hex()));
    for physical in ["offset", "padding", "header", "ownership", "stride"] {
        assert!(!reflected_debug.contains(physical), "reflection leaked {physical}");
    }
    let bytes = descriptor.canonical_bytes();
    for logical in [b"Point".as_slice(), b"x".as_slice(), b"enabled".as_slice()] {
        assert!(!bytes.windows(logical.len()).any(|window| window == logical));
    }
}

#[test]
fn dynamic_values_cannot_nest_inline() {
    let resolver = TestResolver { definitions: BTreeMap::new() };
    let ty = Type::Tuple(vec![nominal("List", vec![named("Int")])]);
    assert!(matches!(
        LayoutInterner::new().intern_type(&ty, &resolver),
        Err(LayoutError::DynamicInlineField(_))
    ));
}
