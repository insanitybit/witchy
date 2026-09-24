use super::*;
use crate::canonical_safewir::{
    BlockId, CanonicalModule, CheckedSourceIdentity, ComparisonOperator, FunctionId, GuardKind, LoweredCandidate,
    LoweredModuleParts, Revision, Terminator,
};
use crate::layout::{LayoutBundle, LayoutId};

const ZERO: ValueId = ValueId::new(0);
const LENGTH: ValueId = ValueId::new(1);
const MOVED_COUNT: ValueId = ValueId::new(2);
const SOURCE: ValueId = ValueId::new(3);
const RENAMED: ValueId = ValueId::new(4);
const DESTINATION: ValueId = ValueId::new(5);

fn layouts() -> (LayoutInterner, LayoutId, LayoutId, u32, u32) {
    let mut layouts = LayoutInterner::new();
    let child = layouts.intern_scalar(ScalarKind::Int).unwrap();
    let slot = layouts.intern_runtime_pointer_value_slot(child, PointerSlot::Owned).unwrap();
    let list = layouts.intern_packed_list(slot, RcHeader::Required).unwrap();
    let (header, stride) = match &layouts.get(list).unwrap().size() {
        LayoutSize::Dynamic { base, stride } => (
            u32::try_from(*base).unwrap(),
            u32::try_from(*stride).unwrap(),
        ),
        _ => panic!("a packed list has a counted layout"),
    };
    (layouts, child, list, header, stride)
}

fn signature() -> FunctionSignature {
    FunctionSignature {
        id: FunctionSignatureId::new(0),
        parameters: Vec::new(),
        results: Vec::new(),
        contract: CallableContract {
            parameters: Vec::new(),
            results: Vec::new(),
            parameter_effects: BTreeMap::new(),
        },
    }
}

fn transfer_module(source_length: u64, moved_count: u64) -> CanonicalModule<LoweredCandidate> {
    let (layouts, child, list, header_bytes, stride_bytes) = layouts();
    let mut value_types: BTreeMap<_, _> = [
        (ZERO, ValueType::Size),
        (LENGTH, ValueType::Size),
        (MOVED_COUNT, ValueType::Size),
        (SOURCE, ValueType::Ref(list)),
        (RENAMED, ValueType::Ref(list)),
        (DESTINATION, ValueType::Ref(list)),
    ].into_iter().collect();
    let mut operations = vec![
        Operation::ConstSize { destination: ZERO, value: 0 },
        Operation::ConstSize { destination: LENGTH, value: source_length },
        Operation::ConstSize { destination: MOVED_COUNT, value: moved_count },
        Operation::Alloc {
            destination: SOURCE,
            layout: list,
            extent: Extent::Counted {
                header_bytes, count: LENGTH, capacity: LENGTH, stride_bytes,
            },
            class: AllocationClass::ReferenceCounted,
        },
        Operation::Alloc {
            destination: DESTINATION,
            layout: list,
            extent: Extent::Counted {
                header_bytes, count: ZERO, capacity: LENGTH, stride_bytes,
            },
            class: AllocationClass::ReferenceCounted,
        },
    ];
    for index in 0..source_length {
        let element = ValueId::new(10 + u32::try_from(index).unwrap() * 3);
        let coordinate = ValueId::new(11 + u32::try_from(index).unwrap() * 3);
        let in_bounds = ValueId::new(12 + u32::try_from(index).unwrap() * 3);
        value_types.insert(element, ValueType::Ref(child));
        value_types.insert(coordinate, ValueType::Size);
        value_types.insert(in_bounds, ValueType::Bool);
        operations.extend([
            Operation::Alloc {
                destination: element,
                layout: child,
                extent: Extent::FixedBytes(8),
                class: AllocationClass::ReferenceCounted,
            },
            Operation::ConstSize { destination: coordinate, value: index },
            Operation::Compare {
                destination: in_bounds,
                operator: ComparisonOperator::LessUnsigned,
                left: coordinate,
                right: LENGTH,
            },
            Operation::Guard { condition: in_bounds, kind: GuardKind::Bounds },
            Operation::Store {
                place: Place::projected(
                    SOURCE,
                    Projection::Element { index: coordinate },
                    ValueType::Ref(child),
                    AccessMode::Write,
                ),
                value: element,
                transfer: StoreTransfer::MoveUnique,
            },
        ]);
    }
    operations.extend([
        Operation::MoveValue { destination: RENAMED, source: SOURCE },
        Operation::MoveRange {
            destination: Place::projected(
                DESTINATION,
                Projection::Slice { start: ZERO, length: MOVED_COUNT },
                ValueType::Ref(child),
                AccessMode::Write,
            ),
            source: Place::projected(
                RENAMED,
                Projection::Slice { start: ZERO, length: MOVED_COUNT },
                ValueType::Ref(child),
                AccessMode::Read,
            ),
            count: MOVED_COUNT,
        },
        Operation::UpdateCount { object: RENAMED, count: ZERO },
        Operation::UpdateCount { object: DESTINATION, count: MOVED_COUNT },
        Operation::DropRef { object: RENAMED },
        Operation::DropRef { object: DESTINATION },
    ]);
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest([0x79; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [list]).unwrap(),
        signatures: vec![signature()],
        imports: Vec::new(),
        functions: vec![Function {
            id: FunctionId::new(0),
            signature: FunctionSignatureId::new(0),
            entry: BlockId::new(0),
            value_types,
            blocks: vec![BasicBlock {
                id: BlockId::new(0),
                parameters: Vec::new(),
                operations,
                terminator: Terminator::Return { values: Vec::new() },
            }],
        }],
        proper_tail_sites: Vec::new(),
    })
}

