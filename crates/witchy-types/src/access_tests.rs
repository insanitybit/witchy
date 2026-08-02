use witchy_syntax::ast::{Convention, Type, TypeQual};

use crate::access::{
    AccessKind, AccessMismatchKind, AccessQualifier, AccessSignature, AccessSignatureError,
    OwnershipStateClass, SignaturePosition, ownership_state_class,
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
        Some(&OwnershipStateClass::LayoutDependent)
    );
    assert_eq!(
        sig.params()[2].ownership().writeback(),
        Some(&OwnershipStateClass::LayoutDependent)
    );
    assert_eq!(
        sig.params()[3].ownership().input(),
        Some(&OwnershipStateClass::LayoutDependent)
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
        Some(&OwnershipStateClass::LayoutDependent)
    );
    assert_eq!(
        sig.params()[1].ownership().input(),
        Some(&OwnershipStateClass::LayoutDependent)
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
        Some(&OwnershipStateClass::LayoutDependent)
    );

    let scalar = signature(
        Vec::new(),
        qualified(TypeQual::Unique, named("Int")),
        Vec::new(),
    );
    assert_eq!(scalar.result().ownership_output(), None);
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
    assert_eq!(sig.borrow_relations()[0].owner_positions(), &[0, 2]);
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
        Some(OwnershipStateClass::LayoutDependent)
    );
    assert_eq!(
        ownership_state_class(&Type::Tuple(vec![named("Int"), named("String")])).unwrap(),
        Some(OwnershipStateClass::Aggregate(vec![
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
