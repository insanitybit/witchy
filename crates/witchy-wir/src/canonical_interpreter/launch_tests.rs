use super::*;
use crate::canonical_safewir::{
    check_initial_with_test_witness, validate_final_for_test, BasicBlock, BlockParameter,
    CallableContract, CheckedSourceIdentity, FunctionSignatureId, GcTypeId, LoweredModuleParts,
    MemoryEffect, Ownership, ParameterContract, ParameterOwnership, ResultContract, Revision,
};
use crate::canonical_safewir::closure_owner_contract::{OwnerConstructor, OwnerFamilySource};
use crate::layout::{LayoutBundle, LayoutInterner, ScalarKind};

// Returned roots remain live at the program audit boundary. They let an
// independently validated program exercise nonzero accounting without
// inventing an illegal lost ownership token or seeding private engine state.
fn returning_owner_fixture(list: bool, empty: bool, drop_result: bool) -> ValidatedSafeWir {
    let family_id = FunctionSignatureId::new(0);
    let declaration = FunctionSignatureId::new(1);
    let entry_signature = FunctionSignatureId::new(2);
    let owner_type = ValueType::Gc(GcTypeId::new(10));
    let list_type = ValueType::Gc(GcTypeId::new(11));
    let owner = ValueId::new(0);
    let owner_list = ValueId::new(1);
    let mut layouts = LayoutInterner::new();
    let int = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let environment = layouts.intern_packed_record(vec![int]).unwrap();
    let family = OwnerFamily {
        source: OwnerFamilySource::Closure,
        owner_type: GcTypeId::new(10), list_type: GcTypeId::new(11),
        signature: family_id, declaration_signature: declaration, environment,
        constructors: vec![OwnerConstructor {
            id: ConstructorId(0), function: FunctionId::new(0), witness: None,
            environment_tag: 0, captures: Vec::new(),
        }],
    };
    let mut operations = Vec::new();
    let mut value_types = BTreeMap::new();
    if !empty {
        value_types.insert(owner, owner_type);
        operations.push(Operation::ClosureOwner(OwnerOperation::Construct {
            destination: owner, constructor: ConstructorId(0), captures: Vec::new(),
        }));
    }
    if list {
        value_types.insert(owner_list, list_type);
        operations.push(Operation::ClosureOwner(OwnerOperation::NewList {
            destination: owner_list, family: family_id,
            elements: if empty { Vec::new() } else { vec![owner] },
        }));
        if !empty {
            operations.push(Operation::ClosureOwner(OwnerOperation::Drop { owner, family: family_id }));
        }
    }
    let result_type = if list { list_type } else { owner_type };
    let result = if list { owner_list } else { owner };
    if drop_result {
        operations.push(Operation::ClosureOwner(if list {
            OwnerOperation::DropList { list: result, family: family_id }
        } else { OwnerOperation::Drop { owner: result, family: family_id } }));
    }
    let results = if drop_result { Vec::new() } else { vec![result_type] };
    let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0), checked_source: CheckedSourceIdentity::from_digest([83; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [environment]).unwrap(),
        signatures: vec![
            FunctionSignature {
                id: family_id, parameters: vec![owner_type], results: Vec::new(),
                contract: CallableContract {
                    parameters: vec![ParameterContract { value_type: owner_type, ownership: ParameterOwnership::Shared }],
                    results: Vec::new(),
                    parameter_effects: BTreeMap::from([(0, vec![MemoryEffect::Read, MemoryEffect::Release])]),
                },
            },
            FunctionSignature {
                id: declaration, parameters: Vec::new(), results: Vec::new(),
                contract: CallableContract { parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new() },
            },
            FunctionSignature {
                id: entry_signature, parameters: Vec::new(), results: results.clone(),
                contract: CallableContract { parameters: Vec::new(),
                    results: results.into_iter().map(ResultContract::Value).collect(), parameter_effects: BTreeMap::new() },
            },
        ],
        imports: Vec::new(), proper_tail_sites: Vec::new(),
        functions: vec![
            Function {
                id: FunctionId::new(0), signature: family_id, entry: BlockId::new(0),
                value_types: BTreeMap::from([(owner, owner_type)]),
                blocks: vec![BasicBlock {
                    id: BlockId::new(0),
                    parameters: vec![BlockParameter { value: owner, value_type: owner_type, ownership: Some(Ownership::Shared) }],
                    operations: vec![Operation::ClosureOwner(OwnerOperation::Drop { owner, family: family_id })],
                    terminator: Terminator::Return { values: Vec::new() },
                }],
            },
            Function {
                id: FunctionId::new(1), signature: entry_signature, entry: BlockId::new(0), value_types,
                blocks: vec![BasicBlock { id: BlockId::new(0), parameters: Vec::new(), operations,
                    terminator: Terminator::Return { values: if drop_result { Vec::new() } else { vec![result] } } }],
            },
        ],
    }).with_owner_families(vec![family]);
    validate_final_for_test(check_initial_with_test_witness(candidate).expect("initial owner validation"))
        .expect("final owner validation")
}

#[test]
fn launch_audit_counts_opaque_owners_and_lists_like_program_audit() {
    for (list, empty, drop_result, expected) in [
        (false, false, false, 2), // One owner and its linear environment.
        (true, true, false, 1),   // Empty owner-list storage is still live.
        (true, false, false, 3),  // List, retained owner, and environment.
        (true, false, true, 0),   // Dropping the list releases all three.
    ] {
        let module = returning_owner_fixture(list, empty, drop_result);
        let direct = interpret_program(&module, FunctionId::new(1), Vec::new(), InterpreterConfig::default()).unwrap();
        let launch = interpret_entry_program(&module, FunctionId::new(1), Vec::new(), &[], InterpreterConfig::default()).unwrap();
        assert_eq!(direct.leaked_objects, expected, "independent live-resource expectation");
        assert_eq!(launch.leaked_objects, expected, "launch must audit every storage family");
        assert_eq!(launch, direct, "both launch APIs execute and audit the same program");
    }
}
