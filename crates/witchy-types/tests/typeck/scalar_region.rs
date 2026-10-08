const SOURCE: &str = r#"from chan import Receiver, Sender
async fn producer(tx: Sender(Int), n: Int):
    var i = 0
    while i < n:
        chan.send(tx, i).await
        i = i + 1
async fn consume(console: Console, rx: Receiver(Int)):
    var sum = 0
    for await value in rx:
        sum = sum + value
    console.print("${sum}")
async fn main(console: Console, clock: Clock):
    let before = clock.now_monotonic()
    let (tx, rx) = chan.channel(1).await
    chan.scope([producer(tx, 7), consume(console, rx)]).await
    let after = clock.now_monotonic()
    console.print("duration=${after - before}")
"#;

#[test]
fn closed_scalar_receipt_preserves_explicit_and_implicit_source_coverage() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(options));
        let checked = super::source::checked_module(SOURCE).expect("checked closed source kernel");
        let view = checked.canonical_source().expect("canonical checked source").view();
        assert_eq!(view.task_await_proofs().len(), 3);
        assert_eq!(view.scalar_region().is_some(), witchy_syntax::opt::enabled(witchy_syntax::opt::Opt::ClosureElide));
        if let Some(receipt) = view.scalar_region() {
            assert_eq!(receipt.kind(), witchy_canonical_source::scalar_region::RegionKind::ScalarScope);
            assert_eq!(receipt.covered_awaits().len(), 3);
            assert!(receipt.validate(view.module(), view.task_await_proofs()));
            assert_eq!(view.source_witness().scalar_region_manifest_digest(), receipt.digest());
        }
    }
}

#[test]
fn closed_scalar_refuses_changed_scheduler_and_scope_recipes() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let task = witchy_syntax::linker::std_source("task").unwrap();
    let (before, run) = task.split_once("pub fn run(root:").unwrap();
    for (old, new) in [("let n = slot_count", "let n = slot_count + 0"),
        ("var i = 0", "var i = 1"),
        ("while go:", "while go && slot_count > 0:")] {
        assert!(run.contains(old));
        let altered = format!("{before}pub fn run(root:{}", run.replacen(old, new, 1));
        let checked = super::source::checked_bundle_with_std_override(vec![("main", SOURCE)], "main", Some(("task", &altered)))
            .expect("ordinary changed scheduler remains source-checkable");
        assert!(checked.canonical_source().unwrap().view().scalar_region().is_none(), "changed {old}");
    }
    let chan = witchy_syntax::linker::std_source("chan").unwrap();
    let altered = chan.replace("spawn_all_into(children, 0, [], fn(ids): join_all_from(ids, 0))",
        "spawn_all_into(children, 1, [], fn(ids): join_all_from(ids, 0))");
    assert_ne!(chan, altered);
    let checked = super::source::checked_bundle_with_std_override(vec![("main", SOURCE)], "main", Some(("chan", &altered)))
        .expect("ordinary changed Scope remains source-checkable");
    assert!(checked.canonical_source().unwrap().view().scalar_region().is_none());
}

#[test]
fn closed_scalar_receipt_rejects_detached_body_and_await_inventory() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let checked = super::source::checked_module(SOURCE).expect("checked scalar region");
    let view = checked.canonical_source().unwrap().view();
    let receipt = view.scalar_region().expect("selected region receipt");
    let mut changed = view.module().clone();
    let witchy_syntax::ast::Item::Function(companion) = &mut changed.items[receipt.companion_item() as usize] else { panic!("companion") };
    companion.body.stmts.push(witchy_syntax::ast::Stmt::Expr(witchy_syntax::ast::Expr::Tuple(vec![])));
    companion.body.lines.push(0);
    assert!(!receipt.validate(&changed, view.task_await_proofs()));
    let mut changed = view.module().clone();
    let witchy_syntax::ast::Item::Function(root) = &mut changed.items[receipt.root_item() as usize] else { panic!("root") };
    let witchy_syntax::ast::Stmt::Expr(witchy_syntax::ast::Expr::Call { args, .. }) = &mut root.body.stmts[0] else { panic!("root call") };
    args[0] = witchy_syntax::ast::Expr::Tuple(vec![]);
    assert!(!receipt.validate(&changed, view.task_await_proofs()));
    let mut proofs = view.task_await_proofs().to_vec();
    proofs.remove(receipt.covered_awaits()[0] as usize);
    assert!(!receipt.validate(view.module(), &proofs));
}

#[test]
fn closed_scalar_recipe_is_checked_ordinary_source() {

    let checked = super::source::checked_module(SOURCE).expect("checked source kernel");
    let (contract, companion) = witchy_types::pipeline::inspect_closed_scalar_region(&checked)
        .expect("source-addressed closed kernel recipe");
    let mut candidate = checked.module().clone();
    candidate.items.push(witchy_syntax::ast::Item::Function(companion));
    witchy_types::typeck::annotate_checked_source(candidate).expect("ordinary ownership/type checks of generated recipe");
    assert!(contract, "exact linked scheduler bodies");
}

#[test]
fn closed_scalar_refuses_missing_scheduler_projection() {
    let checked = super::source::checked_module(SOURCE).expect("checked source");
    witchy_types::pipeline::assert_closed_scalar_scheduler_rejects_missing_projection(&checked);
}

#[test]
fn closed_scalar_refuses_unsupported_operators_and_endpoint_uses() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let mut cases = vec![
        SOURCE.replace("chan.send(tx, i).await", "chan.send(tx, i).await\n        chan.send(tx, i).await"),
        SOURCE.replace("    let before", "    let scalar_region_payload = 0\n    let before"),
    ];
    for operator in ["/", "%", "<<", ">>"] {
        cases.push(SOURCE.replace("chan.send(tx, i)", &format!("chan.send(tx, i {operator} 1)")));
    }
    for (old, new) in [
        ("var i = 0", "var i = 1 / n"),
        ("while i < n:", "while i / n < n:"),
        ("        chan.send", "        i = i / n\n        chan.send"),
        ("i = i + 1", "i = i / n"),
        ("var sum = 0", "var sum = 1 / 7"),
        ("sum = sum + value", "sum = sum / value"),
        ("${sum}", "${sum / 7}"),
        ("chan.channel(1)", "chan.channel(1 / 7)"),
        ("producer(tx, 7)", "producer(tx, 7 / 7)"),
        ("    let before", "    let scalar = 1 / 7\n    let before"),
        ("duration=${after - before}", "duration=${(after - before) / 7}"),
    ] { cases.push(SOURCE.replace(old, new)); }
    for source in cases {
        let checked = super::source::checked_module(&source).expect("ordinary source remains valid");
        assert!(checked.canonical_source().unwrap().view().scalar_region().is_none(), "{source}");
        assert_eq!(checked.canonical_source().unwrap().view().task_await_proofs().len(),
            if source.matches("chan.send(tx").count() == 2 { 4 } else { 3 });
    }
}
