use witchy_syntax::ast::{Expr, Type};
use witchy_types::checked_channel_send::checked_channel_send_sites;
use super::source::checked_module;

#[test]
fn send_sites_keep_source_operands_and_final_await_binding() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let checked = checked_module("from chan import Sender\n\nasync fn produce(tx: Sender(Int), limit: Int):\n    var i: Int = 0\n    while i < limit:\n        chan.send(tx, i + 1).await\n        i = i + 1\n    ()\n\nfn main():\n    ()\n").expect("checked producer");
    let sites = checked_channel_send_sites(&checked).expect("checked send sites");
    assert_eq!(sites.len(), 1);
    let site = &sites[0];
    assert!(matches!(site.endpoint(), Expr::Var(name) if name == "tx"));
    assert!(matches!(site.payload(), Expr::Binary { .. }));
    assert_eq!(site.payload_type(), &Type::Named("Int".into(), vec![]));
    assert_eq!(site.proof().source_identity().3, 0);
    assert!(site.continuation().is_some());
    site.validate_for(&checked).expect("same checked allocation");
    let canonical = checked.canonical_source().expect("canonical source");
    let mut prepared = 0;
    for item in &canonical.typed_module().module().items {
        if let witchy_syntax::ast::Item::Function(function) = item {
            witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
                if canonical.checked_send_preparation(expression).is_some() {
                    prepared += 1;
                    assert!(canonical.checked_send_preparation(&expression.clone()).is_none(),
                        "a detached AST cannot borrow the exact operation witness");
                }
                Ok::<_, ()>(())
            }).unwrap();
        }
    }
    assert_eq!(prepared, 1, "producer preparation has an exact executable witness");
    let other = checked_module("fn main():\n    ()\n").expect("other checked allocation");
    assert_eq!(site.validate_for(&other).unwrap_err(),
        "channel-send site belongs to a different checked allocation");
}

#[test]
fn user_send_spelling_does_not_select_channel_operation() {
    let checked = checked_module("async fn send(value: Int):\n    ()\n\nasync fn use_send():\n    send(1).await\n    ()\n\nfn main():\n    ()\n").expect("checked user send");
    assert!(checked_channel_send_sites(&checked).expect("checked sites").is_empty());
}

#[test]
fn send_sites_distinguish_shadowed_payload_bindings_and_prior_awaits() {
    let checked = checked_module("from chan import Sender\n\nasync fn produce(tx: Sender(Int), value: Int):\n    chan.yield_now().await\n    chan.send(tx, value).await\n    let value = 7\n    chan.send(tx, value).await\n    ()\n\nfn main():\n    ()\n").expect("checked shadowed sends");
    let sites = checked_channel_send_sites(&checked).expect("checked send sites");
    assert_eq!(sites.len(), 2);
    assert_eq!(sites[0].proof().source_identity().3, 1);
    assert_eq!(sites[1].proof().source_identity().3, 2);
    assert_ne!(sites[0].expression(), sites[1].expression());
    assert!(!std::ptr::eq(sites[0].payload(), sites[1].payload()));
}
