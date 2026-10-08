use witchy::opt::{Opt, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct Reset;
impl Drop for Reset { fn drop(&mut self) { witchy::opt::set_for_tests(None); } }

const TRACE: &str = r#"from chan import Sender, Receiver

async fn produce(emit: fn(String) -> (), completed: fn() -> (), tx: Sender(Int)):
    var i: Int = 0
    while i < 3:
        emit("before ${i}")
        chan.send(tx, i).await
        emit("after ${i}")
        i = i + 1
    completed()

async fn consume(emit: fn(String) -> (), rx: Receiver(Int)):
    for await value in rx:
        emit("value ${value}")

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    let emit = fn(text: String): console.print(text)
    let completed = fn(): console.print("sent")
    chan.scope([produce(emit, completed, tx), consume(emit, rx)]).await
    console.print("joined")
"#;

fn run(source: &str, options: OptSet) -> Vec<String> {
    attempt(source, options).expect("execute producer frame fixture")
}

fn attempt(source: &str, options: OptSet) -> Result<Vec<String>, String> {
    witchy::opt::set_for_tests(Some(options));
    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("check producer frame fixture with {options:?}: {error}"));
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit producer frame fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() }, 256)
        .expect("spawn admitted producer frame fixture");
    vm.run().map_err(|error| error.root_cause().to_string())?;
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
    Ok(vm.output())
}

#[test]
fn producer_frame_keeps_prefix_suffix_and_probe_rounds() {
    let _reset = Reset;
    for capacity in [0, 1, 3] {
        let source = TRACE.replace("chan.channel(1)", &format!("chan.channel({capacity})"));
        let reference = run(&source, OptSet::none());
        let values = reference.iter().filter(|line| line.starts_with("value ")).cloned().collect::<Vec<_>>();
        assert_eq!(values, ["value 0", "value 1", "value 2"]);
        assert_eq!(run(&source, OptSet::all()), reference);
        assert_eq!(run(&source, OptSet::all().without(Opt::ClosureElide)), reference);
    }
}

#[test]
fn producer_frame_empty_loop_preserves_initial_completion_round() {
    let _reset = Reset;
    let source = TRACE.replace("i < 3", "i < 0");
    let reference = run(&source, OptSet::none());
    assert!(reference.contains(&"sent".to_string()));
    assert!(!reference.iter().any(|line| line.starts_with("value ")));
    assert_eq!(run(&source, OptSet::all()), reference);
}

#[test]
fn producer_frame_replay_has_independent_carried_state() {
    let _reset = Reset;
    let source = r#"from chan import Sender

async fn produce(tx: Sender(Int)):
    var i: Int = 0
    while i < 2:
        chan.send(tx, i).await
        i = i + 1
    ()

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let recipe = produce(tx)
    recipe.await
    recipe.await
    for await value in rx:
        console.print("${value}")
"#;
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(source, options), ["0", "1", "0", "1"]);
    }
}

#[test]
fn producer_frame_preserves_nominal_payload_and_bool_carry() {
    let _reset = Reset;
    let source = TRACE.replace("from chan import Sender, Receiver", "from chan import Sender, Receiver\n\ntype Packet:\n    Packet(Int, Bool)")
        .replace("Sender(Int)", "Sender(Packet)").replace("Receiver(Int)", "Receiver(Packet)")
        .replace("    var i: Int = 0", "    var i: Int = 0\n    var valid: Bool = false")
        .replace("chan.send(tx, i)", "chan.send(tx, Packet(i, valid))")
        .replace("        i = i + 1", "        i = i + 1\n        valid = !valid")
        .replace("        emit(\"value ${value}\")", "        match value:\n            Packet(i, valid) -> emit(\"value ${i} ${valid}\")");
    let reference = run(&source, OptSet::none());
    let values = reference.iter().filter(|line| line.starts_with("value ")).cloned().collect::<Vec<_>>();
    assert_eq!(values, ["value 0 false", "value 1 true", "value 2 false"]);
    assert_eq!(run(&source, OptSet::all()), reference);
}

#[test]
fn producer_frame_cancellation_releases_once_prepared_string_payload() {
    let _reset = Reset;
    let source = r#"from chan import Sender
import task

async fn produce(emit: fn(Int) -> (), tx: Sender(String)) -> Int:
    var i: Int = 0
    while i < 5:
        emit(i)
        chan.send(tx, "payload ${i}").await
        i = i + 1
    9

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    chan.send(tx, "seed").await
    let result = chan.race(produce(fn(value: Int): console.print("before ${value}"), tx), task.done(-1)).await
    console.print("winner ${result}")
    for await value in rx:
        console.print(value)
"#;
    let reference = run(source, OptSet::none());
    assert_eq!(reference, ["before 0", "winner Some(-1)", "seed"]);
    assert_eq!(run(source, OptSet::all()), reference);
}

