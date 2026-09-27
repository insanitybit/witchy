//! A resource is created only after a potentially returning operand completes.
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

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
    Ok(value)
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
            source.push_str("\nfn consume(own token: worker.Box(worker.Ticket), value: Int) -> Result(Int, String):\n    Ok(value)\n");
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
