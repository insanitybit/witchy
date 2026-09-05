use std::collections::BTreeMap;

use witchy_syntax::ast::{Item, Type, TypeDef};
use witchy_syntax::parser::parse_module;
use witchy_wir::canonical_safewir::{
    check_initial, BasicBlock, BlockId, CallableContract, CanonicalModule,
    CheckedSourceIdentity, Function, FunctionId, FunctionSignature, FunctionSignatureId,
    InitialCheckError, LoweredModuleParts, Operation, ResultContract, Revision, Terminator,
    ValueId, ValueType,
};
use witchy_wir::layout::{
    ClosedTypeResolver, LayoutBundle, LayoutId, LayoutInterner, ResolvedNamed, ScalarKind,
};

struct PackedPointResolver {
    point: TypeDef,
}

impl ClosedTypeResolver for PackedPointResolver {
    fn resolve_named<'a>(&'a self, name: &str, _: &[Type]) -> Option<ResolvedNamed<'a>> {
        match name {
            "Int" => Some(ResolvedNamed::Scalar(ScalarKind::Int)),
            "Bool" => Some(ResolvedNamed::Scalar(ScalarKind::Bool)),
            "Point" => Some(ResolvedNamed::PackedRecord(&self.point)),
            _ => None,
        }
    }
}

fn point_layout() -> (LayoutBundle, LayoutId, LayoutId) {
    let module = parse_module("type Point packed:\n    x: Int\n    y: Int\n")
        .expect("parse packed Point");
    let Item::Type(point) = &module.items[0] else { panic!("Point definition") };
    let resolver = PackedPointResolver { point: point.clone() };
    let mut interner = LayoutInterner::new();
    let layout = interner
        .intern_type(&Type::Named("Point".to_owned(), Vec::new()), &resolver)
        .expect("intern checked packed Point");
    let scalar = interner
        .intern_scalar(ScalarKind::Int)
        .expect("intern scalar Int");
    (
        LayoutBundle::from_interner(&interner, [layout, scalar]).expect("bundle"),
        layout,
        scalar,
    )
}

fn candidate(layouts: LayoutBundle, layout: LayoutId, fields: Vec<ValueId>, index: u32) -> CanonicalModule<witchy_wir::canonical_safewir::LoweredCandidate> {
    let value = ValueId::new(0);
    let aggregate = ValueId::new(1);
    let extracted = ValueId::new(2);
    let boolean = ValueId::new(3);
    CanonicalModule::from_lowering(LoweredModuleParts {
        revision: Revision::new(0),
        checked_source: CheckedSourceIdentity::from_digest([41; 32]),
        layouts,
        signatures: vec![FunctionSignature {
            id: FunctionSignatureId::new(0),
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
            id: FunctionId::new(0),
            signature: FunctionSignatureId::new(0),
            entry: BlockId::new(0),
            value_types: BTreeMap::from([
                (value, ValueType::Int),
                (aggregate, ValueType::InlineAggregate(layout)),
                (extracted, ValueType::Int),
                (boolean, ValueType::Bool),
            ]),
            blocks: vec![BasicBlock {
                id: BlockId::new(0),
                parameters: Vec::new(),
                operations: vec![
                    Operation::ConstInt { destination: value, value: 1 },
                    Operation::ConstBool { destination: boolean, value: true },
                    Operation::MakeInlineAggregate { destination: aggregate, layout, fields },
                    Operation::ExtractInlineField { destination: extracted, aggregate, index },
                ],
                terminator: Terminator::Return { values: vec![extracted] },
            }],
        }],
        proper_tail_sites: Vec::new(),
    })
}

#[test]
fn inline_aggregate_rejects_non_aggregate_catalog_layout() {
    let (layouts, _point, scalar) = point_layout();
    assert!(matches!(
        check_initial(candidate(layouts, scalar, vec![ValueId::new(0)], 0)),
        Err(InitialCheckError::InvalidInlineAggregateLayout { .. })
    ));
}

#[test]
fn inline_aggregate_rejects_field_type_and_index_mismatches() {
    let (layouts, point, _scalar) = point_layout();
    assert!(matches!(
        check_initial(candidate(
            layouts.clone(),
            point,
            vec![ValueId::new(0), ValueId::new(3)],
            0,
        )),
        Err(InitialCheckError::TypeMismatch { .. })
    ));
    assert!(matches!(
        check_initial(candidate(layouts, point, vec![ValueId::new(0), ValueId::new(0)], 2)),
        Err(InitialCheckError::InvalidInlineAggregateLayout { .. })
    ));
}
