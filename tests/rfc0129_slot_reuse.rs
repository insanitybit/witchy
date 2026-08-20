use std::collections::{BTreeMap, HashMap};

fn task_run_gc_ops(source: &str) -> (BTreeMap<u32, usize>, BTreeMap<u32, usize>) {
    let wasm = witchy::compile_source(source).expect("compile RFC-0129 slot-reuse fixture");
    let mut imported_functions = 0u32;
    let mut names = HashMap::new();
    for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
        match payload.expect("valid compiled Wasm") {
            wasmparser::Payload::ImportSection(reader) => {
                imported_functions += reader
                    .into_imports()
                    .filter_map(Result::ok)
                    .filter(|import| matches!(import.ty, wasmparser::TypeRef::Func(_)))
                    .count() as u32;
            }
            wasmparser::Payload::CustomSection(reader) => {
                if let wasmparser::KnownCustom::Name(section) = reader.as_known() {
                    for subsection in section {
                        if let wasmparser::Name::Function(map) =
                            subsection.expect("valid name subsection")
                        {
                            for naming in map {
                                let naming = naming.expect("valid function name");
                                names.insert(naming.index, naming.name.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut function_index = imported_functions;
    let mut slot_news = BTreeMap::new();
    let mut slot_sets = BTreeMap::new();
    let mut found_task_run = false;
    for payload in wasmparser::Parser::new(0).parse_all(&wasm) {
        if let wasmparser::Payload::CodeSectionEntry(body) =
            payload.expect("valid compiled Wasm")
        {
            if names.get(&function_index).is_some_and(|name| name == "task.run") {
                found_task_run = true;
                for operator in body.get_operators_reader().expect("task.run operators") {
                    match operator.expect("valid task.run operator") {
                        wasmparser::Operator::StructNew { struct_type_index } => {
                            *slot_news.entry(struct_type_index).or_insert(0) += 1;
                        }
                        wasmparser::Operator::StructSet { struct_type_index, .. } => {
                            *slot_sets.entry(struct_type_index).or_insert(0) += 1;
                        }
                        _ => {}
                    }
                }
            }
            function_index += 1;
        }
    }
    assert!(found_task_run, "compiled fixture must retain task.run");
    (slot_news, slot_sets)
}

fn assert_task_slots_retag(source: &str, work: i64, label: &str) {
    let measured = witchy::stats::compute_timed(source)
        .unwrap_or_else(|error| panic!("execute {label}: {error}"));
    assert_eq!(measured.stats.output, ["ok"], "{label} checksum");
    let ns_per_unit = measured.execution_time_us * 1_000 / work as u128;
    println!(
        "{label} execution_us={} ns_per_unit={ns_per_unit}",
        measured.execution_time_us,
    );

    let (news, sets) = task_run_gc_ops(source);
    assert!(!sets.is_empty(), "{label}: task.run must retag Slot fields in place");
    for (struct_type, set_count) in &sets {
        let new_count = news.get(struct_type).copied().unwrap_or(0);
        // `task.run` retains one constructor for the root slot and one for
        // slots appended by `Fork`. Scheduler transitions must overwrite an
        // existing slot; the unoptimized lowering adds one allocation site for
        // every `slots.set_at` constructor arm.
        assert!(
            new_count <= 2,
            "{label}: Slot type {struct_type} has {set_count} field overwrite sites but still has {new_count} replacement allocation sites: news={news:?} sets={sets:?}",
        );
    }
}

#[test]
fn bounded_channel_retags_task_slots_without_replacement_allocations() {
    const MESSAGES: i64 = 64;
    let source = format!(
        "mode opt\n\nfrom chan import Sender\n\nasync fn producer(let tx: Sender(Int), n: Int) -> Nil:\n    var i = 0\n    while i < n:\n        chan.send(tx, i).await\n        i = i + 1\n\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(8).await\n    let producer_handle = chan.spawn(producer(tx, {MESSAGES})).await\n    var seen = 0\n    for await value in rx:\n        seen = seen + 1\n        chan.done(value)\n    chan.join(producer_handle).await\n    if seen == {MESSAGES}:\n        console.print(\"ok\")\n    else:\n        console.print(\"bad\")\n"
    );
    assert_task_slots_retag(&source, MESSAGES, "bounded-channel");
}

#[test]
fn simultaneous_fanout_retags_task_slots_without_replacement_allocations() {
    const TASKS: i64 = 24;
    let source = format!(
        "mode opt\n\nimport chan\nimport list\n\nasync fn child(n: Int) -> Nil:\n    chan.yield_now().await\n    if n < 0:\n        fail(\"unreachable\")\n\nasync fn main(console: Console):\n    let items = list.range({TASKS})\n    chan.scope(list.map(items, fn(n): child(n))).await\n    if list.length(items) == {TASKS}:\n        console.print(\"ok\")\n    else:\n        console.print(\"bad\")\n"
    );
    assert_task_slots_retag(&source, TASKS, "simultaneous-fanout");
}
