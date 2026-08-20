use std::collections::HashMap;

use witchy::opt::OptSet;

const MESSAGES: i64 = 1_000_000;
const BASELINE_MESSAGES: i64 = 1_000;
const AGGREGATE_BASELINE_MESSAGES: i64 = 100;
const AGGREGATE_SUSTAINED_MESSAGES: i64 = 10_000;
const WASMTIME_GC_BACKING_BYTES: usize = 65_536;
const ACCEPTED_NS_PER_MESSAGE: f64 = 300.0;
const MAX_LINEAR_ALLOCATIONS: i64 = 100;

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        witchy::opt::set_for_tests(None);
    }
}

fn benchmark_source(messages: i64) -> String {
    include_str!("../benchmarks/chan_throughput.witchy").replace(
        "producer(tx, 64000)",
        &format!("producer(tx, {messages})"),
    )
}

fn aggregate_benchmark_source(messages: i64) -> String {
    format!(
        "mode opt\n\nimport chan\nfrom chan import Sender\n\ntype Packet:\n    Packet(Int, Int)\n\nasync fn producer(tx: Sender(Packet), n: Int) -> Nil:\n    var i = 0\n    while i < n:\n        chan.send(tx, Packet(i, i + 1)).await\n        i = i + 1\n\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(64).await\n    let producer_handle = chan.spawn(producer(tx, {messages})).await\n    var seen = 0\n    var sum = 0\n    while seen < {messages}:\n        let packet = chan.recv(rx).await\n        match packet:\n            Some(Packet(left, right)) ->\n                sum = sum + left + right\n                seen = seen + 1\n            None -> fail(\"channel closed before producer completed\")\n    chan.join(producer_handle).await\n    console.print(\"${{sum}}\")\n"
    )
}

