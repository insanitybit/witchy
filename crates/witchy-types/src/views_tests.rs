use witchy_syntax::ast::{Expr, Item, Stmt, Type, TypeQual};

use crate::access::{AccessKind, AccessSignature, OwnershipStateClass};
use crate::typeck::{annotate_checked, check, Ty};

fn checked(source: &str) -> crate::typeck::TypedModule {
    let module = witchy_syntax::parser::parse_module(source).expect("source parses");
    annotate_checked(module).expect("source type-checks")
}

fn checked_with_method_resolution(source: &str) -> crate::typeck::TypedModule {
    let module = witchy_syntax::parser::parse_module(source).expect("source parses");
    let mut lowered = crate::traits::lower_checked(module).expect("methods resolve");
    witchy_syntax::parser::lower_sugar_module(&mut lowered);
    annotate_checked(lowered).expect("source type-checks")
}

#[test]
fn rooted_views_return_aggregate_generic_and_capture_without_lifetime_relations() {
    let typed = checked(
        "type Holder(a):\n    value: &a\n\n\
         fn keep[a](value: &a) -> &a:\n    value\n\n\
         fn hold[a](value: &a) -> Holder(a):\n    Holder(value)\n\n\
         fn capture[a](value: &a) -> fn() -> &a:\n    let callback: fn() -> &a = fn() -> &a:\n        value\n    callback\n\n\
         fn first[a](items: &[a]) -> Option(&a):\n    Some(&items[0])\n\n\
         fn main() -> Int:\n    let values = [1, 2, 3]\n    let view = &values\n    let _ = keep(view)\n    let _ = hold(view)\n    let callback = capture(view)\n    let _ = callback()\n    let slice = &values[0..3]\n    let _ = first(slice)\n    0\n",
    );
    let Item::Function(keep) = &typed.module().items[1] else { panic!("keep") };
    let parameter = keep.params[0].ty.as_ref().expect("typed parameter");
    assert!(matches!(parameter, Type::Qualified(TypeQual::View, _)));
    let Stmt::Expr(value) = &keep.body.stmts[0] else { panic!("keep body") };
    assert!(matches!(
        typed.table().type_of(value).or_else(|| typed.table().view_type_of(value)),
        Some(Ty::View(_))
    ));
    assert!(typed.loan_facts().is_err(), "canonical views never enter lexical LoanFacts");
    let facts = typed.view_facts().expect("rooted view facts");
    assert!(facts.iter().count() >= 3, "each &place formation is authenticated");
}

#[test]
fn rooted_view_snapshot_remains_readable_after_root_rebinding() {
    checked(
        "fn main() -> Int:\n    var value = 7\n    let snapshot = &value\n    value = 9\n    return *snapshot\n",
    );
}

#[test]
fn rooted_slices_support_length_and_element_view_projection() {
    let typed = checked_with_method_resolution(
        "fn first[a](items: &[a]) -> Option(&a):\n    if items.length() == 0:\n        None\n    else:\n        Some(&items[0])\n\n\
         fn main() -> Int:\n    let values = [1, 2, 3]\n    let slice = &values[1..3]\n    let _ = first(slice)\n    0\n",
    );
    let facts = typed.view_facts().expect("rooted view facts");
    assert!(
        facts.iter().any(|fact| fact.place().root() == "items"),
        "the element view remains an authenticated projection of the slice carrier",
    );

    for source in [
        "fn invalid(items: &[Int]):\n    let _ = list.push(items, 1)\n",
        "fn invalid(items: &[Int]):\n    let _ = list.__set_at(items, 0, 1)\n",
        "fn invalid(items: &[unique String]):\n    let _ = list.at(items, 0)\n",
        "must type Ticket:\n    Ticket(Int)\n\nfn invalid(items: &[Ticket]):\n    let _ = list.at(items, 0)\n",
    ] {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        assert!(
            check(&module).is_err(),
            "a rooted slice is never a mutable List receiver or generic by-value read: {source}"
        );
    }
    let module = witchy_syntax::parser::parse_module(
        "fn invalid(items: &[unique String]):\n    items.at(0)\n",
    )
    .expect("source parses");
    assert!(
        crate::traits::lower_checked(module).is_err(),
        "a rooted slice exposes no generic element extraction method"
    );
    let module = witchy_syntax::parser::parse_module(
        "type Slice(a):\n    value: a\n\nfn invalid(items: &Slice(Int)):\n    items.length()\n",
    )
    .expect("source parses");
    assert!(
        crate::traits::lower_checked(module).is_err(),
        "the internal Slice carrier representation is not a public type spelling"
    );
}

