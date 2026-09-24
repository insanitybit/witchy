use std::collections::BTreeMap;
use witchy_syntax::ast::{Type, TypeDef};
use witchy_canonical_source::aggregates::{
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

// The actual std scheduler types, checked from source, must have finite
// aggregate shapes. These compile witchy source, so they live with the
// checker rather than in the source-contract crate.
mod scheduler_shapes {
    use std::collections::HashSet;
    use witchy_canonical_source::aggregates::{is_v1_field_type, scalar_aggregate_shape, type_definitions};
    use witchy_syntax::ast::Type;
    use witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules;
    use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

    fn owners(application: &str) -> AuthenticatedModuleOwners {
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/stdlib", "0.1.0").unwrap();
        let application = PackageCoordinate::new(PackageSource::Workspace, application, "0.1.0").unwrap();
        let mut owners = vec![("main".into(), ModuleLoadIdentity::new(application, ["main"]).unwrap())];
        owners.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
            ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", name]).unwrap())
        }));
        AuthenticatedModuleOwners::from_loader_assignments(owners).unwrap()
    }

    #[test]
    fn canonical_task_tuple_has_a_bounded_concrete_shape() {
        let source = b"import chan\nasync fn next(n: Int) -> Int:\n    n + 1\nasync fn main(console: Console):\n    let values = chan.par_map([4], fn(n): next(n)).await\n    console.print(\"${values}\")\n";
        let checked = link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("main".into(), source.to_vec()),
                ("task".into(), include_bytes!("../../../std/task.witchy").to_vec()),
                ("chan".into(), include_bytes!("../../../std/chan.witchy").to_vec())],
            "main", |_, _, _| Ok(Default::default()),
            &HashSet::from(["main".into()]),
            owners("test/task-tuple-shape"),
        ).expect("the actual task-tuple source checks");
        let definitions = type_definitions(checked.canonical_source().unwrap().typed_module().module());
        let nil = Type::Named("Nil".into(), vec![]);
        let int = Type::Named("Int".into(), vec![]);
        let tuple = Type::Tuple(vec![
            Type::Named("List".into(), vec![Type::Named("task.Task".into(), vec![nil])]),
            Type::Named("List".into(), vec![Type::Named("chan.Receiver".into(), vec![int])]),
        ]);
        let task = Type::Named("task.Task".into(), vec![tuple.clone()]);
        assert!(is_v1_field_type(&tuple, &definitions), "the concrete tuple argument is supported");
        assert!(is_v1_field_type(&task, &definitions), "finite Task<Tuple> refused within the shape-node budget");
        assert!(scalar_aggregate_shape(&task, &definitions).is_some());
    }

    #[test]
    fn canonical_scheduler_slot_is_a_finite_owned_graph() {
        let checked = link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("main".into(), b"import task\nfn main():\n    task.run(task.done(()))\n".to_vec()),
                ("task".into(), include_bytes!("../../../std/task.witchy").to_vec())],
            "main", |_, _, _| Ok(Default::default()),
            &HashSet::from(["main".into()]),
            owners("test/scheduler-shape"),
        ).expect("actual source scheduler checks and messages are prepared");
        let definitions = type_definitions(checked.canonical_source().unwrap().typed_module().module());
        let slot = Type::Named("task.Slot".into(), vec![]);
        let definition = definitions.get("task.Slot").expect("actual scheduler slot declaration");
        let first_bad_field = definition.variants.iter().flat_map(|variant| &variant.fields)
            .find(|field| !is_v1_field_type(field, &definitions));
        assert!(is_v1_field_type(&slot, &definitions),
            "finite scheduler graph refused; independently unsupported field={first_bad_field:?}");

        let mut invalid = definitions.clone();
        invalid.get_mut("task.Slot").unwrap().variants[0].fields.push(Type::Named("Missing".into(), vec![]));
        assert!(!is_v1_field_type(&slot, &invalid), "an unknown field cannot acquire storage authority");
    }
}
