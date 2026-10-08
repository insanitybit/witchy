use witchy::opt::OptSet;
use witchy::runtime::{Capabilities, Runtime};

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }

const SOURCE: &str = r#"from chan import Sender, Receiver
async fn produce(console: Console, tx: Sender(Int), count: Int):
    var index: Int = 0
    while index < count:
        chan.send(tx, index + 4294967296).await
        index = index + 1
    console.print("sent=${index}")
async fn consume(console: Console, rx: Receiver(Int)):
    var total: Int = 0
    var digits: Int = 0
    for await value in rx:
        total = total + value
        digits = digits * 10 + value - 4294967296
    console.print("received=${total}:${digits}")
async fn main(console: Console):
    console.print("before")
    let (tx, rx) = chan.channel(1).await
    chan.scope([produce(console, tx, 7), consume(console, rx)]).await
    console.print("joined")
"#;

fn run(source: &str, options: OptSet, selected: bool) -> Vec<String> {
    witchy::opt::set_for_tests(Some(options));
    let checked = witchy::resolve_std_only_checked(source).expect("check closed scalar source");
    assert_eq!(checked.canonical_source().expect("canonical source").view().scalar_region().is_some(), selected);
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("admit scalar source");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut outputs = Vec::new();
    for _ in 0..2 {
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, print_int: true, clock: true, quiet: true, ..Capabilities::none() }, 256)
            .expect("spawn scalar source");
        vm.run().expect("execute scalar source");
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
        let first = vm.output();
        vm.run().expect("replay scalar root on same VM");
        assert_eq!(vm.canonical_heap_leaks().expect("replay heap audit"), Some(0));
        assert_eq!(vm.output(), [first.clone(), first.clone()].concat());
        outputs.push(first);
    }
    assert_eq!(outputs[0], outputs[1]);
    outputs.remove(0)
}

#[test]
fn closed_scalar_scope_keeps_clock_calls_and_scalar_prefix_once() {
    let _reset = Reset;
    let source = SOURCE.replace("var index: Int = 0", "var index: Int = 0\n    var preparations: Int = 0")
        .replace("        chan.send", "        preparations = preparations + 1\n        chan.send")
        .replace("sent=${index}", "sent=${index}:${preparations}")
        .replace("main(console: Console)", "main(console: Console, clock: Clock)")
        .replace("    console.print(\"before\")", "    let before = clock.now_monotonic()\n    console.print(\"before\")")
        .replace("    console.print(\"joined\")", "    let after = clock.now_monotonic()\n    console.print(\"clock=${after >= before}\")\n    console.print(\"joined\")");
    let reference = run(&source, OptSet::none(), false);
    assert!(reference.iter().any(|line| line == "sent=7:7"));
    assert!(reference.iter().any(|line| line == "clock=true"));
    assert_eq!(run(&source, OptSet::all(), true), reference);
}

#[test]
fn closed_scalar_scope_falls_back_for_initial_prefix_payload_and_resume_traps() {
    let _reset = Reset;
    for source in [
        SOURCE.replace("var index: Int = 0", "var index: Int = 1 / (count - 7)"),
        SOURCE.replace("        chan.send", "        index = 1 / (count - 7)\n        chan.send"),
        SOURCE.replace("index + 4294967296", "1 / (count - 7)"),
        SOURCE.replace("index = index + 1", "index = 1 / (count - 7)"),
    ] {
        let mut results = Vec::new();
        for options in [OptSet::none(), OptSet::all()] {
            witchy::opt::set_for_tests(Some(options));
            let checked = witchy::resolve_std_only_checked(&source).expect("checked scalar trap fixture");
            assert!(checked.canonical_source().unwrap().view().scalar_region().is_none(),
                "trapping arithmetic uses ordinary lowering");
            let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("admit scalar trap fixture");
            let mut runtime = Runtime::batch().unwrap();
            let mut vm = runtime.spawn_admitted(&artifact,
                Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() }, 256).unwrap();
            let error = vm.run().expect_err("source division must trap").root_cause().to_string();
            assert!(error.contains("division by zero"), "{error}");
            results.push((vm.output(), error));
        }
        assert_eq!(results[0].0, results[1].0);
    }
}

#[test]
fn closed_scalar_scope_handles_empty_swapped_and_unbounded_rounds() {
    let _reset = Reset;
    for (count, capacity, swapped) in [(0, 1, false), (7, 0, true), (7, 4, false), (7, -1, false)] {
        let mut source = SOURCE.replace("tx, 7)", &format!("tx, {count})"))
            .replace("chan.channel(1)", &format!("chan.channel({capacity})"));
        if swapped {
            source = source.replace(&format!("produce(console, tx, {count}), consume(console, rx)"),
                &format!("consume(console, rx), produce(console, tx, {count})"));
        }
        let reference = run(&source, OptSet::none(), false);
        assert_eq!(run(&source, OptSet::all(), true), reference, "count={count} capacity={capacity} swapped={swapped}");
    }
}

#[test]
fn closed_scalar_scope_preserves_fifo_and_completion_rounds() {
    let _reset = Reset;
    let reference = run(SOURCE, OptSet::none(), false);
    assert!(reference.iter().any(|line| line == "received=30064771093:123456"));
    assert_eq!(run(SOURCE, OptSet::all(), true), reference);
}

#[test]
fn closed_scalar_scope_large_fifo_replays_without_heap_leaks() {
    let _reset = Reset;
    for count in [10_000_i64, 100_000] {
        let source = SOURCE.replace("index + 4294967296", "index")
            .replace("        digits = digits * 10 + value - 4294967296\n", "")
            .replace("tx, 7)", &format!("tx, {count})"));
        let expected = format!("received={}:0", count * (count - 1) / 2);
        let output = run(&source, OptSet::all(), true);
        assert!(output.iter().any(|line| line == &expected), "{output:?}");
    }
}
