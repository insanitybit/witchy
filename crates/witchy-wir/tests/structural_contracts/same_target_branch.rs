use std::collections::BTreeMap;

use witchy_wir::canonical_safewir::{
    check_initial, BasicBlock, BlockId, BlockParameter, CallableContract, CanonicalModule,
    CheckedSourceIdentity, ComparisonOperator, Function, FunctionId, FunctionSignature,
    FunctionSignatureId, InitialCheckError, LoweredCandidate, LoweredModuleParts, Operation,
    ParameterContract, ParameterOwnership, Revision, Terminator, ValueId, ValueType,
};
use witchy_wir::layout::{LayoutBundle, LayoutInterner};

// An unknown Boolean chooses which scalar reaches p. Only p==1 may return;
// p!=1 reaches a deliberately forbidden Unreachable terminator.
fn candidate(split_targets: bool, then_value: u64, else_value: u64) -> CanonicalModule<LoweredCandidate> {
    let condition = ValueId::new(0);
    let left = ValueId::new(1);
    let right = ValueId::new(2);
    let phi = ValueId::new(3);
    let one = ValueId::new(4);
    let equal = ValueId::new(5);
    let entry = BlockId::new(0);
    let join = BlockId::new(1);
    let returns = BlockId::new(2);
    let forbidden = BlockId::new(3);
    let then_bridge = BlockId::new(4);
    let else_bridge = BlockId::new(5);
    let mut blocks = vec![
        BasicBlock {
            id: entry,
            parameters: vec![BlockParameter { value: condition, value_type: ValueType::Bool, ownership: None }],
            operations: vec![
                Operation::ConstSize { destination: left, value: then_value },
                Operation::ConstSize { destination: right, value: else_value },
            ],
            terminator: Terminator::BranchIf {
                condition,
                then_target: if split_targets { then_bridge } else { join },
                then_arguments: if split_targets { vec![] } else { vec![left] },
                else_target: if split_targets { else_bridge } else { join },
                else_arguments: if split_targets { vec![] } else { vec![right] },
            },
        },
        BasicBlock {
            id: join,
            parameters: vec![BlockParameter { value: phi, value_type: ValueType::Size, ownership: None }],
            operations: vec![
                Operation::ConstSize { destination: one, value: 1 },
                Operation::Compare { destination: equal, operator: ComparisonOperator::Equal, left: phi, right: one },
            ],
            terminator: Terminator::BranchIf {
                condition: equal, then_target: returns, then_arguments: vec![],
                else_target: forbidden, else_arguments: vec![],
            },
        },
        BasicBlock { id: returns, parameters: vec![], operations: vec![], terminator: Terminator::Return { values: vec![] } },
        BasicBlock { id: forbidden, parameters: vec![], operations: vec![], terminator: Terminator::Unreachable },
    ];
    if split_targets {
        blocks.extend([
            BasicBlock { id: then_bridge, parameters: vec![], operations: vec![], terminator: Terminator::Branch { target: join, arguments: vec![left] } },
            BasicBlock { id: else_bridge, parameters: vec![], operations: vec![], terminator: Terminator::Branch { target: join, arguments: vec![right] } },
        ]);
    }
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest([57; 32]),
        layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).expect("empty layouts"),
        signatures: vec![FunctionSignature {
            id: FunctionSignatureId::new(0), parameters: vec![ValueType::Bool], results: vec![],
            contract: CallableContract {
                parameters: vec![ParameterContract { value_type: ValueType::Bool, ownership: ParameterOwnership::Value }],
                results: vec![], parameter_effects: BTreeMap::new(),
            },
        }],
        imports: vec![],
        functions: vec![Function {
            id: FunctionId::new(0), signature: FunctionSignatureId::new(0), entry,
            value_types: [(condition, ValueType::Bool), (left, ValueType::Size),
                (right, ValueType::Size), (phi, ValueType::Size), (one, ValueType::Size),
                (equal, ValueType::Bool)].into_iter().collect(),
            blocks,
        }],
        proper_tail_sites: vec![],
    })
}

fn expected_refusal() -> InitialCheckError {
    InitialCheckError::ReachableUnreachable { function: FunctionId::new(0), block: BlockId::new(3) }
}

#[test]
fn different_same_target_arguments_must_not_prune_a_reachable_failure() {
    assert_eq!(check_initial(candidate(false, 0, 1)).map(|_| ()), Err(expected_refusal()));
}

#[test]
fn split_target_control_rejects_the_same_reachable_failure() {
    assert_eq!(check_initial(candidate(true, 0, 1)).map(|_| ()), Err(expected_refusal()));
}

#[test]
fn reversed_same_target_control_still_rejects() {
    assert_eq!(check_initial(candidate(false, 1, 0)).map(|_| ()), Err(expected_refusal()));
}

#[test]
fn agreeing_same_target_control_admits() {
    check_initial(candidate(false, 1, 1)).expect("both paths prove p==1");
}

#[test]
fn constant_branch_only_merges_the_executable_same_target_arm() {
    for chosen in [false, true] {
        let module = candidate(false, 0, 1).transform_functions(|functions| {
            let function = &mut functions[0];
            let known = ValueId::new(6);
            function.value_types.insert(known, ValueType::Bool);
            function.blocks[0].operations.push(Operation::Compare {
                destination: known, operator: ComparisonOperator::Equal,
                left: ValueId::new(if chosen { 2 } else { 1 }), right: ValueId::new(2),
            });
            let Terminator::BranchIf { condition, .. } = &mut function.blocks[0].terminator else { unreachable!() };
            *condition = known;
        });
        assert_eq!(check_initial(module).map(|_| ()), if chosen { Err(expected_refusal()) } else { Ok(()) });
    }
}
