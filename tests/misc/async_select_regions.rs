use witchy::opt::OptSet;
use witchy::runtime::{Capabilities, Runtime};

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }

fn source(count: i64, capacity: i64, swapped: bool) -> String {
    let mut source = include_str!("../../benchmarks/select_fanin.witchy")
        .replace("2048", &count.to_string()).replace("4096", &(count * 2).to_string())
        .replace(&format!("chan.channel({count})"), &format!("chan.channel({capacity})"))
        .replace("bench_ns=${bench_t1 - bench_t0}", "clock=${bench_t1 >= bench_t0}")
        .replace("produce(tx: Sender(Int)", "produce(console: Console, tx: Sender(Int)")
        .replace("        i = i + 1\n\nfn expected", "        i = i + 1\n    console.print(\"sent=${i}\")\n\nfn expected")
        .replace("produce(a_tx,", "produce(console, a_tx,").replace("produce(b_tx,", "produce(console, b_tx,")
        .replace("    let bench_t0", "    console.print(\"drain\")\n    let bench_t0");
    if swapped { source = source.replace("chan.select(a_rx, b_rx)", "chan.select(b_rx, a_rx)"); }
    source
}
fn run(source: &str, options: OptSet, selected: bool) -> Vec<String> {
    witchy::opt::set_for_tests(Some(options));
    let checked = witchy::resolve_std_only_checked(source).expect("check select region");
    let view = checked.canonical_source().unwrap().view();
    assert_eq!(view.scalar_region().is_some(), selected);
    if let Some(receipt) = view.scalar_region() {
        assert_eq!(receipt.kind(), witchy_canonical_source::scalar_region::RegionKind::SelectDrain);
        assert!(receipt.validate_checked_source(view.module(), view.task_await_proofs(), view.source_witness()));
    }
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked).expect_lowered("admit select region");
    let mut runtime = Runtime::batch().unwrap();
    let mut outputs = Vec::new();
    for _ in 0..2 {
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, print_int: true, clock: true, quiet: true, ..Capabilities::none() }, 256).unwrap();
        vm.run().expect("execute select region");
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
        let first = vm.output();
        vm.run().expect("replay select region");
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
        assert_eq!(vm.output(), [first.clone(), first.clone()].concat());
        outputs.push(first);
    }
    assert_eq!(outputs[0], outputs[1]); outputs.remove(0)
}

#[test]
fn closed_select_region_preserves_fifo_source_priority_and_scope_rounds() {
    let _reset = Reset;
    let source = source(7, 7, false);
    let reference = run(&source, OptSet::none(), false);
    assert!(reference.iter().any(|line| line.contains("order=ok")));
    assert!(reference.iter().any(|line| line == "clock=true"));
    assert_eq!(run(&source, OptSet::all(), true), reference);
}

#[test]
fn closed_select_region_preserves_full_quiescent_empty_and_swapped_rounds() {
    let _reset = Reset;
    for (count, capacity, swapped) in [(7, 1, false), (7, 0, true), (0, 1, false), (7, -1, false)] {
        let source = source(count, capacity, swapped);
        let reference = run(&source, OptSet::none(), false);
        assert_eq!(run(&source, OptSet::all(), true), reference, "count={count}, capacity={capacity}, swapped={swapped}");
    }
}

#[test]
fn closed_select_region_covers_distinct_producer_declarations() {
    let _reset = Reset;
    let mut source = source(7, 3, false);
    let start = source.find("async fn produce(").unwrap();
    let end = source.find("fn expected_checksum").unwrap();
    let second = source[start..end].replace("async fn produce(", "async fn produce_other(");
    source.insert_str(end, &second);
    source = source.replace("produce(console, b_tx,", "produce_other(console, b_tx,");
    witchy::opt::set_for_tests(Some(OptSet::all()));
    let checked = witchy::resolve_std_only_checked(&source).expect("distinct producer source");
    assert_eq!(checked.canonical_source().unwrap().view().scalar_region().unwrap().covered_awaits().len(), 6);
    let reference = run(&source, OptSet::none(), false);
    assert_eq!(run(&source, OptSet::all(), true), reference);
}
