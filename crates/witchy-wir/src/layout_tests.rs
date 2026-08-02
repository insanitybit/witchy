use witchy_syntax::ast::{Item, Type, TypeDef, Variant};

use crate::layout::{
    FieldKind, HeaderLayout, LAYOUT_SCHEMA_VERSION, LayoutError, LayoutId, LayoutInterner,
    LayoutKind, LayoutSize, OperationShape, OwnershipPosition, RcHeader, ReferenceKind, ScalarKind,
    StorageClass,
};

fn record(name: &str, packed: bool, field_count: usize) -> TypeDef {
    TypeDef {
        name: name.to_owned(),
        params: Vec::new(),
        variants: vec![Variant {
            name: name.to_owned(),
            line: 1,
            fields: (0..field_count)
                .map(|_| Type::Named("field".to_owned(), Vec::new()))
                .collect(),
            field_names: (0..field_count).map(|index| format!("f{index}")).collect(),
            field_lines: vec![1; field_count],
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
                fields: (0..*arity)
                    .map(|_| Type::Named("field".to_owned(), Vec::new()))
                    .collect(),
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
    let mut layouts = LayoutInterner::new();
    let cases = [
        (ScalarKind::Bool, 1, 1),
        (ScalarKind::Int, 8, 8),
        (ScalarKind::Float, 8, 8),
        (ScalarKind::Duration, 8, 8),
        (ScalarKind::U32, 4, 4),
        (ScalarKind::Tag8, 1, 1),
        (ScalarKind::Tag16, 2, 2),
        (ScalarKind::Tag32, 4, 4),
    ];
    for (kind, size, alignment) in cases {
        let id = layouts.intern_scalar(kind).unwrap();
        let descriptor = layouts.get(id).unwrap();
        assert_eq!(descriptor.schema_version, LAYOUT_SCHEMA_VERSION);
        assert_eq!(descriptor.size, LayoutSize::Fixed(size));
        assert_eq!(descriptor.alignment, alignment);
        assert_eq!(descriptor.kind, LayoutKind::Scalar(kind));
        assert_eq!(descriptor.operations.drop, OperationShape::None);
    }
    assert_eq!(LAYOUT_SCHEMA_VERSION, 1);
}

#[test]
fn tuple_and_nested_record_padding_is_deterministic() {
    let mut layouts = LayoutInterner::new();
    let bool_id = layouts.intern_scalar(ScalarKind::Bool).unwrap();
    let int_id = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let inner_id = layouts.intern_tuple(&[bool_id, int_id]).unwrap();
    let inner = layouts.get(inner_id).unwrap();
    assert_eq!(inner.size, LayoutSize::Fixed(16));
    assert_eq!(inner.alignment, 8);
    assert_eq!(inner.fields[0].offset, 0);
    assert_eq!(inner.fields[0].kind, FieldKind::Scalar(ScalarKind::Bool));
    assert_eq!(inner.fields[1].offset, 8);
    assert_eq!(inner.fields[1].kind, FieldKind::Scalar(ScalarKind::Int));

    let outer_id = layouts
        .intern_packed_record(&record("Outer", true, 2), &[bool_id, inner_id])
        .unwrap();
    let outer = layouts.get(outer_id).unwrap();
    assert_eq!(outer.size, LayoutSize::Fixed(24));
    assert_eq!(outer.alignment, 8);
    assert_eq!(outer.fields[0].offset, 0);
    assert_eq!(outer.fields[1].offset, 8);
    assert_eq!(outer.fields[1].kind, FieldKind::Inline(inner_id));
}

#[test]
fn layout_ids_are_order_independent_and_have_a_frozen_vector() {
    let mut first = LayoutInterner::new();
    let first_bool = first.intern_scalar(ScalarKind::Bool).unwrap();
    let first_int = first.intern_scalar(ScalarKind::Int).unwrap();
    let first_tuple = first.intern_tuple(&[first_bool, first_int]).unwrap();

    let mut second = LayoutInterner::new();
    let second_int = second.intern_scalar(ScalarKind::Int).unwrap();
    let second_bool = second.intern_scalar(ScalarKind::Bool).unwrap();
    let second_tuple = second.intern_tuple(&[second_bool, second_int]).unwrap();

    assert_eq!(first_bool, second_bool);
    assert_eq!(first_int, second_int);
    assert_eq!(first_tuple, second_tuple);
    assert_eq!(first.get(first_tuple).unwrap().layout_id(), first_tuple);
    assert_eq!(LayoutId::from_bytes(*first_tuple.as_bytes()), first_tuple);
    assert_eq!(
        first_tuple.to_hex(),
        "844b107c710523f36340fb3708594654287ea69ea90b670965f9780150a22b97"
    );
}

#[test]
fn nominal_names_do_not_change_physical_identity_but_shapes_do() {
    let mut layouts = LayoutInterner::new();
    let bool_id = layouts.intern_scalar(ScalarKind::Bool).unwrap();
    let int_id = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let point = layouts
        .intern_packed_record(&record("Point", true, 2), &[int_id, int_id])
        .unwrap();
    let vector = layouts
        .intern_packed_record(&record("Vector", true, 2), &[int_id, int_id])
        .unwrap();
    let flags = layouts
        .intern_packed_record(&record("Flags", true, 2), &[bool_id, bool_id])
        .unwrap();
    assert_eq!(point, vector);
    assert_ne!(point, flags);
    assert_eq!(layouts.len(), 4);
}

#[test]
fn packed_list_records_header_stride_ownership_and_operations() {
    let mut layouts = LayoutInterner::new();
    let bool_id = layouts.intern_scalar(ScalarKind::Bool).unwrap();
    let int_id = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let pair = layouts.intern_tuple(&[bool_id, int_id]).unwrap();
    let list = layouts.intern_packed_list(pair, RcHeader::Required).unwrap();
    let descriptor = layouts.get(list).unwrap();

    assert_eq!(
        descriptor.kind,
        LayoutKind::PackedList { element: pair, element_stride: 16 }
    );
    assert_eq!(descriptor.size, LayoutSize::Dynamic { base: 8, stride: 16 });
    assert_eq!(descriptor.alignment, 8);
    assert_eq!(descriptor.ownership, vec![OwnershipPosition::RootBuffer]);
    assert_eq!(
        descriptor.header,
        HeaderLayout::PackedList {
            rc: RcHeader::Required,
            length_offset: 0,
            capacity_offset: 4,
            data_offset: 8,
        }
    );
    assert_eq!(
        descriptor.operations.serialization,
        OperationShape::PackedElements { element: pair, stride: 16 }
    );
}

#[test]
fn closed_sum_uses_a_fixed_tag_and_aligned_payload_band() {
    let mut layouts = LayoutInterner::new();
    let int_id = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let choice = layouts
        .intern_closed_sum(&sum("Choice", &[0, 1]), &[vec![], vec![int_id]])
        .unwrap();
    let descriptor = layouts.get(choice).unwrap();

    assert_eq!(descriptor.size, LayoutSize::Fixed(16));
    assert_eq!(descriptor.alignment, 8);
    let LayoutKind::ClosedSum { tag, payload_offset, variants } = &descriptor.kind else {
        panic!("expected closed sum")
    };
    assert_eq!(*tag, ScalarKind::Tag8);
    assert_eq!(*payload_offset, 8);
    assert!(variants[0].fields.is_empty());
    assert_eq!(variants[1].fields[0].offset, 8);
    assert_eq!(variants[1].fields[0].kind, FieldKind::Scalar(ScalarKind::Int));

    let wide_definition = sum("Wide", &[0; 257]);
    let wide_variants = vec![Vec::new(); 257];
    let wide = layouts
        .intern_closed_sum(&wide_definition, &wide_variants)
        .unwrap();
    let LayoutKind::ClosedSum { tag, .. } = layouts.get(wide).unwrap().kind else {
        panic!("expected closed sum")
    };
    assert_eq!(tag, ScalarKind::Tag16);
}

#[test]
fn references_are_classified_and_rejected_before_inline_layout() {
    let layouts = LayoutInterner::new();
    let cases = [
        (ReferenceKind::Owning, StorageClass::OwningReference),
        (ReferenceKind::BorrowedView, StorageClass::BorrowedView),
        (ReferenceKind::ExternRef, StorageClass::ExternRef),
        (ReferenceKind::GcReference, StorageClass::GcReference),
        (ReferenceKind::Capability, StorageClass::CapabilityReference),
    ];
    for (kind, class) in cases {
        assert_eq!(
            layouts.reject_reference(kind),
            LayoutError::ReferenceNotInline { kind, class }
        );
    }

    let mut capability = record("Database", true, 0);
    capability.is_capability = true;
    assert_eq!(
        LayoutInterner::new().intern_packed_record(&capability, &[]),
        Err(LayoutError::CapabilityDefinition)
    );
}

#[test]
fn invalid_nominal_and_dynamic_nesting_are_loud() {
    let mut layouts = LayoutInterner::new();
    let int_id = layouts.intern_scalar(ScalarKind::Int).unwrap();
    assert_eq!(
        layouts.intern_packed_record(&record("Plain", false, 1), &[int_id]),
        Err(LayoutError::NotPackedRecord)
    );
    assert_eq!(
        layouts.intern_packed_record(&record("Pair", true, 2), &[int_id]),
        Err(LayoutError::FieldCount { expected: 2, actual: 1 })
    );

    let list = layouts.intern_packed_list(int_id, RcHeader::Required).unwrap();
    assert_eq!(
        layouts.intern_tuple(&[list]),
        Err(LayoutError::DynamicInlineField(list))
    );
}

#[test]
fn physical_layout_does_not_enter_logical_reflection() {
    let module = witchy_syntax::parser::parse_module(
        "type Point packed:\n    x: Int\n    enabled: Bool\n",
    )
    .unwrap();
    let reflected_before = witchy_syntax::reflect::module_type_info_exprs(&module).unwrap();
    let definition = module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Type(definition) => Some(definition),
            _ => None,
        })
        .unwrap();

    let mut layouts = LayoutInterner::new();
    let int_id = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let bool_id = layouts.intern_scalar(ScalarKind::Bool).unwrap();
    layouts
        .intern_packed_record(definition, &[int_id, bool_id])
        .unwrap();

    let reflected_after = witchy_syntax::reflect::module_type_info_exprs(&module).unwrap();
    assert_eq!(reflected_before, reflected_after);
    let logical_debug = format!("{reflected_after:?}");
    for physical_name in ["LayoutId", "offset", "padding", "header", "ownership"] {
        assert!(!logical_debug.contains(physical_name), "leaked {physical_name}");
    }
}
