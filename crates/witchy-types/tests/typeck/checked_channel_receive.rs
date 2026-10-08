use super::source::checked_module;
use witchy_syntax::ast::Expr;

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }

const SOURCE: &str = r#"from chan import Receiver

async fn consume(rx: Receiver(Int)):
    var count: Int = 0
    var values: List(Int) = [0]
    while count < 2:
        match chan.recv(rx).await:
            Some(value) ->
                list.set_at(values, 0, value)
                count = count + 1
            None -> count = 2
    ()

async fn main():
    let (tx, rx) = chan.channel(1).await
    consume(rx).await
"#;

#[test]
fn receive_state_frame_retains_original_task_and_source_resume_proof() {
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let checked = checked_module(SOURCE).expect("checked state receive");
    let canonical = checked.canonical_source().unwrap();
    let mut preparations = 0;
    witchy_syntax::ast::visit::visit_module_exprs(canonical.typed_module().module(), &mut |expression| {
        if let Some(preparation) = canonical.checked_receive_preparation(expression) {
            preparations += 1;
            assert!(canonical.checked_receive_preparation(&expression.clone()).is_none());
            assert!(matches!(preparation.endpoint_type(), witchy_syntax::ast::Type::Named(_, args) if args.len() == 1));
            assert!(matches!(expression, Expr::Call { args, .. } if matches!(args.as_slice(), [Expr::Block(_)])));
        }
        Ok::<_, ()>(())
    }).unwrap();
    assert_eq!(preparations, 1);
    assert_eq!(canonical.view().task_await_proofs().iter().filter(|proof| proof.source_identity().1 == 0).count(), 1);
}

#[test]
fn receive_state_frame_rejects_changed_decoder_and_adapters() {
    let _reset = Reset;
    let original = witchy_syntax::linker::bundled_source("chan").unwrap();
    for (old, new) in [
        ("Some(x) -> Some(__unerase(x))", "Some(x) ->\n            fail(\"changed receive decoder\")\n            Some(__unerase(x))"),
        ("resume(unerase_option(message), current)", "resume(None, current)"),
    ] {
        let changed = original.replacen(old, new, 1);
        assert_ne!(changed, original);
        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::none()));
        super::source::checked_bundle_with_std_override(vec![("main", SOURCE)], "main", Some(("chan", &changed)))
            .expect("ordinary source remains valid");
        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
        let error = super::source::checked_bundle_with_std_override(vec![("main", SOURCE)], "main", Some(("chan", &changed)))
            .expect_err("frame must not discard changed standard source effects");
        assert!(error.contains("exact checked standard receive implementation"), "{error}");
    }
}

#[test]
fn receive_state_frame_rejects_changed_probe_and_close_scheduler() {
    let _reset = Reset;
    let original = witchy_syntax::linker::bundled_source("task").unwrap();
    for (old, new) in [
        ("else if kind == 5:\n                let recipe = list.at(frame_recipes, i)\n                if list.at(frame_phases, i) == 1:",
         "else if kind == 5:\n                let recipe = list.at(frame_recipes, i)\n                if list.at(frame_phases, i) == 2:"),
        ("ReceiveStateSlot(activation) ->\n                        list.set_at(frame_pending, activation, None)",
         "ReceiveStateSlot(activation) ->\n                        fail(\"changed receive close\")\n                        list.set_at(frame_pending, activation, None)"),
    ] {
        let changed = original.replacen(old, new, 1);
        assert_ne!(changed, original);
        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::none()));
        super::source::checked_bundle_with_std_override(vec![("main", SOURCE)], "main", Some(("task", &changed)))
            .expect("ordinary source remains valid");
        witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
        let error = super::source::checked_bundle_with_std_override(vec![("main", SOURCE)], "main", Some(("task", &changed)))
            .expect_err("frame must not assume changed scheduler phases");
        assert!(error.contains("changed scalar scheduler recipe task.run"), "{error}");
    }
}
