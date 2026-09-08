use witchy_wir::layout::{LayoutBundle, LayoutInterner, LayoutKind, PointerSlot, ScalarKind};

#[test]
fn nullable_owned_slot_has_a_distinct_canonical_transport_identity() {
    let mut layouts = LayoutInterner::new();
    let pointee = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let owned = layouts
        .intern_runtime_pointer_value_slot(pointee, PointerSlot::Owned)
        .unwrap();
    let nullable = layouts
        .intern_runtime_pointer_value_slot(pointee, PointerSlot::OwnedNullable)
        .unwrap();
    assert_ne!(owned, nullable);

    let bundle = LayoutBundle::from_interner(&layouts, [nullable]).unwrap();
    let bytes = bundle.canonical_bytes();
    let (decoded, imported) = LayoutBundle::decode_canonical(&bytes).unwrap();
    assert_eq!(decoded.canonical_bytes(), bytes);
    assert!(matches!(
        imported.get(nullable).unwrap().kind(),
        LayoutKind::RuntimePointerValueSlot {
            pointee: actual,
            semantics: PointerSlot::OwnedNullable,
        } if *actual == pointee
    ));
}
