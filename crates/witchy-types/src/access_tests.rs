use witchy_syntax::ast::{Block, Convention, Function, Param, Type, TypeQual};

use crate::access::{
    AccessKind, AccessMismatchKind, AccessQualifier, AccessSignature, AccessSignatureError,
    BorrowRelationCatalog, LoanProjection, LoanProjectionStep, OwnershipStateClass,
    SignaturePosition, checked_facts, ownership_state_class,
};

fn named(name: &str) -> Type {
    Type::Named(name.to_string(), Vec::new())
}

fn list(element: Type) -> Type {
    Type::Named("List".to_string(), vec![element])
}

fn qualified(qualifier: TypeQual, ty: Type) -> Type {
    Type::Qualified(qualifier, Box::new(ty))
}

fn signature(
    params: Vec<Type>,
    result: Type,
    conventions: Vec<Convention>,
) -> AccessSignature {
    AccessSignature::from_parts(params, result, conventions).expect("valid access signature")
}

fn layout(children: Vec<Option<OwnershipStateClass>>) -> OwnershipStateClass {
    OwnershipStateClass::LayoutDependent { children }
}

fn catalog_signature(source: &str, function_name: &str) -> AccessSignature {
    let module = witchy_syntax::parser::parse_module(source).expect("parse access fixture");
    let catalog = BorrowRelationCatalog::from_module(&module);
    let function = module
        .items
        .iter()
        .find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function)
                if function.name == function_name =>
            {
                Some(function)
            }
            _ => None,
        })
        .expect("fixture function");
    AccessSignature::from_function_with_catalog(function, &catalog)
        .expect("valid catalog-backed access signature")
}

fn assert_coarse_root_relation(signature: &AccessSignature) {
    let [relation] = signature.borrow_relations() else {
        panic!("one coarse nominal lifetime relation must be preserved")
    };
    assert_eq!(relation.lifetime(), "scope");
    assert_eq!(relation.output_projection(), &LoanProjection::default());
    let [owner] = relation.owners() else { panic!("the input nominal must own the result") };
    assert_eq!(owner.position(), 0);
    assert_eq!(owner.input_projection(), &LoanProjection::default());
}

#[test]
fn public_catalog_free_constructors_preserve_coarse_nominal_lifetime_relations() {
    let lifetime = Type::Named("'scope".into(), Vec::new());
    let parser = Type::Named("Parser".into(), vec![lifetime]);
    let function_type = Type::Fn(
        vec![parser.clone()],
        Box::new(parser.clone()),
        vec![Convention::Let],
    );

    assert_coarse_root_relation(
        &AccessSignature::from_parts(
            vec![parser.clone()],
            parser.clone(),
            vec![Convention::Let],
        )
        .expect("public from_parts keeps a conservative nominal relation"),
    );
    assert_coarse_root_relation(
        &AccessSignature::from_function_type(&function_type)
            .expect("public function-type derivation keeps a conservative nominal relation"),
    );

    let module = witchy_syntax::parser::parse_module(
        "fn parse(value: Parser('scope)) -> Parser('scope):\n    value\n",
    )
    .expect("parse public constructor fixture");
    let function = module
        .items
        .iter()
        .find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) => Some(function),
            _ => None,
        })
        .expect("fixture function");
    assert_coarse_root_relation(
        &AccessSignature::from_function(function)
            .expect("public declaration derivation keeps a conservative nominal relation"),
    );
    assert_coarse_root_relation(
        &AccessSignature::from_resolved_function(function, &function_type)
            .expect("public resolved derivation keeps a conservative nominal relation"),
    );
}

#[test]
fn nominal_borrow_relations_preserve_exact_input_and_output_projections() {
    let signature = catalog_signature(
        "mode opt\n\n\
         type PairView('left, 'right):\n    first: View(String, 'left)\n    second: View(String, 'right)\n\n\
         fn pair(left: let('left) String, right: let('right) String) \
             -> PairView('left, 'right):\n    PairView(left, right)\n",
        "pair",
    );

    let relations = signature.borrow_relations();
    assert_eq!(relations.len(), 2);
    assert_eq!(
        relations[0].output_projection(),
        &LoanProjection { steps: vec![LoanProjectionStep::Field("first".into())] }
    );
    assert_eq!(relations[0].owners()[0].position(), 0);
    assert_eq!(
        relations[0].owners()[0].input_projection(),
        &LoanProjection::default()
    );
    assert_eq!(relations[0].storage_type(), &named("String"));
    assert_eq!(
        relations[1].output_projection(),
        &LoanProjection { steps: vec![LoanProjectionStep::Field("second".into())] }
    );
    assert_eq!(relations[1].owners()[0].position(), 1);
}

