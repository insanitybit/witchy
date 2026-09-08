use std::collections::BTreeMap;
use witchy_syntax::ast::{Type, TypeDef};
use witchy_types::canonical_aggregates::{
    concrete_aggregate_identity, scalar_aggregate_shape, ScalarAggregateShape,
};

fn named(name: &str) -> Type { Type::Named(name.into(), Vec::new()) }
fn stack(argument: Type) -> Type { Type::Named("Stack".into(), vec![argument]) }

fn definition() -> TypeDef {
    let mut definition = ScalarAggregateShape {
        name: "Stack".into(),
        fields: vec![named("a"), stack(named("a"))],
        variants: Vec::new(),
        tagged: false,
        packed: false,
    }.packed_view();
    definition.packed = false;
    definition.params.clear();
    definition
}

#[test]
fn concrete_identity_and_shape_use_inferred_generic_parameters() {
    let declaration = definition();
    let definitions = BTreeMap::from([
        ("Stack".into(), declaration.clone()),
        ("main.Stack".into(), declaration),
    ]);
    let int_identity = Type::Named("main.Stack".into(), vec![named("Int")]);
    assert_eq!(concrete_aggregate_identity(&stack(named("Int")), &definitions), Some(int_identity.clone()));
    assert_eq!(concrete_aggregate_identity(&int_identity, &definitions), Some(int_identity.clone()));
    let string_identity = concrete_aggregate_identity(&stack(named("String")), &definitions).unwrap();
    assert_ne!(int_identity, string_identity);
    let shape = scalar_aggregate_shape(&int_identity, &definitions).expect("inferred concrete shape");
    assert_eq!(shape.fields, vec![named("Int"), stack(named("Int"))]);
    assert!(concrete_aggregate_identity(&stack(named("Missing")), &definitions).is_none());
    assert!(scalar_aggregate_shape(&stack(named("Missing")), &definitions).is_none());
}

#[test]
fn ambiguous_basenames_do_not_merge_distinct_checked_declarations() {
    let declaration = definition();
    let definitions = BTreeMap::from([
        ("Stack".into(), declaration.clone()),
        ("left.Stack".into(), declaration.clone()),
        ("right.Stack".into(), declaration),
    ]);
    assert!(concrete_aggregate_identity(&stack(named("Int")), &definitions).is_none());
    let left = Type::Named("left.Stack".into(), vec![named("Int")]);
    let right = Type::Named("right.Stack".into(), vec![named("Int")]);
    assert_eq!(concrete_aggregate_identity(&left, &definitions), Some(left.clone()));
    assert_eq!(concrete_aggregate_identity(&right, &definitions), Some(right.clone()));
    assert_ne!(left, right);
}

#[test]
fn concrete_identity_bounds_nested_argument_work() {
    let mut argument = named("Int");
    for _ in 0..65 { argument = Type::Named("List".into(), vec![argument]); }
    assert!(concrete_aggregate_identity(&argument, &BTreeMap::new()).is_none());
}