#[test]
fn move_range_zero_after_move_value_cannot_erase_owned_children() {
    let error = super::super::check_initial(transfer_module(1, 0)).unwrap_err();
    assert!(matches!(error, InitialCheckError::UnsupportedMemoryOperation {
        operation: "MoveRange(source-partial)", ..
    }), "{error:?}");
}

#[test]
fn move_range_partial_after_move_value_cannot_erase_unmoved_children() {
    let error = super::super::check_initial(transfer_module(2, 1)).unwrap_err();
    assert!(matches!(error, InitialCheckError::UnsupportedMemoryOperation {
        operation: "MoveRange(source-partial)", ..
    }), "{error:?}");
}

#[test]
fn move_range_completeness_includes_unmaterialized_owned_prefix() {
    // White-box predicate coverage: a summarized Counted value need not have
    // individual slot facts. No claim about a particular ABI transition is
    // needed to preserve the completeness obligation carried by that state.
    let (layouts, _, list, header_bytes, stride_bytes) = layouts();
    let function = Function {
        id: FunctionId::new(0),
        signature: FunctionSignatureId::new(0),
        entry: BlockId::new(0),
        value_types: [(SOURCE, ValueType::Ref(list))].into_iter().collect(),
        blocks: vec![BasicBlock {
            id: BlockId::new(0),
            parameters: Vec::new(),
            operations: Vec::new(),
            terminator: Terminator::Return { values: Vec::new() },
        }],
    };
    let registry = BTreeMap::new();
    let signature = signature();
    let mut checker = MemoryChecker::new(&function, &layouts, &signature, &registry).unwrap();
    checker.state.constants.extend([(ZERO, 0), (LENGTH, 1)]);
    checker.state.objects.insert(SOURCE, ObjectState {
        identity: SOURCE,
        owner_generation: 0,
        layout: list,
        extent: Extent::Counted {
            header_bytes, count: LENGTH, capacity: LENGTH, stride_bytes,
        },
        ownership: Ownership::Unique,
        allocation_class: Some(AllocationClass::ReferenceCounted),
        shared_immutable: false,
        initialized_owned_slots: true,
    });
    let range = CheckedRange {
        object: SOURCE,
        start: ZERO,
        length: ZERO,
        layout: list,
        element: layouts.get(list).unwrap().kind().packed_element_layout().unwrap(),
        identity: SOURCE,
        ownership: Ownership::Unique,
        allocation_class: Some(AllocationClass::ReferenceCounted),
    };
    assert!(checker.require_source_range_complete(range).is_err());
    assert!(checker.require_source_range_complete(CheckedRange {
        length: LENGTH, ..range
    }).is_ok());
}
