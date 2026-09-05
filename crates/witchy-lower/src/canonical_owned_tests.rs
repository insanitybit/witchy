use super::owned::insert_releases;
use std::collections::{BTreeMap, HashMap};
use witchy_wir::canonical_safewir::*;
use witchy_wir::layout::LayoutId;

#[test]
fn owned_result_source_allocates_returns_indexes_and_executes() {
    let source = "fn make() -> List(Int):\n    [42]\nfn main() -> Int:\n    let xs = make()\n    xs[0]\n";
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".to_string(), source.as_bytes().to_vec())],
        "main",
        |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
    ).expect("checked owned-result source");
    let artifact = crate::canonical_pipeline::admit_checked_module(&checked)
        .expect("owned result must reach independent validation and admission");
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("admitted module");
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instance");
    let main = instance.get_typed_func::<(), i64>(&mut store, "f1").expect("main export");
    assert_eq!(main.call(&mut store, ()).expect("execute owned result"), 42);
}

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
fn owned_releases_drop_dead_caller_token_before_owned_call() {
    let ty = ValueType::Ref(LayoutId::from_bytes([1; 32]));
    let source = ValueId::new(0);
    let clone = ValueId::new(1);
    let sig = signature(0, vec![ty], Vec::new());
    let callee = FunctionId::new(1);
    let mut function = Function {
        id: FunctionId::new(0), signature: sig.id, entry: BlockId::new(0),
        value_types: BTreeMap::from([(source, ty), (clone, ty)]),
        blocks: vec![BasicBlock {
            id: BlockId::new(0), parameters: vec![parameter(0, ty)],
            operations: vec![
                Operation::CloneRef { destination: clone, source },
                Operation::Call { destinations: Vec::new(), callee, arguments: vec![CallArgument::Move(clone)] },
            ],
            terminator: Terminator::Return { values: Vec::new() },
        }],
    };
    insert_releases(&mut function, &sig, &[sig.clone()], &HashMap::from([("callee".into(), (callee, sig.id))])).unwrap();
    assert!(matches!(function.blocks[0].operations.as_slice(), [
        Operation::CloneRef { .. },
        Operation::DropRef { object },
        Operation::Call { .. },
    ] if *object == source));
}

#[test]
fn owned_releases_split_edges_instead_of_freeing_other_branch_result() {
    let ty = ValueType::Ref(LayoutId::from_bytes([1; 32]));
    let x = ValueId::new(0);
    let y = ValueId::new(1);
    let condition = ValueId::new(2);
    let sig = signature(0, vec![ty, ty, ValueType::Bool], vec![ty]);
    let mut function = Function {
        id: FunctionId::new(0), signature: sig.id, entry: BlockId::new(0),
        value_types: BTreeMap::from([(x, ty), (y, ty), (condition, ValueType::Bool)]),
        blocks: vec![
            BasicBlock {
                id: BlockId::new(0), parameters: vec![parameter(0, ty), parameter(1, ty), parameter(2, ValueType::Bool)],
                operations: Vec::new(),
                terminator: Terminator::BranchIf {
                    condition, then_target: BlockId::new(1), then_arguments: Vec::new(),
                    else_target: BlockId::new(2), else_arguments: Vec::new(),
                },
            },
            BasicBlock { id: BlockId::new(1), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::Return { values: vec![x] } },
            BasicBlock { id: BlockId::new(2), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::Return { values: vec![y] } },
        ],
    };
    insert_releases(&mut function, &sig, &[sig.clone()], &HashMap::new()).unwrap();
    assert!(function.blocks[0].operations.is_empty());
    assert_eq!(function.blocks.len(), 5);
    assert!(matches!(function.blocks[3].operations.as_slice(), [Operation::DropRef { object }] if *object == y));
    assert!(matches!(function.blocks[4].operations.as_slice(), [Operation::DropRef { object }] if *object == x));
    assert!(matches!(function.blocks[3].terminator, Terminator::Branch { target, .. } if target == BlockId::new(1)));
    assert!(matches!(function.blocks[4].terminator, Terminator::Branch { target, .. } if target == BlockId::new(2)));
}

