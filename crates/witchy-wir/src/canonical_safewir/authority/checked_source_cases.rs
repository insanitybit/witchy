//! Authority cases that judge a real checked source (RFC-0170 P4). They
//! moved out of `authority::tests` so witchy-wir's tests need no checker;
//! `crates/witchy-lower/tests/safewir_checked_source.rs` runs each one under
//! the same name. See `canonical_safewir::checked_source_test_api`.

use std::collections::BTreeMap;

use super::*;
use super::authority_test_candidate as candidate;
use crate::canonical_safewir::checked_source_test_api::{CheckedFixture, SourceChecker};
use crate::canonical_safewir::{
    CallableContract, FunctionId, FunctionSignature, FunctionSignatureId, LoweredModuleParts,
    ResultContract, ValueType,
};
use crate::layout::{LayoutBundle, LayoutInterner};
use witchy_canonical_source::source::CanonicalSourceView;

/// `fn main() -> Int: 3`, the source `authority_test_candidate` implements.
fn with_source(checker: &dyn SourceChecker, body: &mut dyn FnMut(CheckedFixture<'_>)) {
    checker.with_checked("main", b"fn main() -> Int:\n    3\n", body);
}

fn candidate_bound_to_source(source: &CanonicalSourceView<'_>) -> CanonicalModule<LoweredCandidate> {
    let mut candidate = candidate();
    candidate.checked_source = CheckedSourceIdentity::from_digest(
        source.source_witness().source_bundle_digest(),
    );
    // A candidate bound to real source carries that source's launch
    // payload; the initial checker compares the two, so a hand-built
    // candidate without it is refused before any other judgment runs.
    let candidate = candidate
        .with_launch_contract_payload(source.launch_contract_payload().to_vec());
    let source_abi = source.closed_callable_abi("main").expect("main ABI");
    let witchy_canonical_source::abi::CanonicalTargetPolicy::ClosedLocal(target) = source_abi.target_policy else {
        panic!("main must have a closed target");
    };
    let binding = super::super::CandidateAbiBinding::new(
        FunctionId::new(0),
        target.function_ordinal,
    );
    candidate.with_source_abi_bindings(super::super::CandidateAbiBindings::new(vec![binding]))
}

pub fn checked_source_abi_binding_accepts_the_actual_matching_candidate(checker: &dyn SourceChecker) {
    with_source(checker, &mut |checked| {
        assert!(check_initial_with_witness(candidate_bound_to_source(&checked.view), &checked.view).is_ok());
    });
}

pub fn source_own_scalar_rejects_a_unique_candidate_convention(checker: &dyn SourceChecker) {
    checker.with_checked("main", b"fn helper(own value: Int) -> Int:\n    value\n\nfn main() -> Int:\n    helper(1)\n", &mut |checked| {
        let source = checked.view.closed_callable_signature(0).unwrap();
        let candidate = FunctionSignature {
            id: FunctionSignatureId::new(0),
            parameters: vec![ValueType::Int],
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![super::super::ParameterContract {
                    value_type: ValueType::Int,
                    ownership: super::super::ParameterOwnership::Unique,
                }],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };
        assert!(!candidate_signature_matches_source(
            &candidate,
            source,
            &CanonicalModule::from_lowering(LoweredModuleParts {
                revision: Revision::new(1),
                checked_source: CheckedSourceIdentity::from_digest([0; 32]),
                layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
                signatures: vec![candidate.clone()],
                imports: Vec::new(),
                functions: Vec::new(),
                proper_tail_sites: Vec::new(),
            }),
        ));
    });
}

pub fn unique_pop_source_abi_requires_checked_unique_input_and_output(checker: &dyn SourceChecker) {
    use witchy_syntax::ast::{Convention, Type, TypeQual};
    use crate::canonical_safewir::ParameterOwnership;
    checker.with_checked("main", b"fn take(var xs: unique List(Int)) -> Int:\n    0\n\nfn main() -> Int:\n    var xs = [1]\n    take(xs)\n", &mut |checked| {
        let source = checked.view.closed_callable_signature(0).unwrap();
        let mut interner = crate::layout::LayoutInterner::new();
        let integer = interner.intern_scalar(crate::layout::ScalarKind::Int).unwrap();
        let list = interner.intern_packed_list(integer, crate::layout::RcHeader::Required).unwrap();
        let signature = FunctionSignature {
            id: FunctionSignatureId::new(0), parameters: vec![ValueType::Ref(list)],
            results: vec![ValueType::Int, ValueType::Ref(list)],
            contract: CallableContract {
                parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(list), ownership: ParameterOwnership::UniqueWriteBack }],
                results: vec![ResultContract::Value(ValueType::Int), ResultContract::Unique { layout: list }],
                parameter_effects: BTreeMap::new(),
            },
        };
        let module = CanonicalModule::from_lowering(LoweredModuleParts {
            revision: Revision::new(1), checked_source: CheckedSourceIdentity::from_digest([0; 32]),
            layouts: LayoutBundle::from_interner(&interner, [list]).unwrap(), signatures: vec![signature.clone()],
            imports: vec![], functions: vec![], proper_tail_sites: vec![],
        });
        assert!(candidate_signature_matches_source(&signature, source, &module), "checked signature: {source:?}");
        let mut weaker = signature.clone();
        weaker.contract.parameters[0].ownership = ParameterOwnership::WriteBack;
        assert!(!candidate_signature_matches_source(&weaker, source, &module));
        weaker = signature.clone();
        weaker.contract.results[1] = ResultContract::Shared { layout: list };
        assert!(!candidate_signature_matches_source(&weaker, source, &module));
        let Type::Fn(parameters, result, _, qualifiers) = source else { panic!("checked callable") };
        let Type::Qualified(TypeQual::Unique, plain) = &parameters[0] else { panic!("checked unique qualifier") };
        let ordinary = Type::Fn(vec![(**plain).clone()], result.clone(), vec![Convention::Var], qualifiers.clone());
        assert!(!candidate_signature_matches_source(&signature, &ordinary, &module), "candidate cannot invent unique authority for plain source var");
    });
}

