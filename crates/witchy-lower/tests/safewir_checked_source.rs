//! SafeWIR tests that need a real checked source (RFC-0170 P4).
//!
//! witchy-wir cannot depend on witchy-types, not even as a dev-dependency:
//! Cargo applies one to every test target, so each checker edit would rebuild
//! wir's test binaries. The tests below used to live in witchy-wir under the
//! same names. The authority, owner-authority and closure-contract cases judge
//! the checker's product with wir-private predicates, so their bodies stay in
//! wir behind the dev-only `checked-source-test-api` feature and this file
//! supplies the real frontend. The last two modules use only public wir API
//! and moved here whole.

use witchy_wir::canonical_safewir::checked_source_test_api::{
    self as cases, CheckedFixture, SourceChecker,
};

/// The checked frontend, exactly as the tests in witchy-wir used to call it.
struct Frontend;

impl SourceChecker for Frontend {
    fn with_checked(&self, entry: &str, source: &[u8], body: &mut dyn FnMut(CheckedFixture<'_>)) {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![(entry.to_string(), source.to_vec())],
            entry,
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let source = checked.canonical_source().expect("canonical source");
        body(CheckedFixture {
            view: source.view(),
            typed_module: source.typed_module().module(),
            closure_witness: source.closure_witness(),
        });
    }
}

/// Formerly `witchy-wir` `canonical_safewir::authority::tests`.
mod authority {
    use super::*;

    #[test]
    fn checked_source_abi_binding_accepts_the_actual_matching_candidate() {
        cases::authority::checked_source_abi_binding_accepts_the_actual_matching_candidate(&Frontend);
    }

    #[test]
    fn source_own_scalar_rejects_a_unique_candidate_convention() {
        cases::authority::source_own_scalar_rejects_a_unique_candidate_convention(&Frontend);
    }

    #[test]
    fn unique_pop_source_abi_requires_checked_unique_input_and_output() {
        cases::authority::unique_pop_source_abi_requires_checked_unique_input_and_output(&Frontend);
    }

    #[test]
    fn source_closure_family_normalizes_nominals_and_default_let_not_own() {
        cases::authority::source_closure_family_normalizes_nominals_and_default_let_not_own(&Frontend);
    }

    #[test]
    fn option_rooted_int_view_requires_checked_payload_state() {
        cases::authority::option_rooted_int_view_requires_checked_payload_state(&Frontend);
    }

    #[test]
    fn checked_source_abi_binding_rejects_bool_substitution() {
        cases::authority::checked_source_abi_binding_rejects_bool_substitution(&Frontend);
    }

    #[test]
    fn checked_source_abi_binding_rejects_an_extra_result_lane() {
        cases::authority::checked_source_abi_binding_rejects_an_extra_result_lane(&Frontend);
    }

    #[test]
    fn source_dir_and_env_match_their_authenticated_capability_kinds() {
        cases::authority::source_dir_and_env_match_their_authenticated_capability_kinds(&Frontend);
    }

    #[test]
    fn source_console_requires_the_authenticated_capability_kind() {
        cases::authority::source_console_requires_the_authenticated_capability_kind(&Frontend);
    }

    #[test]
    fn source_higher_order_callable_matches_the_actual_nested_signature() {
        cases::authority::source_higher_order_callable_matches_the_actual_nested_signature(&Frontend);
    }

    #[test]
    fn aggregate_callable_field_rejects_a_registered_wrong_signature() {
        cases::authority::aggregate_callable_field_rejects_a_registered_wrong_signature(&Frontend);
    }

    #[test]
    fn source_higher_order_unit_callable_requires_zero_result_lanes() {
        cases::authority::source_higher_order_unit_callable_requires_zero_result_lanes(&Frontend);
    }

    #[test]
    fn checked_source_abi_binding_rejects_missing_and_duplicate_entries() {
        cases::authority::checked_source_abi_binding_rejects_missing_and_duplicate_entries(&Frontend);
    }

    #[test]
    fn final_authority_refuses_a_mismatched_closure_witness() {
        cases::authority::final_authority_refuses_a_mismatched_closure_witness(&Frontend);
    }
}

/// Formerly `witchy-wir` `canonical_safewir::owner_authority::tests`.
mod owner_authority {
    use super::*;

    #[test]
    fn aggregate_owner_source_schema_rejects_type_and_storage_forgery() {
        cases::owner_authority::aggregate_owner_source_schema_rejects_type_and_storage_forgery(&Frontend);
    }

    #[test]
    fn exact_int_slice_record_field_rejects_erased_and_wrong_owner_carriers() {
        cases::owner_authority::exact_int_slice_record_field_rejects_erased_and_wrong_owner_carriers(&Frontend);
    }

