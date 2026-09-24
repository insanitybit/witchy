use std::collections::BTreeMap;

use witchy_wir::canonical_interpreter::{interpret, InterpreterValue};
use witchy_wir::canonical_safewir::*;
use witchy_wir::layout::{LayoutBundle, LayoutInterner};

#[test]
fn zero_pass_final_validation_preserves_source_binding_and_scalar_behavior() {
    let source = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("main".to_owned(), b"fn main() -> Int:\n    40 + 2\n".to_vec())],
        "main", |_, _, _| Ok(witchy_syntax::origin::OriginTable::default()),
    ).unwrap();
    let source = source.canonical_source().unwrap();
    let function = FunctionId::new(0);
    let signature = FunctionSignatureId::new(0);
    let left = ValueId::new(0);
    let right = ValueId::new(1);
    let sum = ValueId::new(2);
    let candidate = CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest(source.source_witness().source_bundle_digest()),
        layouts: LayoutBundle::from_interner(&LayoutInterner::new(), []).unwrap(),
        signatures: vec![FunctionSignature {
            id: signature, parameters: vec![], results: vec![ValueType::Int],
            contract: CallableContract {
                parameters: vec![], results: vec![ResultContract::Value(ValueType::Int)],
                parameter_effects: BTreeMap::new(),
            },
        }],
        imports: vec![], proper_tail_sites: vec![],
        functions: vec![Function {
            id: function, signature, entry: BlockId::new(0),
            value_types: BTreeMap::from([(left, ValueType::Int), (right, ValueType::Int), (sum, ValueType::Int)]),
            blocks: vec![
                BasicBlock {
                    id: BlockId::new(0), parameters: vec![],
                    operations: vec![
                        Operation::ConstInt { destination: left, value: 40 },
                        Operation::ConstInt { destination: right, value: 2 },
                        Operation::Arithmetic {
                            destination: sum, operator: ArithmeticOperator::Add,
                            left, right, overflow: OverflowBehavior::Wrap,
                        },
                    ],
                    terminator: Terminator::Return { values: vec![sum] },
                },
                BasicBlock {
                    id: BlockId::new(1), parameters: vec![], operations: vec![],
                    terminator: Terminator::Unreachable,
                },
            ],
        }],
    }).with_source_abi_bindings(CandidateAbiBindings::new(vec![CandidateAbiBinding::new(function, 0)]))
    .with_launch_contract_payload(source.launch_contract_payload().to_vec());

    let checked = check_initial_with_witness(candidate, &source.view()).unwrap();
    let validated = validate_final(checked, &source.view()).unwrap();
    assert_eq!(interpret(&validated, function, vec![]).unwrap(), vec![InterpreterValue::Int(42)]);
}
