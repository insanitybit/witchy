//! `?` must transfer a nested resource on either selected Result branch.
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const WORKER: &str = r#"mode opt
must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)

pub fn choose(ok: Bool) -> Result(Box(Ticket), Box(Ticket)):
    if ok: Ok(Box(Ticket(7))) else: Err(Box(Ticket(9)))

pub fn finish(own box: Box(Ticket)) -> Int:
    match move box:
        Box(ticket) -> match move ticket:
            Ticket(n) -> n

pub fn relay(own value: Result(Box(Ticket), Box(Ticket))) -> Result(Int, Box(Ticket)):
    let box = (move value)?
    Ok(finish(box))
"#;

struct ResetOptimization;
impl Drop for ResetOptimization {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn nested_resource_try_transfers_both_branches_in_four_contexts() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimization;
    for (mode, route) in [
        ("", "local"),
        ("mode opt\n", "local"),
        ("", "cross"),
        ("", "imported"),
    ] {
        let functions = match route {
            "cross" => String::new(),
            "imported" => r#"
fn relay(own value: Result(worker.Box(worker.Ticket), worker.Box(worker.Ticket))) -> Result(Int, worker.Box(worker.Ticket)):
    let box = (move value)?
    Ok(worker.finish(box))
"#.to_owned(),
            _ => r#"
must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)
fn choose(ok: Bool) -> Result(Box(Ticket), Box(Ticket)):
    if ok: Ok(Box(Ticket(7))) else: Err(Box(Ticket(9)))
fn finish(own box: Box(Ticket)) -> Int:
    match move box:
        Box(ticket) -> match move ticket:
            Ticket(n) -> n
fn relay(own value: Result(Box(Ticket), Box(Ticket))) -> Result(Int, Box(Ticket)):
    let box = (move value)?
    Ok(finish(box))
"#.to_owned(),
        };
        let (choose, relay, finish) = match route {
            "cross" => ("worker.choose", "worker.relay", "worker.finish"),
            "imported" => ("worker.choose", "relay", "worker.finish"),
            _ => ("choose", "relay", "finish"),
        };
        let source = format!(r#"{mode}import worker
{functions}
fn run(ok: Bool) -> Int:
    match {relay}({choose}(ok)):
        Ok(n) -> n
        Err(box) -> {finish}(box)
fn main() -> Int:
    run(true) * 10 + run(false)
"#);
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), source.into_bytes()), ("worker".into(), WORKER.as_bytes().to_vec())],
            "main", witchy_interp::comptime::EXPANDER,
        ).unwrap_or_else(|error| panic!("{route} source refused: {error}"));
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("nested affine try admits unoptimized SafeWIR");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), vec!["79"], "{route}");
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0), "{route}");
    }
}

#[test]
fn early_error_cannot_abandon_an_unrelated_live_resource() {
    for (mode, ticket_type, constructor, finisher, in_worker) in [
        ("", "Ticket", "Ticket", "finish", false),
        ("mode opt\n", "Ticket", "Ticket", "finish", false),
        ("", "worker.Ticket", "worker.Ticket", "worker.finish", false),
        ("", "Ticket", "Ticket", "finish", true),
    ] {
        let local = if ticket_type == "Ticket" {
            r#"must type Ticket:
    Ticket(Int)
fn finish(own ticket: Ticket) -> Int:
    match move ticket:
        Ticket(n) -> n
"#
        } else { "" };
        let body = format!(r#"fn maybe(bad: Bool) -> Result(Int, String):
    if bad: Err("stop") else: Ok(9)
fn early(bad: Bool) -> Result(Int, String):
    let token: {ticket_type} = {constructor}(7)
    let n = maybe(bad)?
    Ok({finisher}(token) + n)
"#);
        let (main, worker) = if in_worker {
            ("import worker\nfn main() -> Int:\n    worker.early(true) ?? 0\n".to_owned(),
                format!("{WORKER}\n{}", body.replace("fn early(", "pub fn early(")))
        } else {
            (format!("{mode}import worker\n{local}{body}\nfn main() -> Int:\n    0\n"), WORKER.to_owned())
        };
        let outcome = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), main.into_bytes()), ("worker".into(), worker.into_bytes())],
            "main", witchy_interp::comptime::EXPANDER,
        );
        let message = match outcome {
            Ok(_) => panic!("source accepted a live Ticket across the `?` failure exit"),
            Err(error) => error.to_string(),
        };
        assert!(message.contains("return leaves must-consume value `token` undisposed"), "{message}");
    }
}
