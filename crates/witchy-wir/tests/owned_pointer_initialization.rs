use std::collections::BTreeMap;

use witchy_wir::canonical_safewir::{
    check_initial, AccessMode, AllocationClass, BasicBlock, BlockId, CallArgument, CallableContract,
    ComparisonOperator, Extent, Function, FunctionId, FunctionSignature, FunctionSignatureId,
    GuardKind, InitialCheckError, LoweredModuleParts, Operation, ParameterContract,
    ParameterOwnership, Place, Projection, ResultContract, ReturnedExtent, Revision, StoreTransfer,
    Terminator, ValueId, ValueType,
};
use witchy_wir::layout::{LayoutBundle, LayoutInterner, LayoutSize, PointerSlot, RcHeader, ScalarKind};

#[derive(Clone, Copy)]
enum Escape {
    Drop,
    Release,
    Reclaim,
    Call,
    Return,
}

fn candidate(
    count: u64,
    initialize: bool,
    escape: Escape,
) -> witchy_wir::canonical_safewir::CanonicalModule<witchy_wir::canonical_safewir::LoweredCandidate> {
    let mut interner = LayoutInterner::new();
    let child_layout = interner.intern_scalar(ScalarKind::Int).expect("child layout");
    let slot = interner
        .intern_runtime_pointer_value_slot(child_layout, PointerSlot::Owned)
        .expect("owned slot");
    let root_layout = interner.intern_packed_list(slot, RcHeader::Required).expect("root layout");
    let LayoutSize::Dynamic { base: header_bytes, stride: stride_bytes } = interner
        .get(root_layout).expect("root descriptor").size() else { panic!("packed root") };
    let root = ValueId::new(0);
    let child = ValueId::new(1);
    let count_value = ValueId::new(2);
    let capacity = ValueId::new(3);
    let index = ValueId::new(4);
    let in_bounds = ValueId::new(5);
    let alias = ValueId::new(6);
    let callee_arg = ValueId::new(7);
    let caller = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let caller_signature = FunctionSignatureId::new(0);
    let callee_signature = FunctionSignatureId::new(1);
    let returns_root = matches!(escape, Escape::Return);
    let mut caller_value_types = [
        (root, ValueType::Ref(root_layout)),
        (count_value, ValueType::Size),
        (capacity, ValueType::Size),
    ].into_iter().collect::<BTreeMap<_, _>>();
    if initialize {
        caller_value_types.extend([
            (child, ValueType::Ref(child_layout)),
            (index, ValueType::Size),
            (in_bounds, ValueType::Bool),
        ]);
    }
    if matches!(escape, Escape::Release) {
        caller_value_types.insert(alias, ValueType::Ref(root_layout));
    }
    let mut operations = vec![
        Operation::ConstSize { destination: count_value, value: count },
        Operation::ConstSize { destination: capacity, value: count },
        Operation::Alloc {
            destination: root,
            layout: root_layout,
            extent: Extent::Counted { header_bytes, count: count_value, capacity, stride_bytes },
            class: AllocationClass::ReferenceCounted,
        },
    ];
    if initialize {
        operations.extend([
            Operation::ConstSize { destination: index, value: 0 },
            Operation::Compare {
                destination: in_bounds,
                operator: ComparisonOperator::LessUnsigned,
                left: index,
                right: capacity,
            },
            Operation::Guard { condition: in_bounds, kind: GuardKind::Bounds },
            Operation::Alloc {
                destination: child,
                layout: child_layout,
                extent: Extent::FixedBytes(8),
                class: AllocationClass::ReferenceCounted,
            },
            Operation::Store {
                place: Place::projected(
                    root,
                    Projection::Element { index },
                    ValueType::Ref(child_layout),
                    AccessMode::Write,
                ),
                value: child,
                transfer: StoreTransfer::MoveUnique,
            },
        ]);
    }
    match escape {
        Escape::Drop => operations.push(Operation::DropRef { object: root }),
        Escape::Release => operations.extend([
            Operation::CloneRef { destination: alias, source: root },
            Operation::Release { object: alias },
            Operation::DropRef { object: root },
        ]),
        Escape::Reclaim => operations.push(Operation::Reclaim { object: root }),
        Escape::Call => operations.push(Operation::Call {
            destinations: Vec::new(),
            callee,
            arguments: vec![CallArgument::Move(root)],
        }),
        Escape::Return => {}
    }
    witchy_wir::canonical_safewir::CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: witchy_wir::canonical_safewir::CheckedSourceIdentity::from_digest([23; 32]),
        layouts: LayoutBundle::from_interner(&interner, [root_layout]).expect("layouts"),
        signatures: vec![
            FunctionSignature {
                id: caller_signature,
                parameters: Vec::new(),
                results: returns_root.then_some(ValueType::Ref(root_layout)).into_iter().collect(),
                contract: CallableContract {
                    parameters: Vec::new(),
                    results: returns_root.then_some(ResultContract::Fresh {
                        layout: root_layout,
                        class: AllocationClass::ReferenceCounted,
                        extent: ReturnedExtent::Counted { header_bytes, stride_bytes },
                    }).into_iter().collect(),
                    parameter_effects: BTreeMap::new(),
                },
            },
            FunctionSignature {
                id: callee_signature,
                parameters: vec![ValueType::Ref(root_layout)],
                results: Vec::new(),
                contract: CallableContract {
                    parameters: vec![ParameterContract {
                        value_type: ValueType::Ref(root_layout),
                        ownership: ParameterOwnership::Unique,
                    }],
                    results: Vec::new(),
                    parameter_effects: BTreeMap::new(),
                },
            },
        ],
        imports: Vec::new(),
        functions: vec![
            Function {
                id: caller,
                signature: caller_signature,
                entry: BlockId::new(0),
                value_types: caller_value_types,
                blocks: vec![BasicBlock {
                    id: BlockId::new(0), parameters: Vec::new(), operations,
                    terminator: Terminator::Return { values: returns_root.then_some(root).into_iter().collect() },
                }],
            },
            Function {
                id: callee,
                signature: callee_signature,
                entry: BlockId::new(1),
                value_types: [(callee_arg, ValueType::Ref(root_layout))].into_iter().collect(),
                blocks: vec![BasicBlock {
                    id: BlockId::new(1),
                    parameters: vec![witchy_wir::canonical_safewir::BlockParameter {
                        value: callee_arg,
                        value_type: ValueType::Ref(root_layout),
                        ownership: Some(witchy_wir::canonical_safewir::Ownership::Unique),
                    }],
                    operations: Vec::new(),
                    terminator: Terminator::Trap,
                }],
            },
        ],
        proper_tail_sites: Vec::new(),
    })
}

