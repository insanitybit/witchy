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