#[test]
fn implicit_nominal_type_parameters_preserve_nested_borrow_slots() {
    let signature = catalog_signature(
        "mode opt\n\n\
         type Leaf('scope):\n    value: View(String, 'scope)\n\n\
         type Wrapper('scope):\n    item: x\n\n\
         fn wrap(value: let('scope) String) -> Wrapper('scope, Leaf('scope)):\n    Wrapper(Leaf(value))\n",
        "wrap",
    );

    let [relation] = signature.borrow_relations() else {
        panic!("the implicit generic field must retain its nested borrow")
    };
    assert_eq!(
        relation.output_projection(),
        &LoanProjection {
            steps: vec![
                LoanProjectionStep::Field("item".into()),
                LoanProjectionStep::Field("value".into()),
            ],
        }
    );
    assert_eq!(relation.owners()[0].position(), 0);
}

#[test]
fn finite_nominal_chains_deeper_than_the_old_limit_preserve_relations() {
    let depth = 64;
    let result = format!(
        "{}Leaf('scope){}",
        "Wrapper(".repeat(depth),
        ")".repeat(depth)
    );
    let source = format!(
        "mode opt\n\n\
         type Leaf('scope):\n    value: View(String, 'scope)\n\n\
         type Wrapper(x):\n    item: x\n\n\
         fn deep(value: let('scope) String) -> {result}:\n    value\n"
    );
    let signature = catalog_signature(&source, "deep");

    let [relation] = signature.borrow_relations() else {
        panic!("a valid finite nominal chain must not erase its borrow relation")
    };
    assert_eq!(relation.output_projection().steps.len(), depth + 1);
    assert!(
        relation
            .output_projection()
            .steps
            .iter()
            .take(depth)
            .all(|step| step == &LoanProjectionStep::Field("item".into()))
    );
}

#[test]
fn recursive_nominal_cycles_terminate_without_erasing_direct_borrow_slots() {
    let signature = catalog_signature(
        "mode opt\n\n\
         type Node('scope):\n    next: Node('scope)\n    value: View(String, 'scope)\n\n\
         fn node(value: let('scope) String) -> Node('scope):\n    Node(value, value)\n",
        "node",
    );

    let [relation] = signature.borrow_relations() else {
        panic!("cycle termination must retain the direct borrowed field")
    };
    assert_eq!(
        relation.output_projection(),
        &LoanProjection { steps: vec![LoanProjectionStep::Field("value".into())] }
    );
}

#[test]
fn exact_verifier_rejects_nominal_projection_erasure() {
    let required = catalog_signature(
        "mode opt\n\n\
         type Wrapper('scope):\n    view: View(String, 'scope)\n\n\
         fn wrap(value: let('scope) String) -> Wrapper('scope):\n    Wrapper(value)\n",
        "wrap",
    );
    let erased = catalog_signature(
        "mode opt\n\n\
         type Wrapper('scope):\n    value: String\n\n\
         fn wrap(value: let('scope) String) -> Wrapper('scope):\n    Wrapper(value)\n",
        "wrap",
    );

    let error = required.verify_exact(&erased).expect_err("projection erasure must fail");
    assert_eq!(error.kind(), AccessMismatchKind::BorrowRelation);
}

#[test]
fn exact_verifier_rejects_swapped_generic_nominal_projections() {
    let required = catalog_signature(
        "mode opt\n\n\
         type Leaf(a, 'scope):\n    value: View(a, 'scope)\n\n\
         type Pair(a, b):\n    first: a\n    second: b\n\n\
         fn pair(left: let('left) String, right: let('right) String) \
             -> Pair(Leaf(String, 'left), Leaf(String, 'right)):\n    Pair(Leaf(left), Leaf(right))\n",
        "pair",
    );
    let swapped = catalog_signature(
        "mode opt\n\n\
         type Leaf(a, 'scope):\n    value: View(a, 'scope)\n\n\
         type Pair(a, b):\n    first: b\n    second: a\n\n\
         fn pair(left: let('left) String, right: let('right) String) \
             -> Pair(Leaf(String, 'left), Leaf(String, 'right)):\n    Pair(Leaf(left), Leaf(right))\n",
        "pair",
    );

    let error = required
        .verify_exact(&swapped)
        .expect_err("generic field substitution must not hide a projection swap");
    assert_eq!(error.kind(), AccessMismatchKind::BorrowRelation);
}

#[test]
fn every_parameter_convention_has_a_distinct_access_and_state_flow() {
    let sig = signature(
        vec![list(named("Int")); 4],
        named("Nil"),
        vec![
            Convention::Let,
            Convention::Borrow,
            Convention::Var,
            Convention::Own,
        ],
    );

    assert_eq!(sig.params()[0].kind(), AccessKind::OwnedImmutable);
    assert_eq!(sig.params()[1].kind(), AccessKind::SharedBorrow);
    assert_eq!(sig.params()[2].kind(), AccessKind::ExclusiveWriteback);
    assert_eq!(sig.params()[3].kind(), AccessKind::Consuming);
    assert_eq!(sig.params()[0].ownership().input(), None);
    assert_eq!(sig.params()[1].ownership().input(), None);
    assert_eq!(
        sig.params()[2].ownership().input(),
        Some(&layout(vec![None]))
    );
    assert_eq!(
        sig.params()[2].ownership().writeback(),
        Some(&layout(vec![None]))
    );
    assert_eq!(
        sig.params()[3].ownership().input(),
        Some(&layout(vec![None]))
    );
    assert_eq!(sig.params()[3].ownership().writeback(), None);
}

