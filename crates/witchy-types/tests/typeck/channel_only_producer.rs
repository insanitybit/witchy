use witchy_syntax::ast::{Expr, Item, Stmt};

#[test]
fn checked_producer_test_recipe_keeps_single_task_channel_proof() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    let producer = "async fn test_produce():\n    let (tx, rx) = chan.channel(1).await\n    var i: Int = 0\n    while i < 3:\n        chan.send(tx, i).await\n        i = i + 1\n    let _ = chan.recv(rx).await\n";
    let select = "async fn test_produce():\n    let (tx, rx) = chan.channel(1).await\n    chan.send(tx, 0).await\n    let _ = chan.select(rx, rx).await\n";
    for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(options));
        for (source, expected) in [(producer, "task.__compiler_run_channel_only"), (select, "task.run")] {
            let suite = super::source::checked_test_suite(source).expect("checked original test inventory");
            assert!(suite.checked_module().canonical_source().is_none());
            assert!(suite.checked_module().typed_async_lowering_input()
                .expect("discovery keeps ordinary async facts").static_calls.is_empty());
            let driver = witchy_types::pipeline::check_bare_test_driver(&suite, "main.test_produce",
                witchy_syntax::linker::ComptimeExpander::new(|_, _, _| Ok(witchy_syntax::origin::OriginTable::default())))
                .expect("checked source-derived test recipe");
            let main = driver.module().items.iter().find_map(|item| match item {
                Item::Function(function) if function.name == "main" => Some(function), _ => None,
            }).expect("recipe main");
            assert!(matches!(main.body.stmts.as_slice(), [Stmt::Expr(Expr::Call { name, .. })] if name == expected),
                "expected {expected}, got {:?}; options {options:?}", main.body.stmts);
            assert!(driver.canonical_source().is_some());
        }
    }
}