fn uninitialized(escape: Escape, operation: &'static str) {
    assert_eq!(
        check_initial(candidate(1, false, escape)).expect_err("uninitialized owned child must not escape"),
        InitialCheckError::UnsupportedMemoryOperation {
            function: FunctionId::new(0),
            operation,
        },
    );
}

#[test]
fn owned_pointer_escape_requires_complete_initialization() {
    check_initial(candidate(0, false, Escape::Drop)).expect("empty owned container");
    check_initial(candidate(1, true, Escape::Drop)).expect("initialized owned container");
    uninitialized(Escape::Drop, "DropRef(uninitialized owned slot)");
    uninitialized(Escape::Release, "Release(uninitialized owned slot)");
    uninitialized(Escape::Reclaim, "Reclaim(uninitialized owned slot)");
    uninitialized(Escape::Call, "Call(uninitialized owned slot)");
    uninitialized(Escape::Return, "Return(uninitialized owned slot)");
}

#[test]
fn fresh_owned_pointer_result_is_complete_at_the_checked_call_boundary() {
    let mut interner = LayoutInterner::new();
    let child_layout = interner.intern_scalar(ScalarKind::Int).expect("child layout");
    let slot = interner.intern_runtime_pointer_value_slot(child_layout, PointerSlot::Owned).expect("owned slot");
    let root_layout = interner.intern_packed_list(slot, RcHeader::Required).expect("root layout");
    let LayoutSize::Dynamic { base: header_bytes, stride: stride_bytes } = interner.get(root_layout).expect("root descriptor").size() else { panic!("packed root") };
    let result = ValueId::new(0);
    let root = ValueId::new(1);
    let child = ValueId::new(2);
    let count = ValueId::new(3);
    let capacity = ValueId::new(4);
    let index = ValueId::new(5);
    let in_bounds = ValueId::new(6);
    let caller = FunctionId::new(0);
    let callee = FunctionId::new(1);
    let caller_signature = FunctionSignatureId::new(0);
    let callee_signature = FunctionSignatureId::new(1);
    let fresh = ResultContract::Fresh {
        layout: root_layout,
        class: AllocationClass::ReferenceCounted,
        extent: ReturnedExtent::Counted { header_bytes, stride_bytes },
    };
    let candidate = witchy_wir::canonical_safewir::CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: witchy_wir::canonical_safewir::CheckedSourceIdentity::from_digest([29; 32]),
        layouts: LayoutBundle::from_interner(&interner, [root_layout]).expect("layouts"),
        signatures: vec![
            FunctionSignature { id: caller_signature, parameters: Vec::new(), results: Vec::new(), contract: CallableContract { parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new() } },
            FunctionSignature { id: callee_signature, parameters: Vec::new(), results: vec![ValueType::Ref(root_layout)], contract: CallableContract { parameters: Vec::new(), results: vec![fresh], parameter_effects: BTreeMap::new() } },
        ],
        imports: Vec::new(),
        functions: vec![
            Function { id: caller, signature: caller_signature, entry: BlockId::new(0), value_types: [(result, ValueType::Ref(root_layout))].into_iter().collect(), blocks: vec![BasicBlock { id: BlockId::new(0), parameters: Vec::new(), operations: vec![Operation::Call { destinations: vec![result], callee, arguments: Vec::new() }, Operation::DropRef { object: result }], terminator: Terminator::Return { values: Vec::new() } }] },
            Function { id: callee, signature: callee_signature, entry: BlockId::new(1), value_types: [(root, ValueType::Ref(root_layout)), (child, ValueType::Ref(child_layout)), (count, ValueType::Size), (capacity, ValueType::Size), (index, ValueType::Size), (in_bounds, ValueType::Bool)].into_iter().collect(), blocks: vec![BasicBlock { id: BlockId::new(1), parameters: Vec::new(), operations: vec![
                Operation::ConstSize { destination: count, value: 1 }, Operation::ConstSize { destination: capacity, value: 1 }, Operation::ConstSize { destination: index, value: 0 },
                Operation::Compare { destination: in_bounds, operator: ComparisonOperator::LessUnsigned, left: index, right: capacity }, Operation::Guard { condition: in_bounds, kind: GuardKind::Bounds },
                Operation::Alloc { destination: root, layout: root_layout, extent: Extent::Counted { header_bytes, count, capacity, stride_bytes }, class: AllocationClass::ReferenceCounted },
                Operation::Alloc { destination: child, layout: child_layout, extent: Extent::FixedBytes(8), class: AllocationClass::ReferenceCounted },
                Operation::Store { place: Place::projected(root, Projection::Element { index }, ValueType::Ref(child_layout), AccessMode::Write), value: child, transfer: StoreTransfer::MoveUnique },
            ], terminator: Terminator::Return { values: vec![root] } }] },
        ],
        proper_tail_sites: Vec::new(),
    });
    check_initial(candidate).expect("checked Fresh pointer result may be dropped by caller");
}