#[test]
fn rooted_views_return_aggregate_and_capture_without_loan_relations() {
    let typed = checked(
        "type Holder:\n    value: &Int\n\n\
         fn keep(value: &Int) -> &Int:\n    value\n\n\
         fn hold(value: &Int) -> Holder:\n    Holder(value)\n\n\
         fn capture(value: &Int) -> fn() -> &Int:\n    let callback: fn() -> &Int = fn() -> &Int:\n        value\n    callback\n\n\
         fn main() -> Int:\n    let value = 1\n    let view = &value\n    let _ = keep(view)\n    let _ = hold(view)\n    let callback = capture(view)\n    let _ = callback()\n    0\n",
    );
    let Item::Function(keep) = &typed.module().items[1] else { panic!("keep") };
    let signature = AccessSignature::from_function(keep).expect("view callable signature");
    assert!(signature.borrow_relations().is_empty(), "views carry no lexical lifetime relation");
    assert!(typed.loan_facts().is_err(), "canonical views never enter lexical LoanFacts");
    assert!(typed.view_facts().expect("view facts").iter().count() >= 1);
}

#[test]
fn dynamic_slice_view_records_exact_endpoints() {
    let typed = checked(
        "fn slice(items: List(Int), lo: Int, hi: Int) -> &[Int]:\n    &items[lo..hi]\n",
    );
    let Item::Function(function) = &typed.module().items[0] else { panic!("function") };
    let Stmt::Expr(view @ Expr::Unary { .. }) = &function.body.stmts[0] else { panic!("view") };
    let fact = typed.view_facts().expect("view facts").view_for(view).expect("exact view fact");
    assert!(fact.dynamic_range().is_some(), "dynamic bounds remain authenticated nodes");
}

#[test]
fn view_formed_through_a_view_roots_in_the_view_binding() {
    let typed = checked(
        "type Rec:\n    label: String\n\nfn label(r: &Rec) -> &String:\n    &(*r).label\n",
    );
    let Item::Function(function) = &typed.module().items[1] else { panic!("function") };
    let Stmt::Expr(view @ Expr::Unary { .. }) = &function.body.stmts[0] else { panic!("view") };
    let fact = typed.view_facts().expect("view facts").view_for(view).expect("exact view fact");
    assert!(fact.formed_through_view(), "the dereferenced base is recorded");
    assert_eq!(fact.place().root(), "r");

    let through_call = witchy_syntax::parser::parse_module(
        "type Rec:\n    n: Int\n\nfn make() -> &Rec:\n    let r = Rec(1)\n    &r\n\nfn invalid() -> &Int:\n    &(*make()).n\n",
    )
    .expect("source parses");
    assert!(annotate_checked(through_call).is_err(), "only a dereferenced view binding is a view base");
}

#[test]
fn dynamic_element_view_records_exact_index() {
    let typed = checked(
        "fn element(items: List(Int), index: Int) -> &Int:\n    &items[index]\n",
    );
    let Item::Function(function) = &typed.module().items[0] else { panic!("function") };
    let Stmt::Expr(view @ Expr::Unary { .. }) = &function.body.stmts[0] else { panic!("view") };
    let fact = typed.view_facts().expect("view facts").view_for(view).expect("exact view fact");
    let Expr::Unary { expr, .. } = view else { unreachable!() };
    let Expr::Index { index, .. } = expr.as_ref() else { panic!("element projection") };
    assert!(fact.dynamic_element().is_some_and(|checked| checked.matches(index)));
}

#[test]
fn view_conventions_operate_on_the_carrier_not_the_referent() {
    let module = witchy_syntax::parser::parse_module(
        "fn relay(var view: &Int) -> ():\n    ()\n\nfn take(own view: &Int):\n    ()\n\nfn main():\n    var value = 1\n    var view = &value\n    relay(view)\n    take(view)\n",
    )
    .expect("source parses");
    check(&module).expect("var replaces a view carrier and own consumes it");
    let signature = AccessSignature::from_function(match &module.items[0] {
        Item::Function(function) => function,
        _ => unreachable!(),
    })
    .expect("access signature");
    assert_eq!(signature.params()[0].kind(), AccessKind::ExclusiveWriteback);
    assert_eq!(signature.params()[0].ownership().writeback(), Some(&OwnershipStateClass::RootedViewCarrier));
}

#[test]
fn ordinary_view_boundaries_retain_the_rooted_carrier() {
    let module = witchy_syntax::parser::parse_module(
        "fn relay(view: &Int) -> &Int:\n    view\n\nfn capture(view: &Int) -> fn() -> &Int:\n    let callback: fn() -> &Int = fn() -> &Int:\n        view\n    callback\n",
    )
    .expect("source parses");
    check(&module).expect("a captured and returned view remains a copyable carrier");

    let Item::Function(relay) = &module.items[0] else { panic!("relay") };
    let signature = AccessSignature::from_function(relay).expect("access signature");
    assert_eq!(
        signature.params()[0].ownership().input(),
        Some(&OwnershipStateClass::RootedViewCarrier),
        "ordinary view inputs retain their authenticated root",
    );
    assert_eq!(
        signature.result().ownership_output(),
        Some(&OwnershipStateClass::RootedViewCarrier),
        "ordinary view results retain their authenticated root",
    );
    assert!(signature.borrow_relations().is_empty(), "carrier state is not a lifetime relation");
}

