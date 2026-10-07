    use witchy_syntax::async_lower::*;
    use witchy_syntax::source_check::AsyncLoweredModule;
    #[allow(unused_imports)]
    use witchy_syntax::ast::*;

    fn lower_module(module: Module) -> Result<AsyncLoweredModule, String> {
        let checked = witchy_syntax::source_check::check(module).map_err(|error| error.to_string())?;
        let checked = witchy_syntax::generators::lower(checked)?;
        lower(checked)
    }

    #[test]
    fn lowering_rejects_value_returning_async_main() {
        let source = "async fn main(console: Console) -> Int:\n    1\n";
        let module = witchy_syntax::parser::parse_module(source).expect("parse async main");
        let error =
            lower_module(module).expect_err("the executor cannot surface an async root value");

        assert!(error.contains("async fn `main` returns `Int`"), "{error}");
        assert!(error.contains("Task(())"), "{error}");
    }

    #[test]
    fn lowering_rejects_a_borrowed_view_live_across_await() {
        let source = "mode opt\n\nfn view(text: &String) -> &String:\n    text\n\nasync fn bad(console: Console):\n    let text = \"x\"\n    let w = view(text)\n    let _ = task.done(0).await\n    console.print(w)\n";
        let module = witchy_syntax::parser::parse_module(source).expect("parse borrowed async body");
        let error = lower_module(module).expect_err("a view cannot be carried through a segment");
        assert!(
            error.contains("borrowed value `w` remains live across `await`"),
            "{error}"
        );
        assert!(
            error.contains("materialize a direct view with `.owned()`"),
            "{error}"
        );
    }

    #[test]
    fn lowering_rejects_lifetime_bearing_nominal_async_signatures() {
        let source = "mode opt\n\ntype Holder('a):\n    view: &'a String\n\nasync fn bad(input: (Holder('a), Int)):\n    task.done(0).await\n";
        let module = witchy_syntax::parser::parse_reference_migration(source).expect("parse borrowed nominal signature");
        let error = lower_module(module)
            .expect_err("a nested lifetime-bearing shell cannot enter an async task");

        assert!(error.contains("lifetime-bearing shell"), "{error}");
        assert!(error.contains("parameter or result"), "{error}");
    }

    #[test]
    fn lowering_rejects_an_unannotated_borrowed_nominal_live_across_await() {
        let source = "mode opt\n\ntype Holder('a):\n    view: &'a String\n\nasync fn bad():\n    let text = \"x\"\n    let holder = Holder(text)\n    let _ = task.done(0).await\n    let keep = holder\n";
        let module = witchy_syntax::parser::parse_reference_migration(source).expect("parse borrowed nominal local");
        let error = lower_module(module)
            .expect_err("a lifetime-bearing constructor result cannot cross suspension");

        assert!(
            error.contains("borrowed value `holder` remains live across `await`"),
            "{error}"
        );
        assert!(error.contains("before building the shell"), "{error}");
    }



#[test]
fn typed_receive_frames_keep_payload_and_reject_suspending_or_root_state() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); }
    }
    let _reset = Reset;
    let source = "from chan import Receiver\n\ntype Packet:\n    Packet(Int, Int)\n\nasync fn consume(rx: Receiver(Packet)):\n    var sum: Int = 0\n    for await packet in rx:\n        match packet:\n            Packet(a, b) -> sum = sum + a + b\n    let _ = sum\n";
    for enabled in [false, true] {
        let options = if enabled { witchy_syntax::opt::OptSet::all() } else { witchy_syntax::opt::OptSet::none() };
        witchy_syntax::opt::set_for_tests(Some(options));
        let module = lower_module(witchy_syntax::parser::parse_module(source).expect("parse receive loop"))
            .expect("lower receive loop");
        let text = format!("{:?}", witchy_syntax::records::lower(module).expect("lower records").into_module());
        assert_eq!(text.contains("chan.recv_typed_frame_loop"), enabled, "{text}");
        if enabled {
            assert!(text.contains("Var(\"packet\")"), "typed payload must enter body: {text}");
        }
    }
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    for unsupported in [
        source.replace("    for await", "    var values: List(Int) = []\n    for await").replace("sum = sum + a + b", "list.push(values, a + b)"),
        source.replace("        match packet:", "        chan.yield_now().await\n        match packet:"),
        source.replace("Packet(a, b) -> sum = sum + a + b", "Packet(a, b) -> return"),
    ] {
        let module = lower_module(witchy_syntax::parser::parse_module(&unsupported).expect("parse fallback"))
            .expect("lower fallback");
        let text = format!("{:?}", witchy_syntax::records::lower(module).expect("lower records").into_module());
        assert!(!text.contains("chan.recv_typed_frame_loop"), "{text}");
    }
}

