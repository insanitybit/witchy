//! RFC-0112 structural callable-owner evidence.

use witchy_syntax::ast::{Expr, Function, Item, Module, Stmt, Type};
use witchy_types::access::{
    AccessKind, AccessQualifier, AccessSignature, LoanProjection, LoanProjectionStep,
    OwnershipStateClass, checked_facts,
};
use witchy_types::{traits, typeck};

const CALLABLE_OWNER_MATRIX: &str = r#"
mode opt

type Holder('a):
    view: View(String, 'a)

type Catalog:
    marker: Int

trait SuppliesCallable:
    fn callable() -> fn(let View(String, 'supplied), let View(Holder('supplied), 'supplied)) -> View(String, 'supplied)

impl SuppliesCallable for Catalog:
    fn callable() -> fn(let View(String, 'supplied), let View(Holder('supplied), 'supplied)) -> View(String, 'supplied):
        direct

fn direct(let owner: let('a) String, let holder: let('a) Holder('a)) -> View(String, 'a):
    owner

fn matrix() -> Int:
    let direct_value: fn(let View(String, 'direct), let View(Holder('direct), 'direct)) -> View(String, 'direct) = direct
    let alpha_value: fn(let View(String, 'renamed), let View(Holder('renamed), 'renamed)) -> View(String, 'renamed) = direct
    let closure_value: fn(let View(String, 'closure), let View(Holder('closure), 'closure)) -> View(String, 'closure) = fn(let owner: let('inner) String, let holder: let('inner) Holder('inner)) -> View(String, 'inner):
        owner
    let static_value: fn(let View(String, 'static), let View(Holder('static), 'static)) -> View(String, 'static) = Catalog.callable()

    let direct_observed = direct_value
    let alpha_observed = alpha_value
    let closure_observed = closure_value
    let static_observed = static_value
    0
"#;

const RELATION_CHANGING_ASCRIPTION: &str = r#"
mode opt

type Holder('a):
    view: View(String, 'a)

fn direct(let owner: let('a) String, let holder: let('a) Holder('a)) -> View(String, 'a):
    owner

fn matrix() -> Int:
    let wrong: fn(let View(String, 'left), let View(Holder('right), 'right)) -> View(String, 'left) = direct
    0
"#;

fn function<'module>(module: &'module Module, name: &str) -> &'module Function {
    module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing function `{name}`"))
}

fn binding<'module>(module: &'module Module, name: &str) -> &'module Expr {
    function(module, "matrix")
        .body
        .stmts
        .iter()
        .find_map(|statement| match statement {
            Stmt::Let {
                name: binding,
                value,
                ..
            } if binding == name => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing matrix binding `{name}`"))
}

fn assert_same_access_identity(required: &AccessSignature, candidate: &AccessSignature) {
    required
        .verify_exact(candidate)
        .expect("the callable shape must preserve the declaration's access identity");
    candidate
        .verify_exact(required)
        .expect("callable access identity must be symmetric under lifetime alpha-renaming");
}

fn assert_holder_owner_contract(signature: &AccessSignature) {
    assert!(signature.callable_qualifiers().is_empty());
    assert_eq!(signature.params().len(), 2);

    let [relation] = signature.borrow_relations() else {
        panic!("the callable result must retain one exact borrowed-storage relation")
    };
    let lifetime = relation.lifetime();

    for parameter in signature.params() {
        assert_eq!(parameter.kind(), AccessKind::SharedBorrow);
        assert_eq!(
            parameter.qualifiers(),
            &[AccessQualifier::Borrow(lifetime.to_string())]
        );
        assert_eq!(
            parameter
                .borrow_lifetimes()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            [lifetime]
        );
        assert_eq!(
            parameter.ownership().input(),
            Some(&OwnershipStateClass::BorrowedOwnerRoot {
                lifetime: lifetime.to_string()
            })
        );
        assert!(parameter.ownership().writeback().is_none());
    }

    assert_eq!(
        signature.result().qualifiers(),
        &[AccessQualifier::Borrow(lifetime.to_string())]
    );
    assert_eq!(
        signature
            .result()
            .borrow_lifetimes()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [lifetime]
    );
    assert_eq!(
        signature.result().ownership_output(),
        Some(&OwnershipStateClass::BorrowedOwnerRoot {
            lifetime: lifetime.to_string()
        })
    );

    assert_eq!(relation.output_projection(), &LoanProjection::default());
    assert_eq!(
        relation.storage_type(),
        &Type::Named("String".into(), Vec::new())
    );
    let owners = relation.owners();
    assert_eq!(
        owners.len(),
        2,
        "owner roots must neither disappear nor multiply"
    );
    assert_eq!(owners[0].position(), 0);
    assert_eq!(owners[0].input_projection(), &LoanProjection::default());
    assert_eq!(owners[1].position(), 1);
    assert_eq!(
        owners[1].input_projection(),
        &LoanProjection {
            steps: vec![LoanProjectionStep::Field("view".into())]
        }
    );
}

fn assert_runtime_holder_construction_is_gated(module: &Module, static_name: &str) {
    let direct = function(module, "direct");
    assert!(
        matches!(direct.body.stmts.as_slice(), [Stmt::Expr(Expr::Var(name))] if name == "owner"),
        "direct may describe Holder ownership, but must not construct or return a Holder value"
    );

    let provider = function(module, static_name);
    assert!(
        matches!(provider.body.stmts.as_slice(), [Stmt::Expr(Expr::Var(name))] if name == "direct"),
        "the static provider must return only callable metadata, never a Holder value"
    );

    let closure = binding(module, "closure_value");
    assert!(
        matches!(closure, Expr::Lambda { body, .. }
            if matches!(body.stmts.as_slice(), [Stmt::Expr(Expr::Var(name))] if name == "owner")),
        "the closure may return the ordinary String view only"
    );

    let static_call = binding(module, "static_value");
    assert!(
        matches!(static_call, Expr::Call { args, .. } if args.is_empty()),
        "resolved static evidence must take zero runtime arguments, so it cannot transport Holder"
    );

    for name in [
        "direct_value",
        "alpha_value",
        "direct_observed",
        "alpha_observed",
        "closure_observed",
        "static_observed",
    ] {
        assert!(
            matches!(binding(module, name), Expr::Var(_)),
            "`{name}` must transport only a callable identity"
        );
    }

    // This test intentionally stops at checked structural facts. Do not add an
    // interpreter or codegen invocation until runtime Holder construction has a
    // separately accepted lowering contract.
}

#[test]
fn callable_shapes_share_one_exact_structural_owner_identity() {
    let parsed = witchy_syntax::parser::parse_module(CALLABLE_OWNER_MATRIX)
        .expect("parse RFC-0112 callable-owner matrix");
    let lowered = traits::lower_checked(parsed).expect("resolve the static trait method");
    let typed = typeck::annotate_checked(lowered).expect("typecheck callable-owner matrix");
    let module = typed.module();
    let facts = checked_facts(module, typed.table()).expect("one final checked access authority");

    let direct = facts
        .declaration("direct")
        .expect("direct declaration access identity");
    assert_holder_owner_contract(direct);

    let mut shapes = Vec::new();
    for name in [
        "direct_observed",
        "alpha_observed",
        "closure_value",
        "closure_observed",
        "static_value",
        "static_observed",
    ] {
        let signature = facts
            .callable_at(module, binding(module, name))
            .unwrap_or_else(|| panic!("missing checked callable identity for `{name}`"));
        assert_holder_owner_contract(signature);
        shapes.push(signature);
    }

    for signature in shapes {
        assert_same_access_identity(direct, signature);
    }

    let Expr::Call {
        name: static_name,
        args,
    } = binding(module, "static_value")
    else {
        panic!("trait lowering must resolve Catalog.callable() to a direct call")
    };
    assert!(args.is_empty());
    assert!(
        static_name.contains("SuppliesCallable")
            && static_name.contains("Catalog")
            && static_name.ends_with("callable"),
        "the static call must retain its concrete trait-impl identity: {static_name}"
    );
    let selected = facts
        .call_at(module, binding(module, "static_value"))
        .expect("checked resolved-static call identity");
    let declared = facts
        .declaration(static_name)
        .expect("lowered static trait declaration identity");
    assert_same_access_identity(declared, selected);

    assert_runtime_holder_construction_is_gated(module, static_name);
}

#[test]
fn relation_changing_callable_ascription_is_rejected() {
    let parsed = witchy_syntax::parser::parse_module(RELATION_CHANGING_ASCRIPTION)
        .expect("parse relation-changing callable ascription");
    let lowered = traits::lower_checked(parsed).expect("lower relation-changing fixture");
    let typed = typeck::annotate_checked(lowered)
        .expect("ordinary type shape remains valid before access-identity checking");
    let Err(error) = checked_facts(typed.module(), typed.table()) else {
        panic!("splitting one callable lifetime across owner positions must be rejected")
    };
    let diagnostic = error.to_string();
    assert_eq!(
        diagnostic,
        "function value `wrong` erases or changes its ownership/access contract (parameter 1 does not preserve BorrowRelation)"
    );
}