#[test]
fn qualifiers_are_preserved_and_drive_ownership_requirements() {
    let unique = qualified(TypeQual::Unique, list(named("Int")));
    let local = qualified(TypeQual::LocalUnique, list(named("Int")));
    let frozen = qualified(TypeQual::Frozen, list(named("Int")));
    let sig = signature(
        vec![unique, local, frozen],
        named("Nil"),
        vec![Convention::Let; 3],
    );

    assert_eq!(sig.params()[0].qualifiers(), &[AccessQualifier::Unique]);
    assert_eq!(
        sig.params()[1].qualifiers(),
        &[AccessQualifier::LocalUnique]
    );
    assert_eq!(sig.params()[2].qualifiers(), &[AccessQualifier::Frozen]);
    assert_eq!(
        sig.params()[0].ownership().input(),
        Some(&layout(vec![None]))
    );
    assert_eq!(
        sig.params()[1].ownership().input(),
        Some(&layout(vec![None]))
    );
    assert_eq!(sig.params()[2].ownership().input(), None);
}

#[test]
fn unique_results_return_representation_classed_state() {
    let sig = signature(
        Vec::new(),
        qualified(TypeQual::Unique, list(named("Int"))),
        Vec::new(),
    );
    assert_eq!(sig.result().qualifiers(), &[AccessQualifier::Unique]);
    assert_eq!(
        sig.result().ownership_output(),
        Some(&layout(vec![None]))
    );

    let scalar = signature(
        Vec::new(),
        qualified(TypeQual::Unique, named("Int")),
        Vec::new(),
    );
    assert_eq!(scalar.result().ownership_output(), None);
}

#[test]
fn nested_local_unique_results_are_rejected_recursively() {
    let result = Type::Tuple(vec![
        qualified(TypeQual::LocalUnique, list(named("Int"))),
        named("Int"),
    ]);
    assert_eq!(
        AccessSignature::from_parts(Vec::new(), result, Vec::new()),
        Err(AccessSignatureError::LocalUniqueResult)
    );
}

