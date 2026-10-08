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
async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    chan.scope([producer(tx, 7), consume(console, rx)]).await
"#;

#[test]
fn authentic_scalar_witness_rejects_invented_source_graph_and_receive_control() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); }
    }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let checked = super::source::checked_module(SOURCE).expect("checked scalar source");
    witchy_types::pipeline::assert_closed_scalar_receipt_rejects_forgery(&checked);
}

#[test]
fn timed_scalar_benchmark_selects_authenticated_region() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { witchy_syntax::opt::set_for_tests(None); }
    }
    let _reset = Reset;
    witchy_syntax::opt::set_for_tests(Some(witchy_syntax::opt::OptSet::all()));
    let source = include_str!("../../../../benchmarks/chan_throughput.witchy")
        .replace("async fn main(console: Console):", "async fn main(console: Console, clock: Clock):\n    let bench_t0 = clock.now_monotonic()");
    let source = format!("{source}    let bench_t1 = clock.now_monotonic()\n    console.print(\"bench_ns=${{bench_t1 - bench_t0}}\")\n");
    for count in [1000, 10000] {
        let source = source.replace("64000", &count.to_string());
        let checked = super::source::checked_module(&source).expect("checked timed scalar benchmark");
        let view = checked.canonical_source().expect("canonical timed benchmark").view();
        let receipt = view.scalar_region().expect("timed benchmark selects scalar region");
        assert!(receipt.validate_checked_source(view.module(), view.task_await_proofs(), view.source_witness()));
    }
}