pub fn source_closure_family_normalizes_nominals_and_default_let_not_own(checker: &dyn SourceChecker) {
    use witchy_syntax::ast::{CallableQualifiers, Convention, Type};
    checker.with_checked("main", b"type Node:\n    value: Int\n\nfn main() -> Int:\n    0\n", &mut |checked| {
        let records = witchy_canonical_source::aggregates::type_definitions(checked.typed_module);
        // Synthetic spellings exercise the helper contract. This does not
        // claim the frontend emits both spellings for one source fixture.
        let callable = |name: &str, conventions| Type::Fn(
            vec![Type::Named(name.into(), vec![])],
            Box::new(Type::Named(name.into(), vec![])), conventions, CallableQualifiers::default());
        let bare = callable("Node", vec![]);
        let qualified = callable("main.Node", vec![Convention::Let]);
        assert_eq!(source_closure_family_key(&bare, &records), source_closure_family_key(&qualified, &records));
        let nested = |inner| Type::Fn(vec![inner], Box::new(Type::Named("Int".into(), vec![])),
            vec![Convention::Own], CallableQualifiers::default());
        assert_eq!(source_closure_family_key(&nested(bare), &records),
            source_closure_family_key(&nested(qualified.clone()), &records));
        assert_ne!(source_closure_family_key(&qualified, &records),
            source_closure_family_key(&callable("Node", vec![Convention::Own]), &records));
        assert_ne!(source_closure_family_key(&nested(qualified), &records),
            source_closure_family_key(&nested(callable("Node", vec![Convention::Own])), &records));
        assert_ne!(source_closure_family_key(&callable("Node", vec![]), &records),
            source_closure_family_key(&callable("Int", vec![]), &records));
    });
}

