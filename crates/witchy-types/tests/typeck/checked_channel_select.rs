use super::source::checked_module;
use witchy_syntax::ast::Expr;

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }

const SOURCE: &str = "from chan import Receiver, Selected\n\nasync fn consume(a: Receiver(Int), b: Receiver(Int)):\n    var count: Int = 0\n    while count < 2:\n        let selected = chan.select(a, b).await\n        match selected:\n            First(value) ->\n                count = count + value\n            Second(value) ->\n                count = count + value\n            Closed ->\n                count = 2\n    ()\n\nfn main():\n    ()\n";

#[test]
fn select_preparation_is_bound_to_exact_checked_operand() {
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let checked = checked_module(SOURCE).expect("checked select frame");
    let canonical = checked.canonical_source().expect("canonical checked source");
    let mut count = 0;
    witchy_syntax::ast::visit::visit_module_exprs(canonical.typed_module().module(), &mut |expression| {
        if let Some(preparation) = canonical.checked_select_preparation(expression) {
            count += 1;
            assert_eq!(preparation.endpoint_types()[0], preparation.endpoint_types()[1]);
            assert!(canonical.checked_select_preparation(&expression.clone()).is_none());
            assert!(matches!(expression, Expr::Call { args, .. } if args.len() == 1));
        }
        Ok::<_, ()>(())
    }).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn select_frame_rejects_effectful_standard_decoder() {
    let _reset = Reset;
    let channel = witchy_syntax::linker::bundled_source("chan").unwrap();
    let changed = channel.replacen("fn select2_result(k: Int, o: Option(__Msg)) -> Task(Selected(m)):\n    match o:",
        "fn select2_result(k: Int, o: Option(__Msg)) -> Task(Selected(m)):\n    fail(\"changed decoder ran\")\n    match o:", 1);
    assert_ne!(changed, channel);
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::none()));
    super::source::checked_bundle_with_std_override(vec![("main", SOURCE)], "main", Some(("chan", &changed)))
        .expect("ordinary select retains effectful decoder");
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let error = super::source::checked_bundle_with_std_override(vec![("main", SOURCE)], "main", Some(("chan", &changed)))
        .expect_err("compiler select frame must not discard decoder effects");
    assert!(error.contains("exact checked standard select implementation"), "{error}");
}
