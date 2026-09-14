use super::*;
use crate::canonical_safewir::{AccessMode, BasicBlock, BlockId, Place, Projection};
use crate::canonical_safewir::closure_owner_contract::OwnerOperation;

// Saved pre-index predicate. Deliberately scans the original operation stream,
// rather than sharing the index's candidate selection or authentication code.
fn scan(
    function: &Function,
    callee: ValueId,
    signature: FunctionSignatureId,
    visited: &mut BTreeSet<ValueId>,
) -> bool {
    if !visited.insert(callee) {
        return false;
    }
    function.blocks.iter().flat_map(|block| &block.operations).any(|operation| match operation {
        Operation::Load { destination, place, transfer }
            if *destination == callee
                && *transfer == LoadTransfer::CopyScalar
                && place.value_type() == ValueType::Callable(signature) => true,
        Operation::ClosureOwner(OwnerOperation::ReadCallable {
            destination, family, ..
        }) if *destination == callee && *family == signature => true,
        Operation::MoveValue { destination, source } if *destination == callee => {
            scan(function, *source, signature, visited)
        }
        _ => false,
    })
}

fn function(blocks: Vec<Vec<Operation>>) -> Function {
    Function {
        id: FunctionId::new(0),
        signature: FunctionSignatureId::new(0),
        entry: BlockId::new(100),
        value_types: BTreeMap::new(),
        blocks: blocks.into_iter().enumerate().map(|(index, operations)| BasicBlock {
            // Deliberately not sorted: the original scan uses vector order.
            id: BlockId::new(100 - index as u32),
            parameters: Vec::new(),
            operations,
            terminator: Terminator::Return { values: Vec::new() },
        }).collect(),
    }
}

fn moved(destination: u32, source: u32) -> Operation {
    Operation::MoveValue { destination: ValueId::new(destination), source: ValueId::new(source) }
}

fn owner(destination: u32, family: u32) -> Operation {
    Operation::ClosureOwner(OwnerOperation::ReadCallable {
        destination: ValueId::new(destination),
        owner: ValueId::new(99),
        family: FunctionSignatureId::new(family),
    })
}

fn loaded(destination: u32, value_type: ValueType, transfer: LoadTransfer) -> Operation {
    Operation::Load {
        destination: ValueId::new(destination),
        place: Place::projected(
            ValueId::new(99), Projection::Element { index: ValueId::new(98) },
            value_type, AccessMode::Read,
        ),
        transfer,
    }
}

fn compare(
    function: &Function,
    definitions: &CallableDefinitions<'_>,
    callee: u32,
    signature: u32,
    active: &[u32],
) -> (bool, BTreeSet<ValueId>) {
    let mut expected_visited = active.iter().copied().map(ValueId::new).collect();
    let mut actual_visited = active.iter().copied().map(ValueId::new).collect();
    let expected = scan(function, ValueId::new(callee), FunctionSignatureId::new(signature), &mut expected_visited);
    let actual = authenticated_indirect_callee(
        definitions, ValueId::new(callee), FunctionSignatureId::new(signature), &mut actual_visited,
    );
    assert_eq!(actual, expected, "callee={callee}, signature={signature}, active={active:?}");
    assert_eq!(actual_visited, expected_visited, "visited-set/order discrepancy");
    (actual, actual_visited)
}

#[test]
fn callable_definition_index_chains_cycles_and_active_context_match_scan() {
    let mut chain: Vec<_> = (0..32).map(|index| moved(index, index + 1)).collect();
    chain.push(owner(32, 7));
    for operations in [
        chain,
        vec![moved(0, 1), moved(1, 2), moved(2, 0)],
        vec![moved(0, 0)],
        vec![moved(0, 1), moved(1, 2), moved(2, 0), owner(2, 7)],
    ] {
        let function = function(vec![operations]);
        let definitions = CallableDefinitions::new(&function);
        for active in [vec![], vec![0], vec![1], vec![2], vec![32], vec![1, 2]] {
            for signature in [7, 8] {
                compare(&function, &definitions, 0, signature, &active);
            }
        }
    }
}

