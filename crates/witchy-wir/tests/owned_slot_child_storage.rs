use std::collections::BTreeMap;

use witchy_wir::canonical_safewir::{
    check_initial, AccessMode, AllocationClass, BasicBlock, CallableContract, ComparisonOperator,
    Extent, Function, FunctionId, FunctionSignature, FunctionSignatureId, GuardKind,
    LoweredModuleParts, Operation, Place, Projection, Revision, StoreTransfer, Terminator,
    InitialCheckError, ValueId, ValueType,
};
use witchy_wir::layout::{LayoutBundle, LayoutInterner, LayoutSize, PointerSlot, RcHeader, ScalarKind};

fn owned_slot_candidate(child_class: AllocationClass) -> witchy_wir::canonical_safewir::CanonicalModule<witchy_wir::canonical_safewir::LoweredCandidate> {
    let mut interner = LayoutInterner::new();
    let child_layout = interner.intern_scalar(ScalarKind::Int).expect("child layout");
    let pointer = interner
        .intern_runtime_pointer_value_slot(child_layout, PointerSlot::Owned)
        .expect("owned slot");
    let root_layout = interner
        .intern_packed_list(pointer, RcHeader::Required)
        .expect("root layout");
    let (header_bytes, stride_bytes) = match interner.get(root_layout).expect("root descriptor").size() {
        LayoutSize::Dynamic { base, stride } => (base, stride),
        LayoutSize::Fixed(_) => panic!("packed list extent"),
    };
    let root = ValueId::new(0);
    let child = ValueId::new(1);
    let count = ValueId::new(2);
    let capacity = ValueId::new(3);
    let index = ValueId::new(4);
    let in_bounds = ValueId::new(5);
    let signature = FunctionSignatureId::new(0);
    let function = FunctionId::new(0);
    witchy_wir::canonical_safewir::CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: witchy_wir::canonical_safewir::CheckedSourceIdentity::from_digest([19; 32]),
        layouts: LayoutBundle::from_interner(&interner, [root_layout]).expect("layout bundle"),
        signatures: vec![FunctionSignature {
            id: signature,
            parameters: Vec::new(),
            results: Vec::new(),
            contract: CallableContract { parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new() },
        }],
        imports: Vec::new(),
        functions: vec![Function {
            id: function,
            signature,
            entry: witchy_wir::canonical_safewir::BlockId::new(0),
            value_types: [
                (root, ValueType::Ref(root_layout)),
                (child, ValueType::Ref(child_layout)),
                (count, ValueType::Size),
                (capacity, ValueType::Size),
                (index, ValueType::Size),
                (in_bounds, ValueType::Bool),
            ].into_iter().collect(),
            blocks: vec![BasicBlock {
                id: witchy_wir::canonical_safewir::BlockId::new(0),
                parameters: Vec::new(),
                operations: vec![
                    Operation::ConstSize { destination: count, value: 1 },
                    Operation::ConstSize { destination: capacity, value: 1 },
                    Operation::ConstSize { destination: index, value: 0 },
                    Operation::Compare {
                        destination: in_bounds,
                        operator: ComparisonOperator::LessUnsigned,
                        left: index,
                        right: capacity,
                    },
                    Operation::Guard { condition: in_bounds, kind: GuardKind::Bounds },
                    Operation::Alloc {
                        destination: root,
                        layout: root_layout,
                        extent: Extent::Counted { header_bytes, count, capacity, stride_bytes },
                        class: AllocationClass::ReferenceCounted,
                    },
                    Operation::Alloc {
                        destination: child,
                        layout: child_layout,
                        extent: Extent::FixedBytes(8),
                        class: child_class,
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
                    Operation::DropRef { object: root },
                ],
                terminator: Terminator::Return { values: Vec::new() },
            }],
        }],
        proper_tail_sites: Vec::new(),
    })
}

#[test]
fn owned_slots_require_managed_child_storage_before_consuming_move() {
    check_initial(owned_slot_candidate(AllocationClass::ReferenceCounted)).expect("RC child candidate");
    let bump = check_initial(owned_slot_candidate(AllocationClass::Bump))
        .expect_err("Bump child must fail at owned-slot store");
    assert_eq!(
        bump,
        InitialCheckError::InvalidAllocationClass {
            function: FunctionId::new(0),
            object: ValueId::new(1),
        }
    );
    let scratch = check_initial(owned_slot_candidate(AllocationClass::Scratch))
        .expect_err("Scratch child must fail before storage");
    assert_eq!(
        scratch,
        InitialCheckError::UnsupportedMemoryOperation {
            function: FunctionId::new(0),
            operation: "Alloc(Scratch)",
        }
    );
}