#[test]
fn view_qualifiers_and_unsupported_authority_carriers_reject() {
    for source in [
        "fn invalid(value: unique &Int):\n    ()\n",
        "fn invalid(value: frozen &Int):\n    ()\n",
        "fn invalid(value: &Console):\n    ()\n",
    ] {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        assert!(check(&module).is_err(), "{source}");
    }
    for source in [
        "fn inspect(value: &unique Int):\n    ()\n",
        "fn inspect(value: &frozen Int):\n    ()\n",
    ] {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        check(&module).expect("payload qualifiers remain legal");
    }
}

#[test]
fn view_formation_is_checked_and_own_consumes_the_carrier() {
    let non_place = witchy_syntax::parser::parse_module(
        "fn invalid() -> &Int:\n    &1\n",
    )
    .expect("source parses");
    assert!(check(&non_place).is_err(), "raw expressions cannot form a view");

    let const_non_place = witchy_syntax::parser::parse_module("let cached = &1\n")
        .expect("source parses");
    assert!(check(&const_non_place).is_err(), "const view formation is checked too");

    let use_after_own = witchy_syntax::parser::parse_module(
        "fn take(own view: &Int):\n    ()\n\nfn main():\n    let value = 1\n    let view = &value\n    take(view)\n    take(view)\n",
    )
    .expect("source parses");
    assert!(check(&use_after_own).is_err(), "own consumes the carrier value");
}

#[test]
fn views_cannot_launder_must_or_once_payload_obligations() {
    for source in [
        "must type Ticket:\n    Ticket(Int)\n\nfn main():\n    let ticket = Ticket(1)\n    let view = &ticket\n",
        "fn main():\n    let action: once fn() -> Int = once fn() -> Int: 1\n    let view = &action\n",
    ] {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        assert!(check(&module).is_err(), "copyable views preserve payload obligations: {source}");
    }
}

#[test]
fn canonical_views_do_not_disable_legacy_loan_enforcement_in_mixed_modules() {
    let mut module = witchy_syntax::parser::parse_module(
        "mode opt\n\nfn legacy(value: Int) -> Int:\n    value\n\nfn rooted(value: &Int) -> &Int:\n    value\n",
    )
    .expect("source parses");
    let legacy = match &mut module.items[0] {
        Item::Function(function) => function,
        _ => unreachable!(),
    };
    let borrowed_int = Type::Qualified(
        TypeQual::Borrow("migration".into()),
        Box::new(Type::Named("Int".into(), Vec::new())),
    );
    legacy.params[0].ty = Some(borrowed_int.clone());
    legacy.ret = Some(borrowed_int);
    let error = check(&module).expect_err("mixed canonical and legacy loans fail closed");
    assert!(error.message.contains("cannot be mixed with legacy lifetime loans"), "{error:?}");
}

#[test]
fn generic_view_constraints_are_checked_at_direct_specialization_and_not_transportable() {
    let first = witchy_syntax::parser::parse_module(
        "fn first[a](items: &[a]) -> Option(&a):\n    Some(&items[0])\n\nfn main() -> Int:\n    let values = [1, 2, 3]\n    let slice = &values[0..3]\n    let _ = first(slice)\n    0\n",
    )
    .expect("source parses");
    check(&first).expect("first[Int] resolves a checked generic view payload");

    for source in [
        "fn make[a](value: a) -> &a:\n    &value\n\nfn main(console: Console) -> Int:\n    let _ = make(console)\n    0\n",
        "must type Ticket:\n    Ticket(Int)\n\nfn make[a](own value: a) -> &a:\n    &value\n\nfn main() -> Int:\n    let ticket = Ticket(1)\n    let _ = make(ticket)\n    0\n",
        "fn make[a](own value: a) -> &a:\n    &value\n\nfn main() -> Int:\n    let action: once fn() -> Int = once fn() -> Int: 1\n    let _ = make(action)\n    0\n",
    ] {
        let module = witchy_syntax::parser::parse_module(source).expect("source parses");
        assert!(check(&module).is_err(), "generic rooted views reject inadmissible specialization: {source}");
    }

    let indirect = witchy_syntax::parser::parse_module(
        "fn make[a](value: a) -> &a:\n    &value\n\nfn main():\n    let function = make\n",
    )
    .expect("source parses");
    let error = check(&indirect).expect_err("function values cannot erase view constraints");
    assert!(error.message.contains("cannot be transported as a function value"), "{error:?}");
}
