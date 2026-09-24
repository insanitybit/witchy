//! Owner-family cases that judge checked type definitions (RFC-0170 P4).
//! They moved out of `owner_authority::tests` so witchy-wir's tests need no
//! checker; `crates/witchy-lower/tests/safewir_checked_source.rs` runs each
//! one under the same name. See `canonical_safewir::checked_source_test_api`.

use super::*;
use super::super::closure_owner_contract::{ConstructorId, OwnerCapture, OwnerConstructor};
use super::super::{CheckedSourceIdentity, FunctionSignatureId, GcTypeId, LoweredModuleParts, Revision};
use crate::canonical_safewir::checked_source_test_api::SourceChecker;
use crate::layout::{LayoutBundle, LayoutInterner, ScalarKind};
use witchy_syntax::ast::Type;

/// The type definitions the checker retains for `source` (module `main`).
fn checked_records(checker: &dyn SourceChecker, source: &[u8]) -> BTreeMap<String, witchy_syntax::ast::TypeDef> {
    let mut records = None;
    checker.with_checked("main", source, &mut |checked| {
        records = Some(witchy_canonical_source::aggregates::type_definitions(checked.typed_module));
    });
    records.expect("checked source")
}

fn aggregate_fixture(checker: &dyn SourceChecker) -> (
    CanonicalModule<super::super::LoweredCandidate>,
    OwnerFamily,
    BTreeMap<String, witchy_syntax::ast::TypeDef>,
    crate::layout::LayoutInterner,
) {
    let records = checked_records(
        checker,
        b"type Envelope:\n    Value(Int)\n    Empty\n\nfn main():\n    let n = 0\n",
    );
    let source_type = witchy_canonical_source::aggregates::concrete_aggregate_identity(
        &Type::Named("Envelope".into(), Vec::new()), &records,
    ).expect("normalized concrete aggregate");
    let mut interner = LayoutInterner::new();
    let int = interner.intern_scalar(ScalarKind::Int).expect("Int layout");
    let environment = interner.intern_packed_record(vec![int, int]).expect("aggregate environment");
    let bundle = LayoutBundle::from_interner(&interner, [environment]).expect("layout bundle");
    let family = OwnerFamily {
        source: OwnerFamilySource::Aggregate(source_type),
        owner_type: GcTypeId::new(700),
        list_type: GcTypeId::new(701),
        signature: FunctionSignatureId::new(702),
        declaration_signature: FunctionSignatureId::new(703),
        environment,
        constructors: vec![
            OwnerConstructor {
                id: ConstructorId(9001),
                function: super::super::FunctionId::new(u32::MAX),
                witness: None,
                environment_tag: 0,
                captures: vec![OwnerCapture {
                    value_type: ValueType::Int,
                    storage: CaptureStorage::EnvironmentField(1),
                    transfer: CaptureTransfer::CopyScalar,
                }],
            },
            OwnerConstructor {
                id: ConstructorId(17),
                function: super::super::FunctionId::new(0),
                witness: None,
                environment_tag: 1,
                captures: Vec::new(),
            },
        ],
    };
    let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(1),
        checked_source: CheckedSourceIdentity::from_digest([8; 32]),
        layouts: bundle,
        signatures: Vec::new(),
        imports: Vec::new(),
        functions: Vec::new(),
        proper_tail_sites: Vec::new(),
    }).with_owner_families(vec![family.clone()]);
    (candidate, family, records, interner)
}

pub fn aggregate_owner_source_schema_rejects_type_and_storage_forgery(checker: &dyn SourceChecker) {
    let (candidate, family, records, layouts) = aggregate_fixture(checker);
    let OwnerFamilySource::Aggregate(source_type) = &family.source else { unreachable!() };
    validate_aggregate_family(&candidate, &family, source_type, &records, &layouts)
        .expect("exact checked aggregate schema");

    let mut forged_type = family.clone();
    forged_type.source = OwnerFamilySource::Aggregate(Type::Named("main.Forged".into(), Vec::new()));
    let OwnerFamilySource::Aggregate(source_type) = &forged_type.source else { unreachable!() };
    assert!(validate_aggregate_family(&candidate, &forged_type, source_type, &records, &layouts).is_err());

    let mut wrong_storage = family.clone();
    wrong_storage.constructors[0].captures[0].storage = CaptureStorage::OwnerField(2);
    assert!(validate_aggregate_family(&candidate, &wrong_storage,
        match &wrong_storage.source { OwnerFamilySource::Aggregate(ty) => ty, _ => unreachable!() },
        &records, &layouts).is_err());

    let mut wrong_type = family;
    wrong_type.constructors[0].captures[0].value_type = ValueType::Bool;
    assert!(validate_aggregate_family(&candidate, &wrong_type,
        match &wrong_type.source { OwnerFamilySource::Aggregate(ty) => ty, _ => unreachable!() },
        &records, &layouts).is_err());
}