#[test]
fn borrowed_result_relates_to_owner_parameter_positions() {
    let sig = signature(
        vec![
            qualified(TypeQual::Borrow("a".to_string()), named("String")),
            qualified(TypeQual::Borrow("b".to_string()), named("Bytes")),
            qualified(TypeQual::Borrow("a".to_string()), named("String")),
        ],
        qualified(TypeQual::Borrow("a".to_string()), named("String")),
        vec![Convention::Borrow, Convention::Borrow, Convention::Let],
    );

    assert_eq!(sig.borrow_relations().len(), 1);
    assert_eq!(sig.borrow_relations()[0].lifetime(), "a");
    assert_eq!(
        sig.borrow_relations()[0]
            .owners()
            .iter()
            .map(|owner| owner.position())
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert_eq!(
        sig.result().ownership_output(),
        Some(&OwnershipStateClass::BorrowedOwnerRoot {
            lifetime: "a".to_string()
        })
    );
}

#[test]
fn representation_classification_is_structural_not_container_specific() {
    assert_eq!(ownership_state_class(&named("Int")).unwrap(), None);
    assert_eq!(
        ownership_state_class(&named("String")).unwrap(),
        Some(OwnershipStateClass::LinearMemoryObject)
    );
    assert_eq!(
        ownership_state_class(&named("File")).unwrap(),
        Some(OwnershipStateClass::GcReference)
    );
    assert_eq!(ownership_state_class(&named("Console")).unwrap(), None);
    assert_eq!(
        ownership_state_class(&list(named("Int"))).unwrap(),
        Some(layout(vec![None]))
    );
    assert_eq!(
        ownership_state_class(&Type::Tuple(vec![named("Int"), named("String")])).unwrap(),
        Some(layout(vec![
            None,
            Some(OwnershipStateClass::LinearMemoryObject),
        ]))
    );
}

#[test]
fn function_type_derivation_normalizes_legacy_empty_conventions() {
    let ty = Type::Fn(
        vec![named("Int"), named("String")],
        Box::new(named("Bool")),
        Vec::new(),
    );
    let sig = AccessSignature::from_function_type(&ty).unwrap();
    assert!(
        sig.params()
            .iter()
            .all(|param| param.kind() == AccessKind::OwnedImmutable)
    );
}

#[test]
fn from_function_requires_a_finalized_result_type() {
    let function = Function {
        public: false,
        comptime_only: false,
        attributes: Vec::new(),
        name: "inferred".to_string(),
        params: vec![Param {
            name: "value".to_string(),
            ty: Some(named("Int")),
            convention: Convention::Let,
            default: None,
        }],
        ret: None,
        body: Block { stmts: Vec::new(), lines: Vec::new(), region: None },
        bounds: Vec::new(),
        is_gen: false,
        is_async: false,
    };

    assert_eq!(
        AccessSignature::from_function(&function),
        Err(AccessSignatureError::MissingResultType)
    );
}

#[test]
fn nested_qualifiers_drive_parameter_and_result_state_flow() {
    let borrowed = qualified(TypeQual::Borrow("a".to_string()), named("String"));
    let unique = qualified(TypeQual::Unique, list(named("Int")));
    let sig = signature(
        vec![Type::Tuple(vec![borrowed.clone(), named("Int")])],
        Type::Tuple(vec![borrowed, unique]),
        vec![Convention::Let],
    );

    let expected_input = layout(vec![
        Some(OwnershipStateClass::BorrowedOwnerRoot {
            lifetime: "a".to_string(),
        }),
        None,
    ]);
    assert_eq!(sig.params()[0].ownership().input(), Some(&expected_input));
    assert_eq!(
        sig.result().ownership_output(),
        Some(&layout(vec![
            Some(OwnershipStateClass::BorrowedOwnerRoot {
                lifetime: "a".to_string(),
            }),
            Some(layout(vec![None])),
        ]))
    );
    assert_eq!(
        sig.borrow_relations()[0]
            .owners()
            .iter()
            .map(|owner| owner.position())
            .collect::<Vec<_>>(),
        vec![0]
    );
}

#[test]
fn scalar_only_tuples_remain_layout_dependent() {
    assert_eq!(
        ownership_state_class(&Type::Tuple(vec![named("Int"), named("Bool")])).unwrap(),
        Some(layout(vec![None, None]))
    );
    assert_eq!(ownership_state_class(&Type::Tuple(Vec::new())).unwrap(), None);
}

#[test]
fn qualified_function_types_preserve_outer_callable_qualifiers() {
    let plain = Type::Fn(
        vec![named("Int")],
        Box::new(named("Bool")),
        vec![Convention::Let],
    );
    let qualified_type = qualified(TypeQual::Unique, plain.clone());
    let qualified_sig = AccessSignature::from_function_type(&qualified_type).unwrap();
    let plain_sig = AccessSignature::from_function_type(&plain).unwrap();

    assert_eq!(
        qualified_sig.callable_qualifiers(),
        &[AccessQualifier::Unique]
    );
    let error = qualified_sig.verify_exact(&plain_sig).unwrap_err();
    assert_eq!(error.position(), None);
    assert_eq!(error.kind(), AccessMismatchKind::Qualifier);
}

#[test]
fn exact_verifier_accepts_alpha_renamed_borrow_relations() {
    let required = signature(
        vec![qualified(TypeQual::Borrow("a".to_string()), named("String"))],
        qualified(TypeQual::Borrow("a".to_string()), named("String")),
        vec![Convention::Borrow],
    );
    let renamed = signature(
        vec![qualified(TypeQual::Borrow("owner".to_string()), named("String"))],
        qualified(TypeQual::Borrow("owner".to_string()), named("String")),
        vec![Convention::Borrow],
    );

    required.verify_exact(&renamed).expect("lifetime names alpha-rename");
    renamed.verify_exact(&required).expect("compatibility is symmetric");
}

#[test]
fn exact_verifier_alpha_renames_nominal_lifetime_markers() {
    let holder = |owner: &str| {
        Type::Named(
            "Holder".to_string(),
            vec![named(&format!("'{owner}"))],
        )
    };
    let required = signature(
        vec![
            qualified(TypeQual::Borrow("a".to_string()), named("String")),
            qualified(TypeQual::Borrow("a".to_string()), holder("a")),
        ],
        named("Int"),
        vec![Convention::Borrow; 2],
    );
    let renamed = signature(
        vec![
            qualified(TypeQual::Borrow("owner".to_string()), named("String")),
            qualified(TypeQual::Borrow("owner".to_string()), holder("owner")),
        ],
        named("Int"),
        vec![Convention::Borrow; 2],
    );

    required
        .verify_exact(&renamed)
        .expect("nominal lifetime arguments follow the established alpha mapping");
    renamed
        .verify_exact(&required)
        .expect("nominal lifetime alpha-equivalence is symmetric");

    let pair = |left: &str, right: &str| {
        Type::Named(
            "Pair".to_string(),
            vec![named(&format!("'{left}")), named(&format!("'{right}"))],
        )
    };
    let relation = signature(
        vec![
            qualified(TypeQual::Borrow("left".to_string()), named("String")),
            qualified(TypeQual::Borrow("right".to_string()), named("String")),
            pair("left", "left"),
        ],
        named("Int"),
        vec![Convention::Borrow, Convention::Borrow, Convention::Let],
    );
    let rewired = signature(
        vec![
            qualified(TypeQual::Borrow("x".to_string()), named("String")),
            qualified(TypeQual::Borrow("y".to_string()), named("String")),
            pair("x", "y"),
        ],
        named("Int"),
        vec![Convention::Borrow, Convention::Borrow, Convention::Let],
    );
    let error = relation.verify_exact(&rewired).unwrap_err();
    assert_eq!(error.position(), Some(SignaturePosition::Parameter(2)));
    assert_eq!(error.kind(), AccessMismatchKind::BorrowRelation);

    let concrete = signature(
        vec![Type::Named("Box".to_string(), vec![named("Int")])],
        named("Int"),
        vec![Convention::Let],
    );
    let different = signature(
        vec![Type::Named("Box".to_string(), vec![named("String")])],
        named("Int"),
        vec![Convention::Let],
    );
    let error = concrete.verify_exact(&different).unwrap_err();
    assert_eq!(error.position(), Some(SignaturePosition::Parameter(0)));
    assert_eq!(error.kind(), AccessMismatchKind::TypeShape);
}

#[test]
fn nested_function_lifetimes_stay_in_the_nested_signature_scope() {
    let callback = Type::Fn(
        vec![qualified(TypeQual::Borrow("a".to_string()), named("String"))],
        Box::new(qualified(
            TypeQual::Borrow("a".to_string()),
            named("String"),
        )),
        vec![Convention::Borrow],
    );
    let renamed_callback = Type::Fn(
        vec![qualified(TypeQual::Borrow("inner".to_string()), named("String"))],
        Box::new(qualified(
            TypeQual::Borrow("inner".to_string()),
            named("String"),
        )),
        vec![Convention::Borrow],
    );
    let required = signature(vec![callback.clone()], callback, vec![Convention::Let]);
    let renamed = signature(
        vec![renamed_callback.clone()],
        renamed_callback,
        vec![Convention::Let],
    );

    assert!(required.borrow_relations().is_empty());
    assert!(required.params()[0].borrow_lifetimes().is_empty());
    required
        .verify_exact(&renamed)
        .expect("nested lifetime alpha-renaming is scoped to the nested function");
}

#[test]
fn exact_verifier_rejects_convention_erasure() {
    let required = signature(
        vec![list(named("Int"))],
        named("Nil"),
        vec![Convention::Var],
    );
    let erased = signature(
        vec![list(named("Int"))],
        named("Nil"),
        vec![Convention::Let],
    );
    let error = required.verify_exact(&erased).unwrap_err();
    assert_eq!(error.position(), Some(SignaturePosition::Parameter(0)));
    assert_eq!(error.kind(), AccessMismatchKind::AccessKind);
}

#[test]
fn exact_verifier_rejects_unique_and_frozen_erasure() {
    for qualifier in [TypeQual::Unique, TypeQual::Frozen, TypeQual::LocalUnique] {
        let required = signature(
            vec![qualified(qualifier, list(named("Int")))],
            named("Nil"),
            vec![Convention::Let],
        );
        let erased = signature(
            vec![list(named("Int"))],
            named("Nil"),
            vec![Convention::Let],
        );
        let error = required.verify_exact(&erased).unwrap_err();
        assert_eq!(error.kind(), AccessMismatchKind::Qualifier);
    }
}

#[test]
fn exact_verifier_rejects_result_owner_rewiring() {
    let required = signature(
        vec![
            qualified(TypeQual::Borrow("a".to_string()), named("String")),
            qualified(TypeQual::Borrow("b".to_string()), named("String")),
        ],
        qualified(TypeQual::Borrow("a".to_string()), named("String")),
        vec![Convention::Borrow, Convention::Borrow],
    );
    let rewired = signature(
        vec![
            qualified(TypeQual::Borrow("x".to_string()), named("String")),
            qualified(TypeQual::Borrow("y".to_string()), named("String")),
        ],
        qualified(TypeQual::Borrow("y".to_string()), named("String")),
        vec![Convention::Borrow, Convention::Borrow],
    );
    let error = required.verify_exact(&rewired).unwrap_err();
    assert_eq!(error.position(), Some(SignaturePosition::Result));
    assert_eq!(error.kind(), AccessMismatchKind::BorrowRelation);
}

#[test]
fn exact_verifier_rejects_unique_result_erasure() {
    let required = signature(
        Vec::new(),
        qualified(TypeQual::Unique, list(named("Int"))),
        Vec::new(),
    );
    let erased = signature(Vec::new(), list(named("Int")), Vec::new());
    let error = required.verify_exact(&erased).unwrap_err();
    assert_eq!(error.position(), Some(SignaturePosition::Result));
    assert_eq!(error.kind(), AccessMismatchKind::Qualifier);
}

#[test]
fn malformed_checked_contracts_fail_loudly() {
    assert_eq!(
        AccessSignature::from_parts(
            vec![named("Int")],
            named("Nil"),
            vec![Convention::Let, Convention::Own]
        ),
        Err(AccessSignatureError::ConventionArity {
            params: 1,
            conventions: 2
        })
    );
    assert_eq!(
        AccessSignature::from_parts(
            vec![qualified(TypeQual::Frozen, list(named("Int")))],
            named("Nil"),
            vec![Convention::Var]
        ),
        Err(AccessSignatureError::FrozenMutableParameter { position: 0 })
    );
    assert_eq!(
        AccessSignature::from_parts(
            Vec::new(),
            qualified(TypeQual::LocalUnique, list(named("Int"))),
            Vec::new()
        ),
        Err(AccessSignatureError::LocalUniqueResult)
    );
    assert_eq!(
        AccessSignature::from_parts(
            Vec::new(),
            qualified(TypeQual::Borrow("a".to_string()), named("String")),
            Vec::new()
        ),
        Err(AccessSignatureError::UnboundResultLifetime {
            lifetime: "a".to_string()
        })
    );
}

#[test]
fn checked_query_exposes_declaration_and_exact_call_access_identities() {
    let module = witchy_syntax::parser::parse_module(
        "fn consume(own xs: unique List(Int)) -> unique List(Int):\n    xs\n\n\
         fn caller() -> unique List(Int):\n    consume([1])\n\n\
         fn main():\n    return\n",
    )
    .expect("parse access query fixture");
    let typed = crate::typeck::annotate_checked(module).expect("annotate access query fixture");
    let facts = checked_facts(typed.module(), typed.table()).expect("build checked access facts");
    let consume = facts.declaration("consume").expect("declaration access identity");
    assert_eq!(consume.params()[0].kind(), AccessKind::Consuming);
    assert!(consume.params()[0].ownership().input().is_some());
    assert!(consume.result().ownership_output().is_some());

    let call = typed
        .module()
        .items
        .iter()
        .find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if function.name == "caller" => {
                match function.body.stmts.last() {
                    Some(witchy_syntax::ast::Stmt::Expr(expression)) => Some(expression),
                    _ => None,
                }
            }
            _ => None,
        })
        .expect("caller call expression");
    assert_eq!(facts.call_at(typed.module(), call), Some(consume));
}