pub fn option_rooted_int_view_requires_checked_payload_state(checker: &dyn SourceChecker) {
    checker.with_checked("main", b"fn select(present: Bool, value: &Int) -> Option(&Int):\n    if present: Some(value) else: None\n", &mut |checked| {
        let canonical = checked.view;
        let declaration = canonical.closed_callable_declaration(0).expect("Option declaration");
        let access = canonical.access_facts().expect("retained access facts")
            .declaration(&declaration.name).expect("exact declaration access");
        assert!(aggregate_reference_result_relation_is_checked(
            declaration, Some(access), canonical.type_definitions(),
        ));
    });
}

pub fn checked_source_abi_binding_rejects_bool_substitution(checker: &dyn SourceChecker) {
    with_source(checker, &mut |checked| {
        let mut candidate = candidate_bound_to_source(&checked.view);
        candidate.signatures[0].results = vec![ValueType::Bool];
        candidate.signatures[0].contract.results = vec![ResultContract::Value(ValueType::Bool)];
        // Rebuild the complete identity-only binding after the substitution.
        // No candidate-controlled ABI shape remains in it to bless the forgery.
        let source_abi = checked.view.closed_callable_abi("main").unwrap();
        let witchy_canonical_source::abi::CanonicalTargetPolicy::ClosedLocal(target) = source_abi.target_policy else { panic!("closed target") };
        candidate.abi_bindings = super::super::CandidateAbiBindings::new(vec![
            super::super::CandidateAbiBinding::new(FunctionId::new(0), target.function_ordinal),
        ]);
        assert!(matches!(
            check_initial_with_witness(candidate, &checked.view),
            Err(InitialWitnessError::CandidateAbi(CandidateAbiBindingError::CandidateSignatureMismatch { .. }))
        ));
    });
}

pub fn checked_source_abi_binding_rejects_an_extra_result_lane(checker: &dyn SourceChecker) {
    with_source(checker, &mut |checked| {
        let mut candidate = candidate_bound_to_source(&checked.view);
        candidate.signatures[0].results.push(ValueType::Bool);
        candidate.signatures[0].contract.results.push(ResultContract::Value(ValueType::Bool));
        assert!(matches!(
            check_initial_with_witness(candidate, &checked.view),
            Err(InitialWitnessError::CandidateAbi(CandidateAbiBindingError::CandidateSignatureMismatch { .. }))
        ));
    });
}

/// `Dir` matches kind 1 whatever rights it names, and `Env` kind 2. The
/// rights live in the host's authority object, so they change no ABI
/// fact about the token the entry receives.
pub fn source_dir_and_env_match_their_authenticated_capability_kinds(checker: &dyn SourceChecker) {
    checker.with_checked("main", b"fn main(dir: Dir[Read], env: Env):
    let _ = dir.exists(\"a\")
", &mut |checked| {
        let source = checked.view;
        let witchy_syntax::ast::Type::Fn(parameters, _, _, _) = source.closed_callable_signature(0).unwrap().unqualified() else { panic!("function") };
        let matches = |actual, source: &witchy_syntax::ast::Type| {
            value_type_matches_source(actual, source, &LayoutInterner::new(), &[], &mut CorrespondenceContext::default(), &BTreeMap::new(), &BTreeMap::new())
        };
        assert!(matches(ValueType::Capability(super::super::DIR_CAPABILITY_KIND), &parameters[0]));
        assert!(!matches(ValueType::Capability(super::super::CapabilityKindId::new(0)), &parameters[0]));
        assert!(matches(ValueType::Capability(super::super::ENV_CAPABILITY_KIND), &parameters[1]));
        assert!(!matches(ValueType::Capability(super::super::DIR_CAPABILITY_KIND), &parameters[1]));
        // Both are admissible as ordinary `own` parameters too.
        assert!(ordinary_own_source_type_supported(&parameters[0]));
        assert!(ordinary_own_source_type_supported(&parameters[1]));
    });
}