    #[test]
    fn aggregate_tuple_owner_binds_exact_fields_and_storage() {
        cases::owner_authority::aggregate_tuple_owner_binds_exact_fields_and_storage(&Frontend);
    }

    #[test]
    fn aggregate_gc_correspondence_requires_exact_normalized_source_type() {
        cases::owner_authority::aggregate_gc_correspondence_requires_exact_normalized_source_type(&Frontend);
    }
}

/// Formerly `witchy-wir` `canonical_safewir::closure_owner_contract::tests`.
mod closure_owner_contract {
    use super::*;

    #[test]
    fn opaque_closure_contract_keeps_caps_and_managed_captures_typed() {
        cases::closure_owner_contract::opaque_closure_contract_keeps_caps_and_managed_captures_typed(&Frontend);
    }

    #[test]
    fn opaque_closure_contract_does_not_enable_pure_capability_capture() {
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), b"fn main(console: Console):\n    let bad = pure fn(msg: String): console.print(msg)\n    bad(\"no\")\n".to_vec())],
            "main", |_, _, _| Ok(Default::default()),
        );
        assert!(checked.is_err(), "pure capture restrictions remain a source obligation");
    }
}

/// Formerly `crates/witchy-wir/tests/interpreter_static_shared.rs`.
mod interpreter_static_shared {
    use std::collections::BTreeMap;

    use witchy_wir::canonical_interpreter::{interpret, InterpreterValue};
    use witchy_wir::canonical_safewir::{
        check_initial_with_witness, validate_final, AccessMode, BasicBlock, CallableContract,
        CandidateAbiBinding, CandidateAbiBindings, CanonicalModule, CheckedSourceIdentity,
        ComparisonOperator, Extent,
        Function, FunctionId, FunctionSignature, FunctionSignatureId, LoweredModuleParts,
        GuardKind, Operation, Place, ProperTailSite, ResultContract, Revision,
        StaticDataId, StaticDataSegment, Terminator, ValueId, ValueType,
    };
    use witchy_wir::layout::{HeaderLayout, LayoutBundle, LayoutInterner, RcHeader, ScalarKind};

    #[test]
    fn authenticated_static_scalar_survives_shared_clone_and_drop() {
        let source = "fn main() -> Int:\n    7\n";
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_string(), source.as_bytes().to_vec())],
            "main",
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        )
        .expect("checked source");
        let checked_source = checked.canonical_source().expect("canonical source");

        let mut interner = LayoutInterner::new();
        let element = interner.intern_scalar(ScalarKind::U32).expect("u32 layout");
        let list = interner
            .intern_packed_list(element, RcHeader::Elided)
            .expect("static list layout");
        let layouts = LayoutBundle::from_interner(&interner, [list]).expect("layout bundle");
        let descriptor = interner.get(list).expect("list descriptor");
        let HeaderLayout::PackedList { length_offset, capacity_offset, data_offset, .. } = descriptor.header() else {
            panic!("packed list header");
        };
        let mut bytes = vec![0; data_offset as usize + 4];
        bytes[length_offset as usize..length_offset as usize + 4].copy_from_slice(&1_u32.to_le_bytes());
        bytes[capacity_offset as usize..capacity_offset as usize + 4].copy_from_slice(&1_u32.to_le_bytes());
        bytes[data_offset as usize..data_offset as usize + 4].copy_from_slice(&7_u32.to_le_bytes());

        let function = FunctionId::new(0);
        let signature = FunctionSignatureId::new(0);
        let object = ValueId::new(0);
        let clone = ValueId::new(1);
        let count = ValueId::new(2);
        let loaded = ValueId::new(3);
        let result = ValueId::new(4);
        let index = ValueId::new(5);
        let in_bounds = ValueId::new(6);
        let candidate = CanonicalModule::from_lowering_with_static_data(
            LoweredModuleParts {
                revision: Revision::new(0),
                checked_source: CheckedSourceIdentity::from_digest(
                    checked_source.source_witness().source_bundle_digest(),
                ),
                layouts,
                signatures: vec![FunctionSignature {
                    id: signature,
                    parameters: Vec::new(),
                    results: vec![ValueType::Int],
                    contract: CallableContract {
                        parameters: Vec::new(),
                        results: vec![ResultContract::Value(ValueType::Int)],
                        parameter_effects: BTreeMap::new(),
                    },
                }],
                imports: Vec::new(),
                functions: vec![Function {
                    id: function,
                    signature,
                    entry: witchy_wir::canonical_safewir::BlockId::new(0),
                    value_types: [
                        (object, ValueType::Ref(list)),
                        (clone, ValueType::Ref(list)),
                        (count, ValueType::Size),
                        (loaded, ValueType::Size),
                        (result, ValueType::Int),
                        (index, ValueType::Size),
                        (in_bounds, ValueType::Bool),
                    ]
                    .into_iter()
                    .collect(),
                    blocks: vec![BasicBlock {
                        id: witchy_wir::canonical_safewir::BlockId::new(0),
                        parameters: Vec::new(),
                        operations: vec![
                            Operation::ConstSize { destination: count, value: 1 },
                            Operation::ConstSize { destination: index, value: 0 },
                            Operation::StaticData {
                                destination: object,
                                layout: list,
                                extent: Extent::Counted {
                                    header_bytes: data_offset,
                                    count,
                                    capacity: count,
                                    stride_bytes: 4,
                                },
                                data: StaticDataId::new(0),
                            },
                            Operation::CloneRef { destination: clone, source: object },
                            Operation::Compare {
                                destination: in_bounds,
                                operator: ComparisonOperator::LessUnsigned,
                                left: index,
                                right: count,
                            },
                            Operation::Guard { condition: in_bounds, kind: GuardKind::Bounds },
                            Operation::Load {
                                destination: loaded,
                                place: Place::projected(
                                    object,
                                    witchy_wir::canonical_safewir::Projection::Element { index },
                                    ValueType::Size,
                                    AccessMode::Read,
                                ),
                                transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
                            },
                            Operation::WidenSize { destination: result, value: loaded },
                            Operation::DropRef { object: clone },
                            Operation::DropRef { object },
                        ],
                        terminator: Terminator::Return { values: vec![result] },
                    }],
                }],
                proper_tail_sites: Vec::<ProperTailSite>::new(),
            },
            vec![StaticDataSegment::new(StaticDataId::new(0), bytes)],
        )
        .with_source_abi_bindings(CandidateAbiBindings::new(vec![CandidateAbiBinding::new(function, 0)]))
        .with_launch_contract_payload(checked_source.launch_contract_payload().to_vec());
        let checked_initial = check_initial_with_witness(candidate, &checked_source.view()).expect("initial authority");
        let validated = validate_final(checked_initial, &checked_source.view()).expect("final authority");

        assert_eq!(interpret(&validated, function, Vec::new()), Ok(vec![InterpreterValue::Int(7)]));
    }
}

