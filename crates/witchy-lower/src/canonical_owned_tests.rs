use super::owned::insert_releases;
use std::collections::{BTreeMap, HashMap};
use witchy_wir::canonical_safewir::*;
use witchy_wir::layout::LayoutId;

fn signature(id: u32, parameters: Vec<ValueType>, results: Vec<ValueType>) -> FunctionSignature {
    FunctionSignature {
        id: FunctionSignatureId::new(id),
        contract: CallableContract {
            parameters: parameters.iter().map(|ty| ParameterContract {
                value_type: *ty,
                ownership: if matches!(ty, ValueType::Ref(_)) { ParameterOwnership::Shared } else { ParameterOwnership::Value },
            }).collect(),
            results: results.iter().map(|ty| match ty {
                ValueType::Ref(layout) => ResultContract::Shared { layout: *layout },
                ty => ResultContract::Value(*ty),
            }).collect(),
            parameter_effects: BTreeMap::new(),
        },
        parameters,
        results,
    }
}

fn parameter(value: u32, value_type: ValueType) -> BlockParameter {
    BlockParameter {
        value: ValueId::new(value), value_type,
        ownership: matches!(value_type, ValueType::Ref(_)).then_some(Ownership::Shared),
    }
}

#[test]
fn owned_releases_refuse_join_with_an_unowned_non_reference_lane() {
    let ty = ValueType::Ref(LayoutId::from_bytes([1; 32]));
    let condition = ValueId::new(0);
    let owned = ValueId::new(1);
    let joined = ValueId::new(2);
    let sig = signature(0, vec![ValueType::Bool, ty], vec![ty]);
    let mut function = Function {
        id: FunctionId::new(0), signature: sig.id, entry: BlockId::new(0),
        value_types: BTreeMap::from([
            (condition, ValueType::Bool), (owned, ty), (joined, ty),
        ]),
        blocks: vec![
            BasicBlock {
                id: BlockId::new(0),
                parameters: vec![parameter(0, ValueType::Bool), parameter(1, ty)],
                operations: Vec::new(),
                terminator: Terminator::BranchIf {
                    condition,
                    then_target: BlockId::new(1), then_arguments: Vec::new(),
                    else_target: BlockId::new(2), else_arguments: Vec::new(),
                },
            },
            BasicBlock {
                id: BlockId::new(1), parameters: Vec::new(), operations: Vec::new(),
                terminator: Terminator::Branch { target: BlockId::new(3), arguments: vec![owned] },
            },
            BasicBlock {
                id: BlockId::new(2), parameters: Vec::new(), operations: Vec::new(),
                terminator: Terminator::Branch { target: BlockId::new(3), arguments: vec![condition] },
            },
            BasicBlock {
                id: BlockId::new(3), parameters: vec![parameter(2, ty)], operations: Vec::new(),
                terminator: Terminator::Return { values: vec![joined] },
            },
        ],
    };

    assert!(matches!(
        insert_releases(&mut function, &sig, &[sig.clone()], &HashMap::new()),
        Err(super::owned::ReleasePlacementError::IncompatibleOwnedEdge(target)) if target == BlockId::new(3)
    ));
}