#[test]
fn checked_query_is_tied_to_its_exact_typed_module() {
    fn caller_expression(module: &witchy_syntax::ast::Module) -> &witchy_syntax::ast::Expr {
        module
            .items
            .iter()
            .find_map(|item| match item {
                witchy_syntax::ast::Item::Function(function) if function.name == "caller" => {
                    match function.body.stmts.last() {
                        Some(witchy_syntax::ast::Stmt::Expr(expression)) => Some(expression),
                        _ => None,
                    }
                }
                _ => None,
            })
            .expect("caller expression")
    }

    let source = "fn id(x: Int) -> Int:\n    x\n\n\
                  fn caller() -> Int:\n    id(1)\n\n\
                  fn main():\n    return\n";
    let first = witchy_syntax::parser::parse_module(source).expect("parse first module");
    let first = crate::typeck::annotate_checked(first).expect("annotate first module");
    let facts = checked_facts(first.module(), first.table()).expect("build first facts");
    let first_call = caller_expression(first.module());
    assert!(facts.call_at(first.module(), first_call).is_some());

    let second = witchy_syntax::parser::parse_module(source).expect("parse second module");
    let second = crate::typeck::annotate_checked(second).expect("annotate second module");
    let second_call = caller_expression(second.module());
    assert!(
        facts.call_at(second.module(), second_call).is_none(),
        "an address-keyed query must reject a different AST owner"
    );
}