#[test]
fn producer_frame_offsets_isolate_interleaved_scalar_lanes() {
    let _reset = Reset;
    let source = r#"from chan import Sender

async fn produce(tx: Sender(Int), start: Int):
    var i: Int = start
    var positive: Bool = true
    while i < start + 2:
        chan.send(tx, if positive: i else: -i).await
        i = i + 1
        positive = !positive
    ()

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.scope([produce(tx, 10), produce(tx, 20)]).await
    for await value in rx:
        console.print("${value}")
"#;
    let reference = run(source, OptSet::none());
    let mut values = reference.clone();
    values.sort();
    assert_eq!(values, ["-11", "-21", "10", "20"]);
    assert_eq!(run(source, OptSet::all()), reference);
}

#[test]
fn producer_frame_keeps_payload_and_resume_trap_locations() {
    let _reset = Reset;
    for source in [
        TRACE.replace("chan.send(tx, i)", "chan.send(tx, 42 / (i - i))"),
        TRACE.replace("        i = i + 1", "        i = 42 / (i - i)"),
    ] {
        let reference = attempt(&source, OptSet::none()).expect_err("division must trap");
        assert!(reference.contains("division by zero"), "{reference}");
        assert_eq!(attempt(&source, OptSet::all()).expect_err("frame division must trap"), reference);
    }
}

#[test]
fn producer_frame_range_prefix_locals_have_correct_liveness() {
    let _reset = Reset;
    for suffix in ["", "        emit(\"after ${i}\")\n"] {
        let source = TRACE.replace("    var i: Int = 0\n    while i < 3:\n", "    for i in 0..3:\n")
            .replace("        emit(\"after ${i}\")\n        i = i + 1\n", suffix);
        let reference = run(&source, OptSet::none());
        let values = reference.iter().filter(|line| line.starts_with("value ")).cloned().collect::<Vec<_>>();
        assert_eq!(values, ["value 0", "value 1", "value 2"]);
        assert_eq!(run(&source, OptSet::all()), reference);
    }
}

#[test]
fn producer_frame_spilled_range_binding_does_not_escape_loop_scope() {
    let _reset = Reset;
    let source = r#"from chan import Sender

async fn produce(tx: Sender(Int), i: Int) -> Int:
    for i in 0..3:
        chan.send(tx, i).await
        let next: Int = i + 1
        let _ = next
    i

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let result = produce(tx, 99).await
    console.print("result ${result}")
    for await value in rx:
        console.print("${value}")
"#;
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(source, options), ["result 99", "0", "1", "2"]);
    }
}

#[test]
fn producer_frame_bare_test_recipe_uses_channel_only_scheduler() {
    let temp = super::temp_dir::TempDir::new("producer-frame-test-driver");
    let file = temp.write("suite.witchy", r#"import testing

async fn test_producer_frame():
    let (tx, rx) = chan.channel(4).await
    var i: Int = 0
    while i < 3:
        chan.send(tx, i).await
        i = i + 1
    let first = chan.recv(rx).await
    let second = chan.recv(rx).await
    let third = chan.recv(rx).await
    match first:
        Some(value) -> testing.assert(value == 0, "first message")
        None -> testing.assert(false, "missing first message")
    match second:
        Some(value) -> testing.assert(value == 1, "second message")
        None -> testing.assert(false, "missing second message")
    match third:
        Some(value) -> testing.assert(value == 2, "third message")
        None -> testing.assert(false, "missing third message")
"#);
    for options in ["none", "all"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_witchy"))
            .env("WITCHY_OPT", options)
            .arg("test").arg(&file).output().expect("execute real test driver");
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{text}");
        assert!(text.contains("1 passed"), "{text}");
    }
}

#[test]
fn raw_producer_preparation_is_not_source_authority() {
    for source in [
        "import task\n\nfn main():\n    let _ = task.__prepare_send(task.ready_unit())\n",
        "import task\n\nfn main():\n    let _ = task.__producer_frame([], fn(var state): None, fn(var state): false, fn(state): task.ready_unit())\n",
    ] {
        let error = witchy::resolve_std_only_checked(source).expect_err("private producer bridge");
        assert!(error.to_string().contains("private"), "{error}");
    }
}

#[test]
fn producer_frame_preserves_false_condition_carry_writeback() {
    let _reset = Reset;
    let source = r#"from chan import Sender

fn advance(var value: Int) -> Bool:
    value = value + 1
    value < 3

async fn produce(tx: Sender(Int)) -> Int:
    var i: Int = 0
    while advance(i):
        chan.send(tx, i).await
    i

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    let result = produce(tx).await
    console.print("result ${result}")
    for await value in rx:
        console.print("${value}")
"#;
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(source, options), ["result 3", "1", "2"]);
    }
}