pub fn source_console_requires_the_authenticated_capability_kind(checker: &dyn SourceChecker) {
    checker.with_checked("main", b"fn main(console: Console):\n    console.print(\"ok\")\n", &mut |checked| {
        let source = checked.view;
        let witchy_syntax::ast::Type::Fn(parameters, _, _, _) = source.closed_callable_signature(0).unwrap().unqualified() else { panic!("function") };
        assert!(!value_type_matches_source(ValueType::Capability(super::super::CapabilityKindId::new(1)), &parameters[0], &LayoutInterner::new(), &[], &mut CorrespondenceContext::default(), &BTreeMap::new(), &BTreeMap::new()));
        assert!(value_type_matches_source(ValueType::Capability(super::super::CapabilityKindId::new(0)), &parameters[0], &LayoutInterner::new(), &[], &mut CorrespondenceContext::default(), &BTreeMap::new(), &BTreeMap::new()));
    });
}

/// A closure record of one code slot and no captures.
fn closure_record_layout() -> crate::layout::LayoutId {
    let mut interner = LayoutInterner::new();
    let code = interner.intern_scalar(crate::layout::ScalarKind::Callable).unwrap();
    interner.intern_packed_record(vec![code]).unwrap()
}

pub fn source_higher_order_callable_matches_the_actual_nested_signature(checker: &dyn SourceChecker) {
    checker.with_checked("apply", b"fn apply(f: fn(Int) -> Int) -> Int:\n    f(1)\n", &mut |checked| {
        let source = checked.view.closed_callable_signature(0).unwrap();
        // The `fn(Int) -> Int` parameter is a closure record whose registered
        // signature takes the record first.
        let closure = closure_record_layout();
        let inner = FunctionSignature { id: FunctionSignatureId::new(1), parameters: vec![ValueType::Ref(closure), ValueType::Int], results: vec![ValueType::Int], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(closure), ownership: super::super::ParameterOwnership::Shared }, super::super::ParameterContract { value_type: ValueType::Int, ownership: super::super::ParameterOwnership::Value }], results: vec![ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new() } };
        let outer = FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![ValueType::Ref(closure)], results: vec![ValueType::Int], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(closure), ownership: super::super::ParameterOwnership::Shared }], results: vec![ResultContract::Value(ValueType::Int)], parameter_effects: BTreeMap::new() } };
        let mut candidate = candidate().with_closure_layouts([(closure, FunctionSignatureId::new(1))].into_iter().collect());
        candidate.signatures = vec![outer, inner];
        assert!(candidate_signature_matches_source(&candidate.signatures[0], source, &candidate));
    });
}

pub fn aggregate_callable_field_rejects_a_registered_wrong_signature(checker: &dyn SourceChecker) {
    use crate::layout::{LayoutInterner, PointerSlot, ScalarKind};

    checker.with_checked("apply", b"fn apply(f: fn(Int) -> Int) -> Int:\n    f(1)\n", &mut |checked| {
        let source = checked.view.closed_callable_signature(0).unwrap();
        let witchy_syntax::ast::Type::Fn(parameters, _, _, _) = source.unqualified() else {
            panic!("function")
        };
        let callable_source = parameters[0].clone();

        let mut layouts = LayoutInterner::new();
        let callable = layouts.intern_scalar(ScalarKind::Callable).unwrap();
        let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
        let environment = layouts.intern_packed_record(vec![int]).unwrap();
        let environment_slot = layouts
            .intern_runtime_pointer_value_slot(environment, PointerSlot::Owned)
            .unwrap();
        let closure = layouts
            .intern_packed_record(vec![callable, environment_slot])
            .unwrap();
        let closure_slot = layouts
            .intern_runtime_pointer_value_slot(closure, PointerSlot::Owned)
            .unwrap();
        let aggregate = layouts.intern_packed_record(vec![closure_slot]).unwrap();

        let signature_id = FunctionSignatureId::new(1);
        let wrong_signature = FunctionSignature {
            id: signature_id,
            parameters: vec![ValueType::Ref(closure), ValueType::Bool],
            results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![
                    super::super::ParameterContract {
                        value_type: ValueType::Ref(closure),
                        ownership: super::super::ParameterOwnership::Shared,
                    },
                    super::super::ParameterContract {
                        value_type: ValueType::Bool,
                        ownership: super::super::ParameterOwnership::Value,
                    },
                ],
                results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        };

        assert!(!record_layout_matches_with_closures(
            aggregate,
            &[callable_source],
            false,
            &layouts,
            &BTreeMap::new(),
            &BTreeMap::from([(closure, signature_id)]),
            &[wrong_signature],
            &mut CorrespondenceContext::default(),
        ));
    });
}