/// Formerly `crates/witchy-wir/tests/optimization_binding_roundtrip.rs`.
mod optimization_binding_roundtrip {
    use std::collections::BTreeMap;

    use witchy_wir::canonical_interpreter::{interpret, InterpreterValue};
    use witchy_wir::canonical_safewir::*;
    use witchy_wir::layout::{LayoutBundle, LayoutInterner};

    #[test]
    fn zero_pass_final_validation_preserves_source_binding_and_scalar_behavior() {
        let source = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".to_owned(), b"fn main() -> Int:\n    40 + 2\n".to_vec())],
            "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
        ).unwrap();
        let source = source.canonical_source().unwrap();
        let function = FunctionId::new(0);
        let signature = FunctionSignatureId::new(0);
        let left = ValueId::new(0);
        let right = ValueId::new(1);
        let sum = ValueId::new(2);
        let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest(source.source_witness().source_bundle_digest()),
            layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
            signatures: vec![FunctionSignature {
                id: signature, parameters: vec![], results: vec![ValueType::Int],
                contract: CallableContract {
                    parameters: vec![], results: vec![ResultContract::Value(ValueType::Int)],
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: vec![], proper_tail_sites: vec![],
            functions: vec![Function {
                id: function, signature, entry: BlockId::new(0),
                value_types: BTreeMap::from([(left, ValueType::Int), (right, ValueType::Int), (sum, ValueType::Int)]),
                blocks: vec![
                    BasicBlock {
                        id: BlockId::new(0), parameters: vec![],
                        operations: vec![
                            Operation::ConstInt { destination: left, value: 40 },
                            Operation::ConstInt { destination: right, value: 2 },
                            Operation::Arithmetic {
                                destination: sum, operator: ArithmeticOperator::Add,
                                left, right, overflow: OverflowBehavior::Wrap,
                            },
                        ],
                        terminator: Terminator::Return { values: vec![sum] },
                    },
                    BasicBlock {
                        id: BlockId::new(1), parameters: vec![], operations: vec![],
                        terminator: Terminator::Unreachable,
                    },
                ],
            }],
        }).with_source_abi_bindings(CandidateAbiBindings::new(vec![CandidateAbiBinding::new(function, 0)]))
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());

        let checked = check_initial_with_witness(candidate, &source.view()).unwrap();
        let validated = validate_final(checked, &source.view()).unwrap();
        assert_eq!(interpret(&validated, function, vec![]).unwrap(), vec![InterpreterValue::Int(42)]);
    }
}