#[test]
fn producer_channel_only_cli_preserves_full_queue_effect_order_and_empty_finish() {
    let temp = super::temp_dir::TempDir::new("producer-channel-only-effects");
    let file = temp.write("suite.witchy", r#"import testing

fn payload(var trace: Int, value: Int) -> Int:
    trace = trace * 10 + 2
    value

async fn test_full_queue():
    let (tx, rx) = chan.channel(1).await
    var i: Int = 0
    var trace: Int = 0
    while i < 3:
        trace = trace * 10 + 1
        chan.send(tx, payload(trace, i)).await
        trace = trace * 10 + 3
        i = i + 1
    testing.assert(trace == 123123123, "prefix payload resume order")
    var expected: Int = 0
    while expected < 3:
        let result = chan.recv(rx).await
        match result:
            Some(value) -> testing.assert(value == expected, "bounded full queue FIFO")
            None -> testing.assert(false, "missing queued message")
        expected = expected + 1

async fn test_empty_finish():
    let (tx, rx) = chan.channel(1).await
    var i: Int = 0
    var visited: Int = 0
    while i < 0:
        visited = visited + 1
        chan.send(tx, i).await
        visited = visited + 10
    testing.assert(visited == 0, "empty body effects")
    testing.assert(i == 0, "empty completion carry")
    let empty = chan.recv(rx).await
    match empty:
        None -> ()
        Some(_) -> testing.assert(false, "unexpected empty message")

async fn test_string_cleanup():
    let (tx, rx) = chan.channel(1).await
    var i: Int = 0
    while i < 3:
        chan.send(tx, "owned ${i}").await
        i = i + 1
    var received: Int = 0
    while received < 3:
        let value = chan.recv(rx).await
        match value:
            Some(text) -> testing.assert(text == "owned ${received}", "string FIFO")
            None -> testing.assert(false, "missing owned message")
        received = received + 1
"#);
    for options in ["none", "all"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_witchy"))
            .env("WITCHY_OPT", options).arg("test").arg(&file).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{options}: {text}");
        assert!(text.contains("3 passed"), "{text}");
    }
}

#[test]
fn producer_channel_only_cli_preserves_prefix_payload_and_resume_traps() {
    let temp = super::temp_dir::TempDir::new("producer-channel-only-traps");
    for (stage, prefix, payload, suffix) in [
        ("prefix", "let trapped = 1 / (i - i)\n        let _ = trapped", "0", "()"),
        ("payload", "()", "1 / (i - i)", "()"),
        ("resume", "()", "0", "let trapped = 1 / (i - i)\n        let _ = trapped"),
    ] {
        let source = format!("async fn test_trap():\n    let (tx, rx) = chan.channel(1).await\n    var i: Int = 0\n    while i < 3:\n        {prefix}\n        chan.send(tx, {payload}).await\n        {suffix}\n        i = i + 1\n");
        let file = temp.write("suite.witchy", &source);
        for options in ["none", "all"] {
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_witchy"))
                .env("WITCHY_OPT", options).arg("test").arg(&file).output().unwrap();
            let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert!(!output.status.success(), "trap must fail test");
            assert!(text.contains("division by zero"), "{options}/{stage}: {text}");
            assert!(text.contains("1 failed"), "{text}");
        }
    }
}

#[test]
fn producer_channel_only_cli_replays_existing_public_executor_task() {
    let temp = super::temp_dir::TempDir::new("producer-channel-only-replay");
    let file = temp.write("suite.witchy", r#"import task
import testing

async fn recipe():
    let (tx, rx) = chan.channel(1).await
    var i: Int = 0
    while i < 3:
        chan.send(tx, i).await
        i = i + 1
    var expected: Int = 0
    while expected < 3:
        let result = chan.recv(rx).await
        match result:
            Some(value) -> testing.assert(value == expected, "fresh replay carry and channel")
            None -> testing.assert(false, "missing replay message")
        expected = expected + 1

fn test_replay():
    let reusable = recipe()
    task.__compiler_run_channel_only(reusable)
    task.__compiler_run_channel_only(reusable)
"#);
    for options in ["none", "all"] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_witchy"))
            .env("WITCHY_OPT", options).arg("test").arg(&file).output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert!(output.status.success(), "{options}: {text}");
        assert!(text.contains("1 passed"), "{text}");
    }
}

#[test]
fn producer_channel_only_cli_rejects_unproved_scheduler_effect() {
    let temp = super::temp_dir::TempDir::new("producer-channel-only-effect-refusal");
    let file = temp.write("suite.witchy", "import task\n\nasync fn child():\n    ()\n\nfn test_effect():\n    task.__compiler_run_channel_only(chan.scope([child()]))\n");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_witchy"))
        .env("WITCHY_OPT", "all").arg("test").arg(&file).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(!output.status.success());
    assert!(text.contains("compiler-proven channel-only Task produced another scheduler effect"), "{text}");
    assert!(text.contains("1 failed"), "{text}");
}

#[test]
fn producer_frame_callback_snapshot_preserves_caller_and_callee_aliases() {
    let _reset = Reset;
    let source = "fn invoke(f: fn(var List(Int)) -> (), var values: List(Int)):\n    f(values)\n\nfn main(console: Console):\n    var values = [1]\n    let before = values\n    let update = fn(var values: List(Int)):\n        let snapshot = values\n        values[0] = values[0] + 1\n        console.print(\"${snapshot[0]}\")\n    invoke(update, values)\n    console.print(\"${before[0]} ${values[0]}\")\n";
    for options in [OptSet::none(), OptSet::all(), OptSet::all().without(Opt::ClosureElide)] {
        assert_eq!(run(source, options), ["1", "1 2"]);
    }
}