fn assert_scalar_main_bypasses_task_run(wasm: &[u8]) {
    let mut functions = HashMap::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        if let wasmparser::Payload::CustomSection(reader) = payload.expect("valid scalar Wasm")
            && let wasmparser::KnownCustom::Name(section) = reader.as_known()
        {
            for subsection in section {
                match subsection.expect("valid name subsection") {
                    wasmparser::Name::Function(map) => {
                        for naming in map {
                            let naming = naming.expect("valid function name");
                            functions.insert(naming.index, naming.name.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let main = functions.iter().find_map(|(index, name)| (name == "main").then_some(*index))
        .expect("compiled source main function");
    let task_run = functions.iter().find_map(|(index, name)| (name == "task.run").then_some(*index))
        .expect("linked fallback scheduler remains available");
    let mut imported_functions = 0u32;
    let mut function_index = 0u32;
    let mut main_calls = Vec::new();
    for payload in wasmparser::Parser::new(0).parse_all(wasm) {
        match payload.expect("valid scalar Wasm") {
            wasmparser::Payload::ImportSection(reader) => {
                imported_functions = reader.into_imports()
                    .filter_map(Result::ok)
                    .filter(|import| matches!(import.ty, wasmparser::TypeRef::Func(_)))
                    .count() as u32;
                function_index = imported_functions;
            }
            wasmparser::Payload::CodeSectionEntry(body) => {
                if function_index == main {
                    for operator in body.get_operators_reader().expect("main operators") {
                        if let wasmparser::Operator::Call { function_index } =
                            operator.expect("valid main operator")
                        {
                            main_calls.push(function_index);
                        }
                    }
                }
                function_index += 1;
            }
            _ => {}
        }
    }
    assert!(imported_functions > 0, "compiled program imports its Console host");
    assert!(
        !main_calls.contains(&task_run),
        "the measured scalar entry must bypass task.run: {main_calls:?}"
    );
}

fn median_min_max(mut samples: Vec<u128>) -> (u128, u128, u128) {
    samples.sort_unstable();
    (samples[samples.len() / 2], samples[0], samples[samples.len() - 1])
}

/// RFC-0129 acceptance row 4: execute the real producer/consumer benchmark
/// through compiled Wasm's scalar executor, then enforce both the historical
/// per-message latency ceiling and the absence of per-resume linear allocation.
#[test]
fn million_message_scalar_executor_meets_resumption_cost_and_allocation_gate() {
    witchy::opt::set_for_tests(Some(OptSet::all()));
    let _reset_optimization_override = ResetOptimizationOverride;
    let source = benchmark_source(MESSAGES);
    let baseline_source = benchmark_source(BASELINE_MESSAGES);
    let wasm = witchy::compile_source(&source).expect("compile real scalar channel benchmark");
    assert_scalar_main_bypasses_task_run(&wasm);

    let repetitions = std::env::var("WITCHY_RFC0129_REPETITIONS")
        .ok()
        .map(|value| value.parse::<usize>().expect("repetitions must be an integer"))
        .unwrap_or(3);
    assert!(repetitions > 0, "at least one measurement repetition is required");

    let expected = (MESSAGES * (MESSAGES - 1) / 2).to_string();
    let baseline_expected =
        (BASELINE_MESSAGES * (BASELINE_MESSAGES - 1) / 2).to_string();
    let mut execution_times_us = Vec::with_capacity(repetitions);
    let mut allocation_counts = Vec::with_capacity(repetitions);
    for repetition in 1..=repetitions {
        let baseline = witchy::stats::compute_timed(&baseline_source)
            .unwrap_or_else(|error| panic!("execute scalar baseline {repetition}: {error}"));
        assert_eq!(
            baseline.stats.output,
            [baseline_expected.as_str()],
            "baseline fold checksum"
        );
        let measured = witchy::stats::compute_timed(&source)
            .unwrap_or_else(|error| panic!("execute scalar repetition {repetition}: {error}"));
        assert_eq!(measured.stats.output, [expected.as_str()], "observable fold checksum");
        assert_eq!(
            baseline.gc_heap_capacity_bytes, 0,
            "the direct scalar executor must bypass Wasmtime's GC task heap"
        );
        assert_eq!(
            measured.gc_heap_capacity_bytes,
            baseline.gc_heap_capacity_bytes,
            "repetition {repetition}: the direct scalar executor's zero GC capacity must be independent of message count"
        );
        assert!(
            measured.stats.rc_alloc_calls < MAX_LINEAR_ALLOCATIONS,
            "one million scalar messages must not allocate per resume; observed {} linear allocations",
            measured.stats.rc_alloc_calls,
        );
        let latency = measured.execution_time_us as f64 * 1_000.0 / MESSAGES as f64;
        execution_times_us.push(measured.execution_time_us);
        allocation_counts.push(measured.stats.rc_alloc_calls);
        println!(
            "scalar repetition={repetition} execution_us={} ns_per_message={latency:.3} rc_alloc_calls={} gc_heap_capacity_bytes={}",
            measured.execution_time_us,
            measured.stats.rc_alloc_calls,
            measured.gc_heap_capacity_bytes,
        );
    }
    let (median_us, min_us, max_us) = median_min_max(execution_times_us);
    let median = median_us as f64 * 1_000.0 / MESSAGES as f64;
    let min = min_us as f64 * 1_000.0 / MESSAGES as f64;
    let max = max_us as f64 * 1_000.0 / MESSAGES as f64;
    println!(
        "scalar summary messages={MESSAGES} repetitions={repetitions} ns_per_message_median={median:.3} ns_per_message_min={min:.3} ns_per_message_max={max:.3} rc_alloc_calls={allocation_counts:?}"
    );
    assert!(
        median <= ACCEPTED_NS_PER_MESSAGE,
        "RFC-0129 scalar latency gate is {ACCEPTED_NS_PER_MESSAGE} ns/message; measured median {median} ns/message"
    );
}

/// RFC-0129 row 4 GC-backed carrier evidence. A nominal aggregate message
/// rejects scalar executor synthesis, so this matched compiled-Wasm run must
/// exercise the fallback task scheduler without growing Wasmtime's GC backing
/// heap as the message count increases.
#[test]
fn aggregate_channel_gc_heap_capacity_is_flat() {
    witchy::opt::set_for_tests(Some(OptSet::all()));
    let _reset_optimization_override = ResetOptimizationOverride;
    let baseline_source = aggregate_benchmark_source(AGGREGATE_BASELINE_MESSAGES);
    let sustained_source = aggregate_benchmark_source(AGGREGATE_SUSTAINED_MESSAGES);

    let baseline = witchy::stats::compute_timed(&baseline_source)
        .expect("execute aggregate channel baseline");
    let sustained = witchy::stats::compute_timed(&sustained_source)
        .expect("execute sustained aggregate channel run");
    assert_eq!(
        baseline.stats.output,
        [(AGGREGATE_BASELINE_MESSAGES * AGGREGATE_BASELINE_MESSAGES).to_string()],
        "aggregate baseline checksum"
    );
    assert_eq!(
        sustained.stats.output,
        [(AGGREGATE_SUSTAINED_MESSAGES * AGGREGATE_SUSTAINED_MESSAGES).to_string()],
        "aggregate sustained checksum"
    );
    assert_eq!(
        baseline.gc_heap_capacity_bytes, WASMTIME_GC_BACKING_BYTES,
        "aggregate channels must exercise one Wasmtime GC backing page"
    );
    assert_eq!(
        sustained.gc_heap_capacity_bytes,
        baseline.gc_heap_capacity_bytes,
        "Wasmtime GC backing capacity must remain flat across sustained aggregate traffic"
    );
    println!(
        "aggregate channel baseline_messages={} sustained_messages={} baseline_gc_heap_capacity_bytes={} sustained_gc_heap_capacity_bytes={} sustained_execution_us={}",
        AGGREGATE_BASELINE_MESSAGES,
        AGGREGATE_SUSTAINED_MESSAGES,
        baseline.gc_heap_capacity_bytes,
        sustained.gc_heap_capacity_bytes,
        sustained.execution_time_us,
    );
}
