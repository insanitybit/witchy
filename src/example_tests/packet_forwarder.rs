use super::*;

#[test]
fn checked_packet_forwarder_keeps_signed_weighted_fifo_and_completion_clock() {
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); } }
    let _reset = Reset;
    let source = "from chan import Sender, Receiver\n\ntype Envelope:\n    Envelope(Int, Int, Int)\n\nasync fn relay(tx: Sender(Envelope), value: Envelope):\n    chan.send(tx, value).await\n\nasync fn produce(console: Console, tx: Sender(Envelope), n: Int):\n    var i = 0\n    while i < n:\n        relay(tx, Envelope(i * 17 - 11, 31 - i * 3, (i % 5) * 101 - 1000)).await\n        i = i + 1\n    console.print(\"producer finished\")\n\nasync fn consume(console: Console, clock: Clock, rx: Receiver(Envelope), before: Int):\n    var sum = 0\n    var order = 0\n    var next = 0\n    for await value in rx:\n        match value:\n            Envelope(left, middle, right) ->\n                sum = sum + left * 7 + middle * 13 + right * 19\n                order = order + left * (next + 1) + middle * (next + 3) + right * (next + 5)\n                next = next + 1\n    let after = clock.now_monotonic()\n    console.print(\"receiver finished\")\n    console.print(\"${sum}\")\n    console.print(\"${order}\")\n    console.print(\"${next}\")\n    console.print(\"bench_ns=${after - before}\")\n\nasync fn main(console: Console, clock: Clock):\n    let before = clock.now_monotonic()\n    let (tx, rx) = chan.channel(__CAPACITY__).await\n    chan.scope([produce(console, tx, 17), consume(console, clock, rx, before)]).await\n";
    let mut sum = 0;
    let mut order = 0;
    for i in 0..17 {
        let left = i * 17 - 11;
        let middle = 31 - i * 3;
        let right = (i % 5) * 101 - 1000;
        sum += left * 7 + middle * 13 + right * 19;
        order += left * (i + 1) + middle * (i + 3) + right * (i + 5);
    }
    let expected = vec![sum.to_string(), order.to_string(), "17".to_owned()];
    for capacity in [0, 1, 4] {
        let source = source.replace("__CAPACITY__", &capacity.to_string());
      for reversed in [false, true] {
        let source = if reversed {
            source.replace("produce(console, tx, 17), consume(console, clock, rx, before)",
                "consume(console, clock, rx, before), produce(console, tx, 17)")
        } else { source.clone() };
        let mut baseline = None;
        let disabled = witchy_syntax::opt::OptSet::all().without(witchy_syntax::opt::Opt::ClosureElide);
        for options in [witchy_syntax::opt::OptSet::none(), witchy_syntax::opt::OptSet::all(), disabled] {
            witchy_syntax::opt::set_for_tests(Some(options));
            let checked = checked_modules(&[("main", source.as_str())], "main");
            let view = checked.canonical_source().expect("checked source witness").view();
            assert_eq!(view.scalar_region().is_some(), options == witchy_syntax::opt::OptSet::all());
            if let Some(receipt) = view.scalar_region() {
                assert_eq!(receipt.kind(), witchy_canonical_source::scalar_region::RegionKind::ForwardedPacketScope);
                assert_eq!(receipt.covered_awaits().len(), 4);
            }
            let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("admitted checked forwarding packet stream");
            let replay = with_batch_runtime(|runtime| {
                let mut actor = runtime.spawn_admitted(&artifact, full_grants(Vec::new()), 4).expect("spawn forwarding stream");
                let mut offset = 0;
                let mut runs = Vec::new();
                for _ in 0..2 {
                    actor.run().expect("complete or replay forwarding stream");
                    let output = actor.output();
                    let trace = output[offset..].iter().filter(|line| !line.starts_with("bench_ns="))
                        .cloned().collect::<Vec<_>>();
                    assert_eq!(trace.len(), 5);
                    let values = trace.iter().filter(|line| !line.ends_with("finished")).cloned().collect::<Vec<_>>();
                    assert_eq!(values, expected);
                    assert_eq!(&trace[..2], ["producer finished", "receiver finished"]);
                    assert_eq!(trace.iter().filter(|line| line.as_str() == "producer finished").count(), 1);
                    assert_eq!(trace.iter().filter(|line| line.as_str() == "receiver finished").count(), 1);
                    runs.push(trace);
                    let elapsed = output[offset..].iter().find_map(|line| line.strip_prefix("bench_ns=")).expect("completion clock output").parse::<i64>().expect("numeric elapsed clock");
                    assert!(elapsed >= 0);
                    assert_eq!(output.len(), offset + 6);
                    offset += 6;
                }
                assert_eq!(runs[0], runs[1]);
                runs.remove(0)
            });
            if let Some(baseline) = &baseline { assert_eq!(&replay, baseline, "capacity={capacity} reversed={reversed} options={options:?}"); }
            else { baseline = Some(replay); }
        }
      }
    }
}