#[test]
fn owned_releases_loop_temporary_before_backedge() {
    let layout = LayoutId::from_bytes([1; 32]);
    let object = ValueId::new(1);
    let condition = ValueId::new(0);
    let sig = signature(0, vec![ValueType::Bool], Vec::new());
    let mut function = Function {
        id: FunctionId::new(0), signature: sig.id, entry: BlockId::new(0),
        value_types: BTreeMap::from([(object, ValueType::Ref(layout)), (condition, ValueType::Bool)]),
        blocks: vec![
            BasicBlock {
                id: BlockId::new(0), parameters: vec![parameter(0, ValueType::Bool)], operations: Vec::new(),
                terminator: Terminator::Branch { target: BlockId::new(1), arguments: Vec::new() },
            },
            BasicBlock {
                id: BlockId::new(1), parameters: Vec::new(),
                operations: vec![Operation::Alloc { destination: object, layout, extent: Extent::FixedBytes(4), class: AllocationClass::ReferenceCounted }],
                terminator: Terminator::BranchIf {
                    condition, then_target: BlockId::new(1), then_arguments: Vec::new(),
                    else_target: BlockId::new(2), else_arguments: Vec::new(),
                },
            },
            BasicBlock { id: BlockId::new(2), parameters: Vec::new(), operations: Vec::new(), terminator: Terminator::Return { values: Vec::new() } },
        ],
    };
    insert_releases(&mut function, &sig, &[sig.clone()], &HashMap::new()).unwrap();
    assert!(matches!(function.blocks[1].operations.as_slice(), [
        Operation::Alloc { .. }, Operation::DropRef { object: dropped },
    ] if *dropped == object));
    assert_eq!(function.blocks.len(), 3);
}

#[test]
fn owned_releases_apply_loop_token_permutations_simultaneously() {
    let ty = ValueType::Ref(LayoutId::from_bytes([1; 32]));
    let condition = ValueId::new(0);
    let left = ValueId::new(1);
    let right = ValueId::new(2);
    let loop_left = ValueId::new(3);
    let loop_right = ValueId::new(4);
    let loop_condition = ValueId::new(5);
    let exit_left = ValueId::new(6);
    let exit_right = ValueId::new(7);
    let sig = signature(0, vec![ValueType::Bool, ty, ty], vec![ty, ty]);
    let mut function = Function {
        id: FunctionId::new(0),
        signature: sig.id,
        entry: BlockId::new(0),
        value_types: BTreeMap::from([
            (condition, ValueType::Bool),
            (left, ty),
            (right, ty),
            (loop_left, ty),
            (loop_right, ty),
            (loop_condition, ValueType::Bool),
            (exit_left, ty),
            (exit_right, ty),
        ]),
        blocks: vec![
            BasicBlock {
                id: BlockId::new(0),
                parameters: vec![parameter(0, ValueType::Bool), parameter(1, ty), parameter(2, ty)],
                operations: Vec::new(),
                terminator: Terminator::Branch {
                    target: BlockId::new(1),
                    arguments: vec![left, right, condition],
                },
            },
            BasicBlock {
                id: BlockId::new(1),
                parameters: vec![parameter(3, ty), parameter(4, ty), parameter(5, ValueType::Bool)],
                operations: Vec::new(),
                terminator: Terminator::BranchIf {
                    condition: loop_condition,
                    then_target: BlockId::new(1),
                    then_arguments: vec![loop_right, loop_left, loop_condition],
                    else_target: BlockId::new(2),
                    else_arguments: vec![loop_left, loop_right],
                },
            },
            BasicBlock {
                id: BlockId::new(2),
                parameters: vec![parameter(6, ty), parameter(7, ty)],
                operations: Vec::new(),
                terminator: Terminator::Return { values: vec![exit_left, exit_right] },
            },
        ],
    };

    insert_releases(&mut function, &sig, &[sig.clone()], &HashMap::new())
        .expect("a simultaneous loop permutation preserves both owned tokens");
    assert_eq!(function.blocks.len(), 3);
    assert!(function.blocks.iter().all(|block| block.operations.is_empty()));
}
