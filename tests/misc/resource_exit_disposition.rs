//! Loop exits retain outer owners and require local disposition first.
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const WORKER: &str = r#"must type Ticket:
    Ticket(Int)
type Box(a):
    Box(a)
fn finish(own value: Box(Ticket)) -> Int:
    match value:
        Box(ticket) -> finish_ticket(ticket)
fn finish_ticket(own ticket: Ticket) -> Int:
    match ticket:
        Ticket(n) -> n
pub fn choose(skip: Bool) -> Int:
    let outer = Box(Ticket(7))
    var total = 0
    var index = 0
    while index < 1:
        index = index + 1
        let token = Box(Ticket(8))
        while true:
            break
        total = total + finish(token)
        if skip:
            continue
        break
    total + finish(outer)
"#;

#[test]
fn loop_exit_preserves_outer_owner_and_completed_disposition_across_modes() {
    opt::set_for_tests(Some(OptSet::none()));
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { opt::set_for_tests(None); } }
    let _reset = Reset;
    let fast = format!("mode opt\n{WORKER}");
    let imported = r#"fn finish_imported(own value: fast.Box(fast.Ticket)) -> Int:
    match value:
        fast.Box(ticket) -> finish_imported_ticket(ticket)
fn finish_imported_ticket(own ticket: fast.Ticket) -> Int:
    match ticket:
        fast.Ticket(n) -> n
fn imported(skip: Bool) -> Int:
    let outer = fast.Box(fast.Ticket(7))
    var total = 0
    var index = 0
    while index < 1:
        index = index + 1
        let token = fast.Box(fast.Ticket(8))
        total = total + finish_imported(token)
        if skip:
            continue
        break
    total + finish_imported(outer)
fn main() -> Int:
    choose(true) * 1000000 + choose(false) * 10000 + fast.choose(true) * 100 + imported(false)
"#;
    for mode in ["", "mode opt\n"] {
        let main = format!("{mode}import fast\n{WORKER}\n{imported}");
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), main.into_bytes()), ("fast".into(), fast.as_bytes().to_vec())],
            "main", witchy_interp::comptime::EXPANDER,
        ).expect("checked loop exit disposition");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("unoptimized loop exit admission");
        let mut runtime = Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).unwrap();
        vm.run().expect("loop exit execution");
        // Each execution discharges the inner8 and the surviving outer7 once.
        assert_eq!(vm.output(), vec!["15151515"]);
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    }
}