#[test]
fn typed_receive_frames_keep_nested_escapes_on_the_existing_route() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); }
    }
    let _reset = Reset;
    for escape in ["break", "continue"] {
        let source = format!("from chan import Receiver\n\nasync fn consume(rx: Receiver(Int)):\n    var total: Int = 0\n    for await value in rx:\n        if value == 0:\n            {escape}\n        total = total + value\n    let _ = total\n");
        let mut errors = Vec::new();
        for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
            witchy_syntax::opt::set_for_tests(Some(options));
            match lower_module(witchy_syntax::parser::parse_module(&source).expect("parse escape")) {
                Ok(module) => {
                    let text = format!("{:?}", witchy_syntax::records::lower(module).expect("records").into_module());
                    assert!(!text.contains("chan.recv_typed_frame_loop"));
                    errors.push(None);
                }
                Err(error) => errors.push(Some(error)),
            }
        }
        assert_eq!(errors[0], errors[1], "optimization keeps existing escape behavior");
    }
}

#[test]
fn producer_frames_keep_exact_send_operand_and_resume_identity() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    let source = "from chan import Sender\n\nasync fn produce(tx: Sender(Int), limit: Int):\n    var i: Int = 0\n    while i < limit:\n        chan.send(tx, i + 1).await\n        i = i + 1\n    ()\n";
    for enabled in [false, true] {
        witchy_syntax::opt::set_for_tests(Some(if enabled { witchy_syntax::opt::OptSet::all() }
            else { witchy_syntax::opt::OptSet::none() }));
        let module = lower_module(witchy_syntax::parser::parse_module(source).expect("parse producer"))
            .expect("lower producer");
        let module = witchy_syntax::records::lower(module).expect("lower records").into_module();
        if enabled {
            let mut prepares = 0;
            let mut sends = 0;
            for item in &module.items {
                if let witchy_syntax::ast::Item::Function(function) = item {
                    if witchy_syntax::suspension::source_suspension(function) == Some(0) {
                        let assignment = function.body.stmts.iter().position(|statement|
                            matches!(statement, witchy_syntax::ast::Stmt::Assign { name, .. } if name == "i"))
                            .expect("resume keeps source mutation");
                        assert_eq!(function.body.lines[assignment], 7);
                    }
                    witchy_syntax::ast::visit::visit_block(&function.body, &mut |expression| {
                        if let witchy_syntax::ast::Expr::Call { name, .. } = expression {
                            prepares += usize::from(name == "task.__prepare_send");
                            sends += usize::from(name == "chan.send");
                        }
                        if let witchy_syntax::ast::Expr::Call { name, args } = expression
                            && name == "task.__producer_frame"
                        {
                            for callback in &args[1..3] {
                                let witchy_syntax::ast::Expr::Lambda { params, .. } = callback else {
                                    panic!("producer callbacks are generated lambdas");
                                };
                                assert_eq!(params.len(), 2);
                                assert_eq!(params[0].ty, Some(witchy_syntax::ast::Type::Named("Int".into(), vec![])));
                                assert_eq!(params[1].convention, witchy_syntax::ast::Convention::Var);
                            }
                        }
                        Ok::<_, ()>(())
                    }).unwrap();
                }
            }
            assert_eq!(prepares, 1);
            assert_eq!(sends, 1);
        }
        let text = witchy_syntax::format::module(&module, &[]);
        assert_eq!(text.contains("task.__producer_frame"), enabled, "{text}");
    }
}

#[test]
fn producer_frames_lower_range_prefix_locals() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    for suffix in ["", "        let next: Int = i + 1\n        let _ = next\n"] {
        let source = format!("from chan import Sender\n\nasync fn produce(tx: Sender(Int), limit: Int):\n    for i in 0..limit:\n        chan.send(tx, i).await\n{suffix}    ()\n");
        let module = lower_module(witchy_syntax::parser::parse_module(&source).expect("parse range producer"))
            .expect("lower range producer");
        let text = witchy_syntax::format::module(&witchy_syntax::records::lower(module)
            .expect("lower records").into_module(), &[]);
        assert!(text.contains("task.__producer_frame"), "{text}");
    }
}