pub fn exact_int_slice_record_field_rejects_erased_and_wrong_owner_carriers(checker: &dyn SourceChecker) {
    let records = checked_records(checker, b"type Packet:\n    data: &[Int]\n\nfn main():\n    ()\n");
    let packet = witchy_canonical_source::aggregates::canonical_owner_aggregate_identity(
        &Type::Named("Packet".into(), Vec::new()), &records,
    ).expect("exact aggregate identity");
    let slice_view = |element: &str| Type::Qualified(
        witchy_syntax::ast::TypeQual::View,
        Box::new(Type::Slice(Box::new(Type::Named(element.into(), Vec::new())))),
    );
    let mut layouts = LayoutInterner::new();
    let tag = layouts.intern_scalar(ScalarKind::Int).expect("tag layout");
    let environment = layouts.intern_packed_record(vec![tag]).expect("record environment");
    let bundle = LayoutBundle::from_interner(&layouts, [environment]).expect("layout bundle");
    let family = OwnerFamily {
        source: OwnerFamilySource::Aggregate(packet.clone()),
        owner_type: GcTypeId::new(700),
        list_type: GcTypeId::new(701),
        signature: FunctionSignatureId::new(702),
        declaration_signature: FunctionSignatureId::new(703),
        environment,
        constructors: vec![OwnerConstructor {
            id: ConstructorId(704),
            function: super::super::FunctionId::new(0),
            witness: None,
            environment_tag: 0,
            captures: vec![OwnerCapture {
                value_type: ValueType::Gc(GcTypeId::new(710)),
                storage: CaptureStorage::OwnerField(2),
                transfer: CaptureTransfer::CloneManaged,
            }],
        }],
    };
    let slice_family = |element: &str, id| OwnerFamily {
        source: OwnerFamilySource::SharedReference(slice_view(element)),
        owner_type: GcTypeId::new(id),
        list_type: GcTypeId::new(id + 1),
        signature: FunctionSignatureId::new(id + 2),
        declaration_signature: FunctionSignatureId::new(id + 3),
        environment,
        constructors: Vec::new(),
    };
    let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(1),
        checked_source: CheckedSourceIdentity::from_digest([8; 32]),
        layouts: bundle,
        signatures: Vec::new(),
        imports: Vec::new(),
        functions: Vec::new(),
        proper_tail_sites: Vec::new(),
    }).with_owner_families(vec![family.clone(), slice_family("Int", 710), slice_family("Float", 720)]);

    validate_aggregate_family(&candidate, &family, &packet, &records, &layouts)
        .expect("exact rooted Int slice occupies an owner field");

    let mut erased = family.clone();
    erased.constructors[0].captures[0].value_type = ValueType::Ref(environment);
    assert!(matches!(validate_aggregate_family(&candidate, &erased, &packet, &records, &layouts),
        Err(CandidateAbiBindingError::MissingCanonicalLayouts)),
        "an erased linear reference cannot claim the slice field");

    let mut wrong_storage = family.clone();
    wrong_storage.constructors[0].captures[0].storage = CaptureStorage::EnvironmentField(1);
    assert!(matches!(validate_aggregate_family(&candidate, &wrong_storage, &packet, &records, &layouts),
        Err(CandidateAbiBindingError::MissingCanonicalLayouts)),
        "a slice owner must remain in the authenticated owner field");

    let mut wrong_family = family;
    wrong_family.constructors[0].captures[0].value_type = ValueType::Gc(GcTypeId::new(720));
    assert!(matches!(validate_aggregate_family(&candidate, &wrong_family, &packet, &records, &layouts),
        Err(CandidateAbiBindingError::MissingCanonicalLayouts)),
        "the Float slice owner cannot claim an Int slice field");
}

pub fn aggregate_tuple_owner_binds_exact_fields_and_storage(checker: &dyn SourceChecker) {
    let (candidate, mut family, records, layouts) = aggregate_fixture(checker);
    let tuple = Type::Tuple(vec![Type::Named("Int".into(), Vec::new())]);
    family.source = OwnerFamilySource::Aggregate(tuple.clone());
    family.constructors.truncate(1);
    validate_aggregate_family(&candidate, &family, &tuple, &records, &layouts)
        .expect("structural tuple fields authorize the exact constructor");
    family.constructors[0].captures[0].storage = CaptureStorage::OwnerField(2);
    assert!(validate_aggregate_family(&candidate, &family, &tuple, &records, &layouts).is_err());
    family.constructors[0].captures[0].storage = CaptureStorage::EnvironmentField(1);
    let wrong = Type::Tuple(vec![Type::Named("Bool".into(), Vec::new())]);
    assert!(validate_aggregate_family(&candidate, &family, &wrong, &records, &layouts).is_err());
}

pub fn aggregate_gc_correspondence_requires_exact_normalized_source_type(checker: &dyn SourceChecker) {
    let (candidate, family, records, layouts) = aggregate_fixture(checker);
    let expected = match &family.source { OwnerFamilySource::Aggregate(ty) => ty.clone(), _ => unreachable!() };
    let mut context = CorrespondenceContext::with_owners(&candidate);
    assert!(value_type_matches_source(ValueType::Gc(family.owner_type), &expected, &layouts,
        candidate.signatures(), &mut context, &records, candidate.closure_layouts()));
    let list = Type::Named("List".into(), vec![expected.clone()]);
    let mut context = CorrespondenceContext::with_owners(&candidate);
    assert!(value_type_matches_source(ValueType::Gc(family.list_type), &list, &layouts,
        candidate.signatures(), &mut context, &records, candidate.closure_layouts()));
    let mismatch = Type::Named("Int".into(), Vec::new());
    let mut context = CorrespondenceContext::with_owners(&candidate);
    assert!(!value_type_matches_source(ValueType::Gc(family.owner_type), &mismatch, &layouts,
        candidate.signatures(), &mut context, &records, candidate.closure_layouts()));
}
