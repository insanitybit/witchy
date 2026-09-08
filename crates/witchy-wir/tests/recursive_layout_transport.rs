use witchy_wir::layout::{LayoutBundle, LayoutInterner, PointerSlot, ScalarKind};

fn tree(layouts: &mut LayoutInterner, key: [u8; 32]) -> (witchy_wir::layout::LayoutId, witchy_wir::layout::LayoutId) {
    let nominal = layouts.intern_opaque_nominal(key).unwrap();
    let tag = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let child = layouts.intern_runtime_pointer_value_slot(nominal, PointerSlot::Owned).unwrap();
    let body = layouts.intern_packed_record(vec![tag, child, child]).unwrap();
    (nominal, body)
}

#[test]
fn recursive_layout_binding_roundtrips_without_recursive_hashing() {
    let mut layouts = LayoutInterner::new();
    let (nominal, body) = tree(&mut layouts, [1; 32]);
    layouts.bind_nominal(nominal, body).unwrap();
    let bundle = LayoutBundle::from_interner(&layouts, [body]).unwrap();
    let bytes = bundle.canonical_bytes();
    let (decoded, imported) = LayoutBundle::decode_canonical(&bytes).unwrap();
    assert_eq!(decoded.canonical_bytes(), bytes);
    assert_eq!(imported.resolve_nominal(nominal).unwrap(), body);
    assert_eq!(imported.get(nominal).unwrap().layout_id(), nominal);
    assert_eq!(imported.resolve_nominal(body).unwrap(), body);
}

#[test]
fn recursive_layout_refuses_unbound_rebound_and_inline_identities() {
    let mut layouts = LayoutInterner::new();
    let (nominal, body) = tree(&mut layouts, [2; 32]);
    assert!(LayoutBundle::from_interner(&layouts, [body]).is_err());
    assert!(layouts.resolve_nominal(nominal).is_err());
    assert!(layouts.intern_packed_record(vec![nominal]).is_err());
    assert!(layouts.bind_nominal(nominal, nominal).is_err());
    assert!(layouts.bind_nominal(body, nominal).is_err());
    layouts.bind_nominal(nominal, body).unwrap();
    let other = layouts.intern_packed_record(vec![]).unwrap();
    assert!(layouts.bind_nominal(nominal, other).is_err());
    assert_eq!(layouts.resolve_nominal(nominal).unwrap(), body);
}

#[test]
fn nominal_binding_changes_the_authenticated_bundle() {
    let mut left = LayoutInterner::new();
    let (nominal, body) = tree(&mut left, [3; 32]);
    left.bind_nominal(nominal, body).unwrap();
    let expected = LayoutBundle::from_interner(&left, [nominal]).unwrap().canonical_bytes();
    let mut right = LayoutInterner::new();
    let identity = right.intern_opaque_nominal([3; 32]).unwrap();
    let other_body = right.intern_packed_record(vec![]).unwrap();
    right.bind_nominal(identity, other_body).unwrap();
    assert_ne!(expected, LayoutBundle::from_interner(&right, [identity]).unwrap().canonical_bytes());
    let mut missing_binding = expected.clone();
    missing_binding.truncate(missing_binding.len() - 64);
    assert!(LayoutBundle::decode_canonical(&missing_binding).is_err());
}

#[test]
fn recursive_layout_does_not_grant_production_encoder_authority() {
    use witchy_wir::canonical_safewir::{
        CanonicalModule, CheckedSourceIdentity, InitialCheckError, LoweredModuleParts,
        Revision, check_initial,
    };
    let mut layouts = LayoutInterner::new();
    let (nominal, body) = tree(&mut layouts, [4; 32]);
    layouts.bind_nominal(nominal, body).unwrap();
    let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest([0; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [body]).unwrap(),
        signatures: vec![],
        imports: vec![],
        functions: vec![],
        proper_tail_sites: vec![],
    });
    assert!(matches!(check_initial(candidate), Err(InitialCheckError::InvalidLayoutBundle(reason))
        if reason == "recursive nominal values require canonical ownership and destruction support"));
}

#[test]
fn mutually_recursive_bindings_have_canonical_closed_transport() {
    let mut layouts = LayoutInterner::new();
    let first = layouts.intern_opaque_nominal([5; 32]).unwrap();
    let second = layouts.intern_opaque_nominal([6; 32]).unwrap();
    let first_slot = layouts.intern_runtime_pointer_value_slot(first, PointerSlot::Owned).unwrap();
    let second_slot = layouts.intern_runtime_pointer_value_slot(second, PointerSlot::Owned).unwrap();
    let first_body = layouts.intern_packed_record(vec![second_slot]).unwrap();
    let second_body = layouts.intern_packed_record(vec![first_slot]).unwrap();
    layouts.bind_nominal(first, first_body).unwrap();
    assert!(LayoutBundle::from_interner(&layouts, [first]).is_err());
    layouts.bind_nominal(second, second_body).unwrap();
    let bytes = LayoutBundle::from_interner(&layouts, [first]).unwrap().canonical_bytes();
    let (bundle, imported) = LayoutBundle::decode_canonical(&bytes).unwrap();
    assert_eq!(bundle.canonical_bytes(), bytes);
    assert_eq!(imported.resolve_nominal(first).unwrap(), first_body);
    assert_eq!(imported.resolve_nominal(second).unwrap(), second_body);
}
