use std::collections::BTreeMap;

use witchy_wir::canonical_interpreter::{interpret, InterpreterValue};
use witchy_wir::canonical_safewir::{
    check_initial_with_witness, validate_final, AccessMode, BasicBlock, CallableContract,
    CandidateAbiBinding, CandidateAbiBindings, CanonicalModule, CheckedSourceIdentity,
    ComparisonOperator, Extent,
    Function, FunctionId, FunctionSignature, FunctionSignatureId, LoweredModuleParts,
    GuardKind, Operation, Place, ProperTailSite, ResultContract, Revision,
    StaticDataId, StaticDataSegment, Terminator, ValueId, ValueType,
};
use witchy_wir::layout::{HeaderLayout, LayoutBundle, LayoutInterner, RcHeader, ScalarKind};

#[test]
fn authenticated_static_scalar_survives_shared_clone_and_drop() {
    let source = "fn main() -> Int:\n    7\n";
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".to_string(), source.as_bytes().to_vec())],
        "main",
        |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
    )
    .expect("checked source");
    let checked_source = checked.canonical_source().expect("canonical source");

    let mut interner = LayoutInterner::new();
    let element = interner.intern_scalar(ScalarKind::U32).expect("u32 layout");
    let list = interner
        .intern_packed_list(element, RcHeader::Elided)
        .expect("static list layout");
    let layouts = LayoutBundle::from_interner(&interner, [list]).expect("layout bundle");
    let descriptor = interner.get(list).expect("list descriptor");
    let HeaderLayout::PackedList { length_offset, capacity_offset, data_offset, .. } = descriptor.header() else {
        panic!("packed list header");
    };
    let mut bytes = vec![0; data_offset as usize + 4];
    bytes[length_offset as usize..length_offset as usize + 4].copy_from_slice(&1_u32.to_le_bytes());
    bytes[capacity_offset as usize..capacity_offset as usize + 4].copy_from_slice(&1_u32.to_le_bytes());
    bytes[data_offset as usize..data_offset as usize + 4].copy_from_slice(&7_u32.to_le_bytes());

    let function = FunctionId::new(0);
    let signature = FunctionSignatureId::new(0);
    let object = ValueId::new(0);
    let clone = ValueId::new(1);
    let count = ValueId::new(2);
    let loaded = ValueId::new(3);
    let result = ValueId::new(4);
    let index = ValueId::new(5);
    let in_bounds = ValueId::new(6);
    let candidate = CanonicalModule::from_lowering_with_static_data(
        LoweredModuleParts {
            revision: Revision::new(0),
            checked_source: CheckedSourceIdentity::from_digest(
                checked_source.source_witness().source_bundle_digest(),
            ),
            layouts,
            signatures: vec![FunctionSignature {
                id: signature,
                parameters: Vec::new(),
                results: vec![ValueType::Int],
                contract: CallableContract {
                    parameters: Vec::new(),
                    results: vec![ResultContract::Value(ValueType::Int)],
                    parameter_effects: BTreeMap::new(),
                },
            }],
            imports: Vec::new(),
            functions: vec![Function {
                id: function,
                signature,
                entry: witchy_wir::canonical_safewir::BlockId::new(0),
                value_types: [
                    (object, ValueType::Ref(list)),
                    (clone, ValueType::Ref(list)),
                    (count, ValueType::Size),
                    (loaded, ValueType::Size),
                    (result, ValueType::Int),
                    (index, ValueType::Size),
                    (in_bounds, ValueType::Bool),
                ]
                .into_iter()
                .collect(),
                blocks: vec![BasicBlock {
                    id: witchy_wir::canonical_safewir::BlockId::new(0),
                    parameters: Vec::new(),
                    operations: vec![
                        Operation::ConstSize { destination: count, value: 1 },
                        Operation::ConstSize { destination: index, value: 0 },
                        Operation::StaticData {
                            destination: object,
                            layout: list,
                            extent: Extent::Counted {
                                header_bytes: data_offset,
                                count,
                                capacity: count,
                                stride_bytes: 4,
                            },
                            data: StaticDataId::new(0),
                        },
                        Operation::CloneRef { destination: clone, source: object },
                        Operation::Compare {
                            destination: in_bounds,
                            operator: ComparisonOperator::LessUnsigned,
                            left: index,
                            right: count,
                        },
                        Operation::Guard { condition: in_bounds, kind: GuardKind::Bounds },
                        Operation::Load {
                            destination: loaded,
                            place: Place::projected(
                                object,
                                witchy_wir::canonical_safewir::Projection::Element { index },
                                ValueType::Size,
                                AccessMode::Read,
                            ),
                            transfer: witchy_wir::canonical_safewir::LoadTransfer::CopyScalar,
                        },
                        Operation::WidenSize { destination: result, value: loaded },
                        Operation::DropRef { object: clone },
                        Operation::DropRef { object },
                    ],
                    terminator: Terminator::Return { values: vec![result] },
                }],
            }],
            proper_tail_sites: Vec::<ProperTailSite>::new(),
        },
        vec![StaticDataSegment::new(StaticDataId::new(0), bytes)],
    )
    .with_source_abi_bindings(CandidateAbiBindings::new(vec![CandidateAbiBinding::new(function, 0)]));
    let checked_initial = check_initial_with_witness(candidate, checked_source).expect("initial authority");
    let validated = validate_final(checked_initial, checked_source).expect("final authority");

    assert_eq!(interpret(&validated, function, Vec::new()), Ok(vec![InterpreterValue::Int(7)]));
}
