use witchy::runtime::{Capabilities, Runtime};
use witchy::opt::{Opt, OptSet};

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) { witchy::opt::set_for_tests(None); }
}

const TRACE: &str = r#"from chan import Receiver, Sender

type Packet:
    Packet(Int, Int)

async fn produce(console: Console, tx: Sender(Packet)):
    var i = 0
    while i < 3:
        chan.send(tx, Packet(i, i + 1)).await
        i = i + 1
    console.print("sent")

async fn consume(console: Console, rx: Receiver(Packet)):
    var total: Int = 0
    var count: Int = 0
    var valid: Bool = true
    for await packet in rx:
        match packet:
            Packet(n, next) ->
                total = total + n
                count = count + 1
                valid = valid && next == n + 1
    console.print("done ${total} ${count} ${valid}")

async fn main(console: Console):
    let (tx, rx) = chan.channel(1).await
    chan.scope([produce(console, tx), consume(console, rx)]).await
    console.print("joined")
"#;

const REPLAY: &str = r#"from chan import Receiver

async fn consume(console: Console, rx: Receiver(Int)):
    var total: Int = 0
    for await value in rx:
        total = total + value
    console.print("sum ${total}")

async fn main(console: Console):
    let (tx, rx) = chan.channel(4).await
    chan.send(tx, 1).await
    chan.send(tx, 2).await
    let recipe = consume(console, rx)
    recipe.await
    recipe.await
"#;

fn run(source: &str, options: OptSet) -> Vec<String> {
    attempt(source, options).expect("execute frame fixture")
}

fn attempt(source: &str, options: OptSet) -> Result<Vec<String>, String> {
    witchy::opt::set_for_tests(Some(options));
    let checked = witchy::resolve_std_only_checked(source).expect("checked async frame fixture");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted async frame fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut outputs = Vec::new();
    for _ in 0..2 {
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() }, 256,
        ).expect("spawn admitted frame fixture");
        vm.run().map_err(|error| error.root_cause().to_string())?;
        assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
        outputs.push(vm.output());
    }
    assert_eq!(outputs[0], outputs[1], "VM-local state is independent for the same artifact");
    Ok(outputs.remove(0))
}

#[test]
fn typed_receive_frame_preserves_within_vm_schedule_and_payloads() {
    let _reset = Reset;
    let reference = run(TRACE, OptSet::none());
    assert_eq!(reference, ["sent", "joined", "done 3 3 true"]);
    for options in [OptSet::all(), OptSet::all().without(Opt::ClosureElide)] {
        assert_eq!(run(TRACE, options), reference);
    }
}

#[test]
fn typed_receive_frame_replay_starts_fresh_invocation_state() {
    let _reset = Reset;
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(REPLAY, options), ["sum 3", "sum 0"]);
    }
}

#[test]
fn typed_receive_frame_returns_values_with_fresh_replay_state() {
    let _reset = Reset;
    let source = REPLAY.replace("async fn consume(console: Console, rx: Receiver(Int)):", "async fn consume(rx: Receiver(Int)) -> Int:")
        .replace("    console.print(\"sum ${total}\")", "    total")
        .replace("consume(console, rx)", "consume(rx)")
        .replace("    recipe.await\n    recipe.await", "    let first = recipe.await\n    let second = recipe.await\n    console.print(\"${first} ${second}\")");
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(&source, options), ["3 0"]);
    }
}

#[test]
fn raw_receive_frame_bridge_is_private() {
    let source = "fn main():\n    let _ = task.channel_receive_frame(0, [], fn(_value, var state): true, fn(_state): task.ready_unit())\n";
    let error = witchy::resolve_std_only_checked(source).expect_err("raw frame bridge is not user authority");
    assert!(error.to_string().contains("channel_receive_frame"), "{error}");
}

const RACE: &str = r#"from chan import Receiver

async fn consume(observe: fn(Int) -> (), rx: Receiver(Int)) -> Int:
    var seen: Int = 0
    for await value in rx:
        observe(value)
        seen = seen + 1
    seen

async fn main(console: Console):
    let (tx, rx) = chan.channel(32).await
    var i = 0
    while i < 32:
        chan.send(tx, i).await
        i = i + 1
    let result = chan.race(consume(fn(value: Int): console.print("body ${value}"), rx), task.done(-1)).await
    console.print("winner ${result}")
    chan.yield_now().await
    console.print("end")
"#;

#[test]
fn typed_receive_frame_keeps_body_effects_and_race_cancellation_order() {
    let _reset = Reset;
    let reference = run(RACE, OptSet::none());
    assert_eq!(&reference[reference.len() - 2..], ["winner Some(-1)", "end"]);
    assert!(reference.iter().filter(|line| line.starts_with("body ")).count() < 32);
    assert_eq!(run(RACE, OptSet::all()), reference);
}

