//! A resource-bearing success is transferred, not forbidden by the error guard.
//! Both branches are driven on admitted unoptimized Wasm; 7*10+9 = 79.
//! Four independent pairs (including nested Option) concatenate to 79797979.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const FAST: &str = r#"mode opt
must type Ticket:
    Ticket(Int)

pub fn choose(flag: Bool) -> Result(Ticket, String):
    if flag:
        Ok(Ticket(7))
    else:
        Err("retry")

pub fn finish(own ticket: Ticket) -> Int:
    match ticket:
        Ticket(value) -> value
"#;

const MAIN: &str = r#"import fast

must type Local:
    Local(Int)

fn choose(flag: Bool) -> Result(Local, String):
    if flag:
        Ok(Local(7))
    else:
        Err("retry")

fn finish(own value: Local) -> Int:
    match value:
        Local(n) -> n

fn unwrap(own result: Result(Local, String)) -> Local:
    result ?? Local(9)

fn local(flag: Bool) -> Int:
    // Both arms consume an entry parameter, not just a fresh call result.
    let value = unwrap(choose(flag))
    finish(value)

fn cross_mode(flag: Bool) -> Int:
    let value = fast.choose(flag) ?? fast.Ticket(9)
    fast.finish(value)

fn imported_choice(flag: Bool) -> Result(fast.Ticket, String):
    if flag:
        Ok(fast.Ticket(7))
    else:
        Err("retry")

fn imported_type(flag: Bool) -> Int:
    let value = imported_choice(flag) ?? fast.Ticket(9)
    fast.finish(value)

type Box(a):
    Box(a)

fn nested_choice(flag: Bool) -> Option(Box(Local)):
    if flag:
        Some(Box(Local(7)))
    else:
        None

fn nested(flag: Bool) -> Int:
    let box = nested_choice(flag) ?? Box(Local(9))
    match move box:
        Box(ticket) -> finish(ticket)

fn main() -> Int:
    (local(true) * 10 + local(false)) * 1000000 + (cross_mode(true) * 10 + cross_mode(false)) * 10000 + (imported_type(true) * 10 + imported_type(false)) * 100 + nested(true) * 10 + nested(false)
"#;

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

fn assert_wasm(main: &str, expected: &str) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for mode in ["", "mode opt\n"] {
        let main = format!("{mode}{main}");
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), main.into_bytes()), ("fast".into(), FAST.as_bytes().to_vec())],
            "main",
            witchy_interp::comptime::expand_compile_time,
        ).expect("resource success transfer checks");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("unoptimized canonical SafeWIR admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        ).expect("admitted Wasm instance");
        vm.run().expect("admitted Wasm execution");
        assert_eq!(vm.output(), vec![expected], "admitted unoptimized result");
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}

#[test]
fn fallback_transfers_success_resources_in_both_modes_and_across_imports() {
    assert_wasm(MAIN, "79797979");
}

#[test]
fn propagation_transfers_success_and_error_resources() {
    // Success extraction consumes Ticket(7); propagated String selects 9.
    // Scalar success gives 7; propagated Ticket(9) is consumed by the receiver.
    assert_wasm(r#"
must type Ticket:
    Ticket(Int)

fn finish(own ticket: Ticket) -> Int:
    match ticket:
        Ticket(n) -> n

fn success(own value: Result(Ticket, String)) -> Result(Int, String):
    let ticket = value?
    Ok(finish(ticket))

fn error(own value: Result(Int, Ticket)) -> Result(Int, Ticket):
    let n = value?
    Ok(n)

fn receive(own value: Result(Int, Ticket)) -> Int:
    match value:
        Ok(n) -> n
        Err(ticket) -> finish(ticket)

fn main() -> Int:
    (success(Ok(Ticket(7))) ?? 0) * 1000 + (success(Err("retry")) ?? 9) * 100 + receive(error(Ok(7))) * 10 + receive(error(Err(Ticket(9))))
"#, "7979");
}

#[test]
fn conditional_consumption_preserves_cleanup_on_the_other_path() {
    // Extract and discard Some/None: 7. Skip either discardable owner: 4.
    // The false arm must release its original parameter without extracting it.
    assert_wasm(r#"
fn inspect(own value: Option(once fn() -> Int), take: Bool) -> Int:
    if take:
        let callback = (move value) ?? (once fn(): 9)
        let _ = move callback
        7
    else:
        4

fn main() -> Int:
    inspect(Some(once fn(): 7), true) * 1000 + inspect(None, true) * 100 + inspect(Some(once fn(): 7), false) * 10 + inspect(None, false)
"#, "7744");
}

#[test]
fn consuming_fallback_cannot_steal_or_duplicate_an_owner() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let prelude = "must type Ticket:\n    Ticket(Int)\n\nfn finish(own ticket: Ticket) -> Int:\n    match ticket:\n        Ticket(n) -> n\n\n";
    for (body, boundary, expected) in [
        ("fn twice(own value: Result(Ticket, String)) -> Int:\n    let first = value ?? Ticket(9)\n    let second = value ?? Ticket(9)\n    finish(first) + finish(second)\n\nfn main() -> Int:\n    twice(Ok(Ticket(7)))\n", "lowering", "OwnedReleasePlacement(\"MissingToken("),
        ("fn steal(let value: Result(Ticket, String)) -> Ticket:\n    value ?? Ticket(9)\n\nfn main() -> Int:\n    let value: Result(Ticket, String) = Ok(Ticket(7))\n    let stolen = steal(value)\n    let score = finish(stolen)\n    match move value:\n        Ok(ticket) -> score + finish(ticket)\n        Err(_) -> score\n", "source", "cannot consume borrowed must-consume value `value`"),
    ] {
      for mode in ["", "mode opt\n"] {
        let source = format!("{mode}{prelude}{body}");
        let (actual_boundary, refusal) = match witchy::resolve_std_only_checked(&source) {
            Err(error) => ("source", error.to_string()),
            Ok(checked) => match witchy::codegen::compile_checked_module_artifact(&checked) {
                witchy::codegen::LoweringOutcome::Lowered(_) => panic!("consuming fallback admitted stolen or duplicated ownership"),
                witchy::codegen::LoweringOutcome::Unsupported(error) => ("lowering", error.to_string()),
                witchy::codegen::LoweringOutcome::Rejected(error) => ("admission", error.to_string()),
            },
        };
        // Earlier source ownership checking is an improvement, not a regression.
        // Do not accept unrelated parsing, typing, or layout failures instead.
        let earlier_transfer_refusal = boundary == "lowering"
            && actual_boundary == "source"
            && refusal.contains("value")
            && refusal.contains("after")
            && ["transferred", "consumed", "used", "moved"]
                .iter().any(|message| refusal.contains(message));
        assert!(earlier_transfer_refusal || refusal.contains(expected), "{actual_boundary}: {refusal}");
      }
    }
}