#[test]
fn generic_lambda_access_uses_checked_context_without_a_concrete_type() {
    crate::typeck::check_str(
        "type Step:\n    Empty\n    Item(a)\n\n\
         type Iter:\n    Iter(fn() -> Step(a))\n\n\
         fn empty() -> Iter(a):\n    Iter(fn(): Empty)\n\n\
         fn accept(f: fn(Int) -> Option((a, Int))) -> Int:\n    0\n\n\
         fn wrap(x: a) -> Int:\n    accept(fn(i: Int): if true: Some((x, i)) else: None)\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect(
        "generic lambda nodes absent from the concrete type table use checked context and verify their bodies",
    );
}

#[test]
fn generic_call_specializes_type_variables_without_erasing_access() {
    crate::typeck::check_str(
        "fn fold(xs: List(a), init: b, combine: fn(b, a) -> b) -> b:\n    init\n\n\
         fn generic(xs: List(m), seed: m, combine: fn(m, m) -> m) -> m:\n    fold(xs, seed, combine)\n\n\
         fn inline(xs: List(m), seed: m) -> m:\n    fold(xs, seed, fn(acc: m, item: m) -> m: acc)\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect(
        "generic call-site substitution gives alpha-equivalent callback types one access identity, including a lambda checked under the unspecialized hint",
    );

    let error = crate::typeck::check_str(
        "fn use(f: fn(unique List(a)) -> Int) -> Int:\n    0\n\n\
         fn generic(f: fn(List(m)) -> Int) -> Int:\n    use(f)\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect_err("generic call-site substitution must preserve ownership qualifiers");
    assert!(
        error.contains("ownership/access contract") && error.contains("Qualifier"),
        "{error}"
    );
}

#[test]
fn callee_substitution_does_not_capture_caller_generic_names() {
    crate::typeck::check_str(
        "fn identity(value: a) -> a:\n    value\n\n\
         fn preserve(callback: fn(a) -> a) -> fn(a) -> a:\n    identity(callback)\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect("callee type variables are scoped independently from same-spelled caller variables");

    let error = crate::typeck::check_str(
        "fn identity(value: a) -> a:\n    value\n\n\
         fn erase(callback: fn(unique a) -> a) -> fn(a) -> a:\n    identity(callback)\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect_err("alpha-scope preservation must not permit qualifier erasure");
    assert!(
        error.contains("ownership/access contract") && error.contains("Qualifier"),
        "{error}"
    );
}

#[test]
fn result_only_generics_use_the_contextual_call_result_access_shape() {
    crate::typeck::check_str(
        "fn empty() -> Option(a):\n    None\n\n\
         fn main() -> Int:\n    let callbacks: Option(fn(Int) -> Int) = empty()\n    0\n",
    )
    .expect("a result-only generic specializes from the checked contextual call result");

    let error = crate::typeck::check_str(
        "fn empty() -> Option(unique a):\n    None\n\n\
         fn main() -> Int:\n    let callbacks: Option(fn(Int) -> Int) = empty()\n    0\n",
    )
    .expect_err("contextual result specialization must retain source qualifiers");
    assert!(
        error.contains("ownership/access contract") && error.contains("Qualifier"),
        "{error}"
    );
}

#[test]
fn generated_generator_lambda_uses_the_specialized_unfold_contract() {
    fn no_comptime(
        _name: &str,
        _module: &mut witchy_syntax::ast::Module,
        _siblings: &[(String, witchy_syntax::ast::Module)],
    ) -> Result<witchy_syntax::origin::OriginTable, String> {
        Ok(witchy_syntax::origin::OriginTable::default())
    }

    let main = witchy_syntax::parser::parse_module(
        "import iter\n\ngen fn numbers() -> Iter(Int):\n    yield 1\n\nfn main() -> Int:\n    0\n",
    )
    .expect("parse generator source");

    crate::pipeline::link_checked(
        vec![("main".into(), main)],
        "main",
        no_comptime,
    )
    .expect(
        "generator lowering evaluates its synthesized iter.unfold lambda under the concrete state contract",
    );
}

#[test]
fn earlier_argument_flow_refines_a_dependent_lambda_hint() {
    crate::typeck::check_str(
        "fn higher(seed: a, callback: fn(a) -> Int) -> Int:\n    callback(seed)\n\n\
         fn strict(xs: unique List(Int)) -> Int:\n    0\n\n\
         fn wrapper(seed: fn(unique List(Int)) -> Int) -> Int:\n    higher(seed, fn(callback): callback([1]))\n\n\
         fn main() -> Int:\n    wrapper(strict)\n",
    )
    .expect(
        "a prior argument's callable access flow specializes the callee-owned hint for a later lambda",
    );

    crate::typeck::check_str(
        "fn reversed(callback: fn(a) -> Int, seed: a) -> Int:\n    callback(seed)\n\n\
         fn strict(xs: unique List(Int)) -> Int:\n    0\n\n\
         fn wrapper(seed: fn(unique List(Int)) -> Int) -> Int:\n    reversed(fn(callback: fn(unique List(Int)) -> Int): callback([1]), seed)\n\n\
         fn main() -> Int:\n    wrapper(strict)\n",
    )
    .expect("an explicitly typed dependent lambda is independent of argument order");

    let error = crate::typeck::check_str(
        "fn higher(seed: a, callback: fn(a) -> Int) -> Int:\n    callback(seed)\n\n\
         fn bad(seed: fn(unique List(Int)) -> Int) -> Int:\n    higher(seed, fn(callback: fn(List(Int)) -> Int): callback([1]))\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect_err("dependent hint refinement must not overwrite an explicit caller contract");
    assert!(
        error.contains("ownership/access contract") && error.contains("Qualifier"),
        "{error}"
    );
}

#[test]
fn try_propagation_preserves_nested_callable_access() {
    crate::typeck::check_str(
        "fn option(value: Option(fn(unique List(Int)) -> Int)) -> Option(fn(unique List(Int)) -> Int):\n    let callback = value?\n    Some(callback)\n\n\
         fn result(value: Result(fn(unique List(Int)) -> Int, String)) -> Result(fn(unique List(Int)) -> Int, String):\n    let callback = value?\n    Ok(callback)\n\n\
         fn replace(var values: List(a), value: a) -> List(a):\n    [value]\n\n\
         fn preserve(callback: fn(unique List(Int)) -> Int) -> Int:\n    var callbacks = [callback]\n    replace(callbacks, callback)\n    0\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect(
        "Option and Result propagation unwrap the success callable flow, and generic writeback preserves the same nested contract",
    );

    let error = crate::typeck::check_str(
        "fn erase(value: Option(fn(List(Int)) -> Int)) -> Option(fn(unique List(Int)) -> Int):\n    let callback = value?\n    Some(callback)\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect_err("try propagation must not manufacture a stronger callable qualifier");
    assert!(
        error.contains("ownership/access contract") && error.contains("Qualifier"),
        "{error}"
    );
}

#[test]
fn generic_lambda_context_rejects_qualifier_and_convention_erasure() {
    let qualifier = crate::typeck::check_str(
        "fn accept(f: fn(unique List(Int)) -> Int) -> Int:\n    0\n\n\
         fn generic(x: a) -> Int:\n    accept(fn(xs: List(Int)): 0)\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect_err("a polymorphic lambda must not erase its checked unique parameter contract");
    assert!(
        qualifier.contains("ownership/access contract")
            && qualifier.contains("Qualifier"),
        "{qualifier}"
    );

    let convention = crate::typeck::check_str(
        "fn accept(f: fn(own List(Int)) -> Int) -> Int:\n    0\n\n\
         fn generic(x: a) -> Int:\n    accept(fn(xs: List(Int)): 0)\n\n\
         fn main() -> Int:\n    0\n",
    )
    .expect_err("a polymorphic lambda must not erase its checked own convention");
    assert!(
        convention.contains("ownership/access contract")
            || convention.contains("expected `fn(own List(Int)) -> Int`")
                && convention.contains("found `fn(List(Int)) -> Int`"),
        "{convention}"
    );
}

#[test]
fn indirect_apply_context_checks_a_generic_lambda_contract() {
    let accepted =
        "fn identity(f: fn(fn(List(a)) -> Int) -> Int) -> fn(fn(List(a)) -> Int) -> Int:\n    f\n\n\
         fn generic(x: a) -> Int:\n    identity(fn(callback: fn(List(a)) -> Int): callback([x]))(fn(xs: List(a)): 0)\n\n\
         fn main() -> Int:\n    0\n";
    let parsed = witchy_syntax::parser::parse_module(accepted).expect("parse indirect fixture");
    let tail = parsed
        .items
        .iter()
        .find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if function.name == "generic" => {
                function.body.stmts.last()
            }
            _ => None,
        })
        .expect("generic tail");
    assert!(
        matches!(tail, witchy_syntax::ast::Stmt::Expr(witchy_syntax::ast::Expr::Apply { .. })),
        "fixture must exercise Expr::Apply: {tail:?}"
    );
    crate::typeck::check_str(accepted)
        .expect("an indirect generic lambda preserves its checked callable contract");

    let erased =
        "fn identity(f: fn(fn(unique List(a)) -> Int) -> Int) -> fn(fn(unique List(a)) -> Int) -> Int:\n    f\n\n\
         fn generic(x: a) -> Int:\n    identity(fn(callback: fn(unique List(a)) -> Int): callback([x]))(fn(xs: List(a)): 0)\n\n\
         fn main() -> Int:\n    0\n";
    let error = crate::typeck::check_str(erased)
        .expect_err("indirect application must reject generic lambda qualifier erasure");
    assert!(
        error.contains("ownership/access contract") && error.contains("Qualifier"),
        "{error}"
    );
}

#[test]
fn tuple_context_checks_a_nested_generic_lambda_contract() {
    let accepted =
        "fn accept(pair: (Int, fn(List(a)) -> Int)) -> Int:\n    0\n\n\
         fn generic(x: a) -> Int:\n    accept((0, fn(xs: List(a)): 0))\n\n\
         fn main() -> Int:\n    0\n";
    crate::typeck::check_str(accepted)
        .expect("a tuple-contained generic lambda preserves its checked callable contract");

    let erased =
        "fn accept(pair: (Int, fn(unique List(a)) -> Int)) -> Int:\n    0\n\n\
         fn generic(x: a) -> Int:\n    accept((0, fn(xs: List(a)): 0))\n\n\
         fn main() -> Int:\n    0\n";
    let error = crate::typeck::check_str(erased)
        .expect_err("tuple context must reject nested generic lambda qualifier erasure");
    assert!(
        error.contains("ownership/access contract") && error.contains("Qualifier"),
        "{error}"
    );
}

#[test]
fn build_annotation_keys_types_to_the_returned_module_allocation() {
    let module = witchy_syntax::parser::parse_module(
        "fn main() -> Int:\n    let value = 1\n    value\n",
    )
    .expect("parse build module");
    let typed = crate::typeck::annotate_checked_build(module).expect("annotate build module");
    let tail = typed
        .module()
        .items
        .iter()
        .find_map(|item| match item {
            witchy_syntax::ast::Item::Function(function) if function.name == "main" => {
                match function.body.stmts.last() {
                    Some(witchy_syntax::ast::Stmt::Expr(expression)) => Some(expression),
                    _ => None,
                }
            }
            _ => None,
        })
        .expect("build tail expression");
    assert!(
        typed.table().type_of(tail).is_some(),
        "the build TypeTable must address the exact returned AST"
    );
    assert!(typed.table().function_type("main").is_some());
}