#[test]
fn typed_receive_frame_race_cancels_an_incomplete_frame() {
    let _reset = Reset;
    let source = RACE.replace("observe: fn(Int) -> (), ", "")
        .replace("        observe(value)\n", "")
        .replace("seen = seen + 1", "seen = seen + value + 1")
        .replace("consume(fn(value: Int): console.print(\"body ${value}\"), rx)", "consume(rx)");
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(&source, options), ["winner Some(-1)", "end"]);
    }
}

const MIXED: &str = r#"from chan import Receiver

async fn strings(rx: Receiver(String)) -> Int:
    var total: Int = 0
    for await value in rx:
        let text: String = value
        total = total + text.length()
    total

async fn numbers(rx: Receiver(Int)) -> Int:
    var total: Int = 0
    for await value in rx:
        total = total + value
    total

async fn main(console: Console):
    let (text_tx, text_rx) = chan.channel(4).await
    let (number_tx, number_rx) = chan.channel(4).await
    chan.send(text_tx, "ab").await
    chan.send(number_tx, 9).await
    chan.send(text_tx, "xyz").await
    let text = strings(text_rx).await
    let number = numbers(number_rx).await
    console.print("${text} ${number}")
"#;

#[test]
fn typed_receive_frame_keeps_reference_and_scalar_message_types_separate() {
    let _reset = Reset;
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(MIXED, options), ["5 9"]);
    }
}

#[test]
fn typed_receive_frame_preserves_traps_and_source_location() {
    let _reset = Reset;
    let source = REPLAY.replace("        total = total + value", "        let divisor = value - value\n        total = total + 42 / divisor");
    let reference = attempt(&source, OptSet::none()).expect_err("division must trap");
    assert!(reference.contains("division by zero"), "{reference}");
    assert_eq!(attempt(&source, OptSet::all()).expect_err("frame division must trap"), reference);
}

#[test]
fn typed_receive_frame_selection_is_structural_and_keeps_root_captures_on_fallback() {
    let _reset = Reset;
    for (source, expected) in [(TRACE, true), (REPLAY, true), (MIXED, true), (RACE, false)] {
        for options in [OptSet::none(), OptSet::all()] {
            witchy::opt::set_for_tests(Some(options));
            let selected = selects_typed_frame(source);
            assert_eq!(selected, expected && options == OptSet::all());
        }
    }
}

#[test]
fn typed_receive_frame_keeps_scalar_captures_and_nested_pattern_scopes() {
    let _reset = Reset;
    let source = REPLAY.replace("    var total: Int = 0", "    let offset: Int = 10\n    let enabled: Bool = true\n    var total: Int = 0")
        .replace("        total = total + value", "        match Some(value):\n            Some(total) ->\n                let local: Int = total\n                if enabled:\n                    let _ = local + offset\n            None -> ()\n        total = total + value + offset");
    for options in [OptSet::none(), OptSet::all(), OptSet::none(), OptSet::all()] {
        witchy::opt::set_for_tests(Some(options));
        assert_eq!(selects_typed_frame(&source), options == OptSet::all(), "options {options:?}");
    }
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(&source, options), ["sum 23", "sum 0"]);
    }
}

#[test]
fn typed_receive_frame_cancellation_releases_pending_string_payloads() {
    let _reset = Reset;
    let source = RACE.replace("observe: fn(Int) -> (), ", "")
        .replace("Receiver(Int)", "Receiver(String)")
        .replace("        observe(value)\n", "")
        .replace("seen = seen + 1", "let text: String = value\n        seen = seen + text.length()")
        .replace("chan.send(tx, i)", "chan.send(tx, \"payload\")")
        .replace("consume(fn(value: Int): console.print(\"body ${value}\"), rx)", "consume(rx)");
    for options in [OptSet::none(), OptSet::all()] {
        assert_eq!(run(&source, options), ["winner Some(-1)", "end"]);
    }
}

fn selects_typed_frame(source: &str) -> bool {
    let checked = witchy::resolve_std_only_checked(source).expect("checked selection fixture");
    let mut selected = false;
    witchy_syntax::ast::visit::visit_module_exprs(checked.module(), &mut |expression| {
        if let witchy_syntax::ast::Expr::Call { name, .. } = expression {
            selected |= name == "chan.recv_typed_frame_loop";
        }
        Ok::<_, ()>(())
    }).expect("walk checked calls");
    selected
}

#[test]
fn typed_receive_frame_top_level_carry_shadow_uses_fallback() {
    let _reset = Reset;
    let source = REPLAY.replace("        total = total + value", "        let (total, count) = (value, 1)\n        let _ = total + count");
    for options in [OptSet::none(), OptSet::all()] {
        witchy::opt::set_for_tests(Some(options));
        assert!(!selects_typed_frame(&source));
        assert_eq!(run(&source, options), ["sum 0", "sum 0"]);
    }
}
