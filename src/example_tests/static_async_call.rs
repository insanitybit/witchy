use super::*;

#[test]
fn static_async_call_frames_keep_lazy_source_effect_boundaries() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); }
    }
    let _reset = Reset;
    let source = "from chan import Sender, Receiver\n\ntype Packet:\n    Packet(Int, Int, Int)\n\nfn argument(console: Console, value: Int) -> Int:\n    console.print(\"argument ${value}\")\n    value\n\nasync fn send_one(console: Console, tx: Sender(Packet), packet: Packet) -> ():\n    console.print(\"callee poll\")\n    chan.send(tx, packet).await\n    console.print(\"callee return\")\n\nasync fn produce(console: Console, tx: Sender(Packet)):\n    console.print(\"caller poll\")\n    let child = send_one(console, tx, Packet(argument(console, 1), argument(console, 2), 3))\n    console.print(\"after construction\")\n    child.await\n    console.print(\"caller return\")\n\nasync fn consume(console: Console, rx: Receiver(Packet)):\n    let packet = chan.recv(rx).await\n    console.print(\"received ${packet}\")\n\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(0).await\n    chan.scope([produce(console, tx), consume(console, rx)]).await\n";
    for (source, has_suffix) in [(source.to_owned(), true), (source.replace("    console.print(\"callee return\")\n", ""), false)] {
      let prefix = ["caller poll", "argument 1", "argument 2", "after construction", "callee poll"];
      let mut baseline = None;
      for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(options));
        let checked = checked_modules(&[("main", source.as_str())], "main");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admitted static call frame trace");
        let output = run_admitted_with(&artifact, full_grants(Vec::new()), 4);
        assert_eq!(&output[..prefix.len()], prefix);
        assert_eq!(output.len(), if has_suffix { 8 } else { 7 });
        for expected in ["received Some(Packet(1, 2, 3))", "caller return"] {
            assert_eq!(output.iter().filter(|line| line.as_str() == expected).count(), 1);
        }
        if has_suffix {
            let callee_return = output.iter().position(|line| line == "callee return").expect("callee completes");
            let caller_return = output.iter().position(|line| line == "caller return").expect("caller completes");
            assert!(callee_return < caller_return);
        }
        if let Some(baseline) = baseline.as_ref() { assert_eq!(&output, baseline); }
        else { baseline = Some(output); }
      }
    }
}

#[test]
fn static_async_call_loop_keeps_source_rounds_and_same_vm_replay() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    let source = "from chan import Sender, Receiver\n\ntype Packet:\n    Packet(Int, Int, Int)\n\nfn argument(console: Console, value: Int) -> Int:\n    console.print(\"arg ${value}\")\n    value\n\nasync fn send_one(console: Console, tx: Sender(Packet), packet: Packet):\n    console.print(\"callee poll\")\n    chan.send(tx, packet).await\n\nasync fn produce(console: Console, tx: Sender(Packet)):\n    var i = 0\n    while i < 2:\n        console.print(\"caller ${i}\")\n        send_one(console, tx, Packet(argument(console, i), argument(console, i + 1), i + 2)).await\n        console.print(\"returned ${i}\")\n        i = i + 1\n\nasync fn consume(console: Console, rx: Receiver(Packet)):\n    var i = 0\n    while i < 2:\n        let packet = chan.recv(rx).await\n        console.print(\"received ${packet}\")\n        i = i + 1\n\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(__CAPACITY__).await\n    chan.scope([produce(console, tx), consume(console, rx)]).await\n";
    for capacity in [0, 1, 4] {
        let source = source.replace("__CAPACITY__", &capacity.to_string());
        let mut baseline = None;
        for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
            witchy_syntax::opt::set_for_tests(Some(options));
            let checked = checked_modules(&[("main", source.as_str())], "main");
            let artifact = codegen::compile_checked_module_artifact(&checked)
                .expect_lowered("admitted reusable static caller frame");
            let output = with_batch_runtime(|runtime| {
                let mut actor = runtime.spawn_admitted(&artifact, full_grants(Vec::new()), 4).expect("spawn source caller");
                actor.run().expect("first source caller run");
                let first = actor.output();
                actor.run().expect("same VM source caller replay");
                let repeated = actor.output();
                assert_eq!(&repeated[first.len()..], first);
                first
            });
            assert_eq!(output.len(), 12);
            assert_eq!(output.iter().filter(|line| line.as_str() == "callee poll").count(), 2);
            for expected in ["returned 0", "returned 1", "received Some(Packet(0, 1, 2))", "received Some(Packet(1, 2, 3))"] {
                assert_eq!(output.iter().filter(|line| line.as_str() == expected).count(), 1);
            }
            if let Some(baseline) = baseline.as_ref() { assert_eq!(&output, baseline); }
            else { baseline = Some(output); }
        }
    }
}

#[test]
fn static_async_call_integer_helper_loop_completes() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    let source = "from chan import Sender, Receiver\n\nasync fn publish_number(tx: Sender(Int), value: Int):\n    chan.send(tx, value).await\n\nasync fn produce(tx: Sender(Int)):\n    var i = 0\n    while i < 2:\n        publish_number(tx, i).await\n        i = i + 1\n\nasync fn consume(console: Console, rx: Receiver(Int)):\n    let first = chan.recv(rx).await\n    console.print(\"${first}\")\n    let second = chan.recv(rx).await\n    console.print(\"${second}\")\n\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(1).await\n    chan.scope([produce(tx), consume(console, rx)]).await\n";
    for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all()] {
        witchy_syntax::opt::set_for_tests(Some(options));
        let checked = checked_modules(&[("main", source)], "main");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("admitted integer helper caller frame");
        assert_eq!(run_admitted_with(&artifact, full_grants(Vec::new()), 4), ["Some(0)", "Some(1)"]);
    }
}