pub fn source_higher_order_unit_callable_requires_zero_result_lanes(checker: &dyn SourceChecker) {
    checker.with_checked("apply", b"fn apply(f: fn(Int) -> ()):\n    f(1)\n", &mut |checked| {
        let source = checked.view.closed_callable_signature(0).unwrap();
        let closure = closure_record_layout();
        let inner = FunctionSignature { id: FunctionSignatureId::new(1), parameters: vec![ValueType::Ref(closure), ValueType::Int], results: vec![], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(closure), ownership: super::super::ParameterOwnership::Shared }, super::super::ParameterContract { value_type: ValueType::Int, ownership: super::super::ParameterOwnership::Value }], results: vec![], parameter_effects: BTreeMap::new() } };
        let outer = FunctionSignature { id: FunctionSignatureId::new(0), parameters: vec![ValueType::Ref(closure)], results: vec![], contract: CallableContract { parameters: vec![super::super::ParameterContract { value_type: ValueType::Ref(closure), ownership: super::super::ParameterOwnership::Shared }], results: vec![], parameter_effects: BTreeMap::new() } };
        let mut candidate = candidate().with_closure_layouts([(closure, FunctionSignatureId::new(1))].into_iter().collect());
        candidate.signatures = vec![outer, inner];
        assert!(candidate_signature_matches_source(&candidate.signatures[0], source, &candidate));
    });
}

pub fn checked_source_abi_binding_rejects_missing_and_duplicate_entries(checker: &dyn SourceChecker) {
    with_source(checker, &mut |checked| {
        let source = checked.view;
        let mut missing = candidate_bound_to_source(&source);
        missing.abi_bindings = super::super::CandidateAbiBindings::default();
        assert!(matches!(
            check_initial_with_witness(missing, &source),
            Err(InitialWitnessError::CandidateAbi(CandidateAbiBindingError::MissingBinding { .. }))
        ));

        let candidate = candidate_bound_to_source(&source);
        let binding = candidate.source_abi_bindings().entries()[0].clone();
        let mut duplicate = candidate;
        duplicate.abi_bindings = super::super::CandidateAbiBindings::new(vec![binding.clone(), binding]);
        assert!(matches!(
            check_initial_with_witness(duplicate, &source),
            Err(InitialWitnessError::CandidateAbi(CandidateAbiBindingError::DuplicateSourceDeclaration { .. }))
        ));
    });
}

pub fn final_authority_refuses_a_mismatched_closure_witness(checker: &dyn SourceChecker) {
    with_source(checker, &mut |checked| {
        let source = checked.view;
        let mut checked = check_initial_with_witness(candidate_bound_to_source(&source), &source)
            .expect("initial authority");
        checked.witness.checked_source_witness.closure_witness_digest = [0xa5; 32];

        assert!(matches!(
            validate_final(checked, &source),
            Err(FinalValidationError::SourceWitnessDoesNotMatchInitial)
        ));
    });
}

pub fn final_authority_refuses_a_mismatched_task_behavior_manifest(checker: &dyn SourceChecker) {
    with_source(checker, &mut |checked| {
        let source = checked.view;
        let mut checked = check_initial_with_witness(candidate_bound_to_source(&source), &source)
            .expect("initial authority");
        checked.witness.checked_source_witness.task_behavior_manifest_digest = [0xa5; 32];

        assert!(matches!(
            validate_final(checked, &source),
            Err(FinalValidationError::SourceWitnessDoesNotMatchInitial)
        ));
    });
}
