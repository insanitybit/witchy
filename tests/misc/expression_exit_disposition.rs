//! A resource is created only after a potentially returning operand completes.
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

#[test]
fn trailing_spread_resource_creation_follows_the_written_override() {
    opt::set_for_tests(Some(OptSet::none()));
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { opt::set_for_tests(None); } }
    let _reset = Reset;
    for (root_mode, worker_mode, local) in [
        ("", "", false), ("mode opt\n", "mode opt\n", false),
        ("", "mode opt\n", false), ("", "mode opt\n", true),
    ] {
        let worker = format!(r#"{worker_mode}must type Ticket:
    Ticket(Int)
type Box(a):
    value: a
    count: Int
pub fn make(var created: Int) -> Box(Ticket):
    created = created + 1
    Box(Ticket(7), 0)
pub fn finish(own box: Box(Ticket)) -> Int:
    match move box:
        Box(ticket, count) -> match move ticket:
            Ticket(value) -> value * 10 + count
"#);
        let create = if local { "make(created)" } else { "worker.make(created)" };
        let mut source = format!(r#"{root_mode}import worker
fn maybe(bad: Bool) -> Result(Int, String):
    if bad: Err("stop") else: Ok(9)
fn choose(var created: Int, bad: Bool) -> Result(worker.Box(worker.Ticket), String):
    Ok(worker.Box(count: maybe(bad)?, ..{create}))
fn run(var created: Int, bad: Bool) -> Int:
    match choose(created, bad):
        Ok(box) -> worker.finish(box)
        Err(_) -> 0
fn main() -> Int:
    var created = 0
    let error = run(created, true)
    let value = run(created, false)
    error * 10000 + value * 100 + created
"#);
        if local {
            source.push_str("\nfn make(var created: Int) -> worker.Box(worker.Ticket):\n    created = created + 1\n    worker.Box(worker.Ticket(7), 0)\n");
        }
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), source.into_bytes()), ("worker".into(), worker.into_bytes())],
            "main", witchy_interp::comptime::EXPANDER,
        ).expect("record creation follows the potentially returning operand");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("ordered record update disposition");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).unwrap();
        vm.run().unwrap();
        // Error: no resource creation. Success: one creation and7*10+9.
        assert_eq!(vm.output(), vec!["7901"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn ordered_error_operand_preserves_resource_disposition() {
    opt::set_for_tests(Some(OptSet::none()));
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { opt::set_for_tests(None); } }
    let _reset = Reset;
    for (root_mode, worker_mode, local) in [
        ("", "", false), ("mode opt\n", "mode opt\n", false),
        ("", "mode opt\n", false), ("", "mode opt\n", true),
    ] {
        let worker = format!(r#"{worker_mode}must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)
pub fn consume(own token: Box(Ticket), value: Int) -> Result(Int, String):
    match move token:
        Box(ticket) -> match move ticket:
            Ticket(_n) -> Ok(value)
"#);
        let mut source = format!(r#"{root_mode}import worker
fn maybe(bad: Bool) -> Result(Int, String):
    if bad: Err("stop") else: Ok(9)
fn choose(bad: Bool) -> Result(Int, String):
    let value = maybe(bad)?
    worker.consume(worker.Box(worker.Ticket(7)), value)
fn main() -> Int:
    (choose(true) ?? 0) * 10 + (choose(false) ?? 0)
"#);
        if local {
            source = source.replace("worker.consume(", "consume(");
            source.push_str("\nfn consume(own token: worker.Box(worker.Ticket), value: Int) -> Result(Int, String):\n    match move token:\n        worker.Box(ticket) -> match move ticket:\n            worker.Ticket(_n) -> Ok(value)\n");
        }
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), source.into_bytes()), ("worker".into(), worker.into_bytes())],
            "main", witchy_interp::comptime::EXPANDER,
        ).expect("resource construction follows the returning operand");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("ordered operand disposition");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), vec!["9"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}

#[test]
fn record_update_cleanup_precedes_early_override_exit_when_bound_explicitly() {
    opt::set_for_tests(Some(OptSet::none()));
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { opt::set_for_tests(None); } }
    let _reset = Reset;
    for (root_mode, worker_mode, local) in [
        ("", "", false), ("mode opt\n", "mode opt\n", false),
        ("", "mode opt\n", false), ("", "mode opt\n", true),
    ] {
        let worker = format!(r#"{worker_mode}must type Ticket:
    Ticket(Int)
type Box:
    count: Int
pub fn make(var created: Int) -> Ticket:
    created = created + 1
    Ticket(7)
pub fn cleanup(own token: Ticket, var disposed: Int) -> Box:
    disposed = disposed + 1
    match move token:
        Ticket(_n) -> Box(7)
"#);
        let mut source = format!(r#"{root_mode}import worker
fn maybe(bad: Bool) -> Result(Int, String):
    if bad: Err("stop") else: Ok(9)
fn choose(var created: Int, var disposed: Int, bad: Bool) -> Result(worker.Box, String):
    let token = worker.make(created)
    let cleaned = worker.cleanup(token, disposed)
    Ok(worker.Box(count: maybe(bad)?, ..cleaned))
fn main() -> Int:
    var created = 0
    var disposed = 0
    let _ = choose(created, disposed, true)
    let _ = choose(created, disposed, false)
    created * 10 + disposed
"#);
        if local {
            source = source.replace("worker.make(", "make(").replace("worker.cleanup(", "cleanup(");
            source.push_str("\nfn make(var created: Int) -> worker.Ticket:\n    created = created + 1\n    worker.Ticket(7)\nfn cleanup(own token: worker.Ticket, var disposed: Int) -> worker.Box:\n    disposed = disposed + 1\n    match move token:\n        worker.Ticket(_n) -> worker.Box(7)\n");
        }
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), source.into_bytes()), ("worker".into(), worker.into_bytes())], "main",
            witchy_interp::comptime::EXPANDER,
        ).expect("cleanup is attempted before any override exit");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("explicit cleanup before override exit");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).unwrap();
        vm.run().unwrap();
        assert_eq!(vm.output(), vec!["22"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}
