use std::collections::BTreeMap;
use crate::canonical_safewir::*;
use crate::layout::{LayoutBundle, LayoutInterner, LayoutSize, ScalarKind};

fn candidate(operator: ComparisonOperator) -> CanonicalModule<LoweredCandidate> {
    let mut layouts = LayoutInterner::new();
    let element = layouts.intern_scalar(ScalarKind::U32).unwrap();
    let list = layouts.intern_runtime_buffer(element).unwrap();
    let LayoutSize::Dynamic { base, stride } = layouts.get(list).unwrap().size() else { panic!("dynamic buffer"); };
    let count = ValueId::new(0);
    let capacity = ValueId::new(1);
    let object = ValueId::new(2);
    let high_bit = ValueId::new(3);
    let condition = ValueId::new(4);
    let loaded = ValueId::new(5);
    let function = FunctionId::new(0);
    let signature = FunctionSignatureId::new(0);
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest([0; 32]),
        layouts: LayoutBundle::from_interner(&layouts, [list]).unwrap(),
        signatures: vec![FunctionSignature {
            id: signature, parameters: Vec::new(), results: Vec::new(),
            contract: CallableContract { parameters: Vec::new(), results: Vec::new(), parameter_effects: BTreeMap::new() },
        }],
        imports: Vec::new(), proper_tail_sites: Vec::new(),
        functions: vec![Function {
            id: function, signature, entry: BlockId::new(0),
            value_types: BTreeMap::from([
                (count, ValueType::Size), (capacity, ValueType::Size),
                (object, ValueType::Ref(list)), (high_bit, ValueType::Size),
                (condition, ValueType::Bool), (loaded, ValueType::Size),
            ]),
            blocks: vec![
                BasicBlock {
                    id: BlockId::new(0), parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 0 },
                        Operation::ConstSize { destination: capacity, value: 1 },
                        Operation::Alloc {
                            destination: object, layout: list,
                            extent: Extent::Counted { header_bytes: base, count, capacity, stride_bytes: stride },
                            class: AllocationClass::ReferenceCounted,
                        },
                        Operation::ConstSize { destination: high_bit, value: 1u64 << 31 },
                        Operation::Compare { destination: condition, operator, left: high_bit, right: count },
                    ],
                    terminator: Terminator::BranchIf {
                        condition, then_target: BlockId::new(1), then_arguments: Vec::new(),
                        else_target: BlockId::new(2), else_arguments: Vec::new(),
                    },
                },
                BasicBlock {
                    id: BlockId::new(1), parameters: Vec::new(),
                    operations: vec![
                        // Index zero is outside a zero-length collection,
                        // irrespective of its capacity being one.
                        Operation::Load {
                            destination: loaded,
                            place: Place::projected(object, Projection::Element { index: count }, ValueType::Size, AccessMode::Read),
                            transfer: LoadTransfer::CopyScalar,
                        },
                        Operation::DropRef { object },
                    ],
                    terminator: Terminator::Return { values: Vec::new() },
                },
                BasicBlock {
                    id: BlockId::new(2), parameters: Vec::new(),
                    operations: vec![Operation::DropRef { object }],
                    terminator: Terminator::Return { values: Vec::new() },
                },
            ],
        }],
    })
}

#[test]
fn signed_size_branch_cannot_hide_reachable_out_of_bounds_access() {
    assert!(matches!(
        check_initial(candidate(ComparisonOperator::LessSigned)),
        Err(InitialCheckError::MissingBoundsGuard { .. }),
    ));
}

#[test]
fn unsigned_size_branch_keeps_the_high_bit_positive() {
    check_initial(candidate(ComparisonOperator::LessUnsigned))
        .expect("unsigned high-bit value is not less than zero");
}
