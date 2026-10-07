use witchy_syntax::ast::{Convention, Type, TypeQual};

use witchy_canonical_source::access::{
    AccessMismatchKind, AccessSignature, AccessSignatureError, BorrowRelationCatalog,
    SignaturePosition,
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

fn catalog_signature(source: &str, function_name: &str) -> AccessSignature {
    let module = witchy_syntax::parser::parse_reference_migration(source).expect("parse access fixture");
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

#[test]
fn exact_verifier_rejects_nominal_projection_erasure() {
    let required = catalog_signature(
        "mode opt\n\n\
         type Wrapper('scope):\n    view: &'scope String\n\n\
         fn wrap(value: &'scope String) -> Wrapper('scope):\n    Wrapper(value)\n",
        "wrap",
    );
    let erased = catalog_signature(
        "mode opt\n\n\
         type Wrapper('scope):\n    value: String\n\n\
         fn wrap(value: &'scope String) -> Wrapper('scope):\n    Wrapper(value)\n",
        "wrap",
    );

    let error = required.verify_exact(&erased).expect_err("projection erasure must fail");
    assert_eq!(error.kind(), AccessMismatchKind::BorrowRelation);
    assert_ne!(required.identity_key(), erased.identity_key());
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
    for qualifier in [TypeQual::Unique, TypeQual::Frozen] {
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
    AccessSignature::from_parts(
        vec![qualified(TypeQual::Frozen, list(named("Int")))],
        named("Nil"),
        vec![Convention::Var],
    )
    .expect("a var parameter may replace its frozen value");
    AccessSignature::from_parts(
        vec![qualified(TypeQual::Borrow("a".to_string()), named("String"))],
        named("Nil"),
        vec![Convention::Var],
    )
    .expect("a var parameter may replace its shared-reference handle");
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
fn generic_lambda_context_rejects_qualifier_and_convention_erasure() {
    let qualifier = witchy_types::typeck::check_reference_migration_str(
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

    let convention = witchy_types::typeck::check_reference_migration_str(
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
fn generic_entry_match_preserves_stored_callback_access() {
    let valid = "type Cell(a):\n    Cell(a, fn(var Int) -> Int)\n\nfn forward(own cell: Cell(a)) -> Cell(a):\n    match cell:\n        Cell(value, callback) -> Cell(value, callback)\n\nfn increment(var n: Int) -> Int:\n    n = n + 1\n    n\n\nfn main() -> Int:\n    let _ = forward(Cell(0, increment))\n    0\n";
    witchy_types::typeck::check_reference_migration_str(valid).expect("generic field callback keeps its declared contract");
    let invalid = valid.replace("Cell(value, callback) -> Cell(value, callback)", "Cell(value, callback) -> Cell(value, fn(n: Int): n)");
    let error = witchy_types::typeck::check_reference_migration_str(&invalid)
        .expect_err("generic entry match cannot erase var access");
    assert!(error.contains("ownership/access contract") || error.contains("fn(var Int)"), "{error}");
}