#[test]
fn callable_definition_index_duplicate_order_and_short_circuit_match_scan() {
    for blocks in [
        vec![vec![moved(0, 1), moved(1, 1)], vec![owner(0, 7)]],
        vec![vec![owner(0, 7)], vec![moved(0, 1), moved(1, 1)]],
        vec![vec![owner(0, 8), moved(0, 1)], vec![owner(1, 7), owner(0, 7)]],
    ] {
        let function = function(blocks);
        let definitions = CallableDefinitions::new(&function);
        for signature in [7, 8, 9] {
            compare(&function, &definitions, 0, signature, &[]);
            compare(&function, &definitions, 0, signature, &[1]);
        }
    }
    let first = function(vec![vec![moved(0, 1), moved(1, 1)], vec![owner(0, 7)]]);
    let (accepted, visited) = compare(&first, &CallableDefinitions::new(&first), 0, 7, &[]);
    assert!(accepted);
    assert_eq!(visited, [ValueId::new(0), ValueId::new(1)].into_iter().collect());
    let first = function(vec![vec![owner(0, 7)], vec![moved(0, 1), moved(1, 1)]]);
    let (accepted, visited) = compare(&first, &CallableDefinitions::new(&first), 0, 7, &[]);
    assert!(accepted);
    assert_eq!(visited, [ValueId::new(0)].into_iter().collect());
}

#[test]
fn callable_definition_index_exact_signature_and_transfer_match_scan() {
    for transfer in [LoadTransfer::CopyScalar, LoadTransfer::CloneShared, LoadTransfer::MoveUnique, LoadTransfer::TakeLast] {
        for value_type in [ValueType::Callable(FunctionSignatureId::new(7)), ValueType::Callable(FunctionSignatureId::new(8)), ValueType::Unit] {
            let function = function(vec![vec![loaded(0, value_type, transfer)]]);
            let definitions = CallableDefinitions::new(&function);
            let (accepted, _) = compare(&function, &definitions, 0, 7, &[]);
            assert_eq!(accepted, transfer == LoadTransfer::CopyScalar && value_type == ValueType::Callable(FunctionSignatureId::new(7)));
            assert!(!compare(&function, &definitions, 1, 7, &[]).0);
        }
    }
    let function = function(vec![vec![owner(0, 8), Operation::ConstUnit { destination: ValueId::new(0) }]]);
    assert!(!compare(&function, &CallableDefinitions::new(&function), 0, 7, &[]).0);
}

#[test]
fn callable_definition_index_exhaustive_small_streams_match_scan() {
    // Include malformed duplicates and cyclic move graphs: indexing must not
    // rely on a prior SSA-validity proof or silently retain only one definition.
    let palette = vec![
        moved(0, 0), moved(0, 1), moved(1, 0), moved(1, 2), moved(2, 1),
        owner(0, 7), owner(1, 7), owner(0, 8),
        loaded(0, ValueType::Callable(FunctionSignatureId::new(7)), LoadTransfer::CopyScalar),
        loaded(1, ValueType::Callable(FunctionSignatureId::new(7)), LoadTransfer::MoveUnique),
        Operation::ConstUnit { destination: ValueId::new(0) },
    ];
    for a in &palette {
        for b in &palette {
            for c in &palette {
                let function = function(vec![vec![a.clone(), b.clone()], vec![c.clone()]]);
                let definitions = CallableDefinitions::new(&function);
                for callee in 0..4 {
                    for signature in [7, 8] {
                        for active in [vec![], vec![0], vec![1], vec![0, 1], vec![2]] {
                            compare(&function, &definitions, callee, signature, &active);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn callable_definition_index_does_not_share_function_or_validation_answers() {
    let accepted = function(vec![vec![owner(0, 7)]]);
    let refused = function(vec![vec![moved(0, 0)]]);
    let accepted_definitions = CallableDefinitions::new(&accepted);
    let refused_definitions = CallableDefinitions::new(&refused);
    // Same function/value/signature IDs; different immutable source graphs.
    for _ in 0..3 {
        assert!(compare(&accepted, &accepted_definitions, 0, 7, &[]).0);
        assert!(!compare(&refused, &refused_definitions, 0, 7, &[]).0);
        assert!(!compare(&accepted, &accepted_definitions, 0, 7, &[0]).0);
    }
}
