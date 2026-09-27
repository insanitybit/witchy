//! Record updates consume unchanged affine children and retire overwritten storage.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const PROGRAM: &str = r#"
must type Ticket:
    Ticket(Int)

type Box(a):
    value: a
    count: Int

fn bump[a](own box: Box(a)) -> Box(a):
    let count = box.count + 1
    Box(count: count, ..move box)

fn finish(own box: Box(Ticket)) -> Int:
    match move box:
        Box(ticket, count) -> match move ticket:
            Ticket(value) -> value * 10 + count

fn nested(own box: Box(Box(Ticket))) -> Int:
    match move box:
        Box(inner, count) -> finish(inner) * 10 + count

fn replace(own box: Box(unique String)) -> Box(unique String):
    Box(value: "xy", ..move box)

fn size(own box: Box(unique String)) -> Int:
    match move box:
        Box(value, count) -> value.length() * 10 + count

fn initial_nested() -> Box(Box(unique String)):
    let inner: Box(unique String) = Box("abc", 5)
    Box(inner, 2)

fn replace_nested(own box: Box(Box(unique String))) -> Box(Box(unique String)):
    Box(value: Box("z", 4), ..move box)

fn nested_size(own box: Box(Box(unique String))) -> Int:
    match move box:
        Box(inner, count) -> size(inner) * 10 + count

type Viewed(a):
    value: a
    snapshot: &Int
    count: Int

fn bump_viewed[a](own box: Viewed(a)) -> Viewed(a):
    Viewed(count: 3, ..move box)

fn replace_view[a](own box: Viewed(a)) -> Viewed(a):
    let root = 11
    Viewed(snapshot: &root, ..move box)

fn finish_viewed(own box: Viewed(Ticket)) -> Int:
    match move box:
        Viewed(ticket, snapshot, count) -> match move ticket:
            Ticket(value) -> value * 10 + count + (*snapshot)

fn main() -> Int:
    let direct = finish(bump(Box(Ticket(7), 2)))
    let deep = nested(bump(Box(Box(Ticket(9), 4), 2)))
    let initial: Box(unique String) = Box("abc", 5)
    let replaced = size(replace(initial))
    let nested_replaced = nested_size(replace_nested(initial_nested()))
    let root = 5
    let opaque = finish_viewed(bump_viewed(Viewed(Ticket(8), &root, 2)))
    let opaque_replaced = finish_viewed(replace_view(Viewed(Ticket(8), &root, 3)))
    (direct * 100000 + deep * 100 + replaced) * 100 + opaque + opaque_replaced + nested_replaced
"#;

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn consuming_record_reconstruction_runs_in_both_modes_without_leaks() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}{PROGRAM}");
        let checked = witchy_types::pipeline::link_checked_from_source_bundle(
            vec![("main".into(), source.into_bytes())], "main",
            witchy_interp::comptime::EXPANDER,
        ).expect("checked consuming record reconstruction");
        let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("unoptimized canonical admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).expect("admitted unoptimized Wasm");
        vm.run().expect("record reconstruction execution");
        // Direct: 7*10+(2+1)=73. Nested: (9*10+4)*10+(2+1)=943.
        // Replacement: length("xy")*10+5=25. Opaque: 8*10+(2+1)+5=88.
        // Replaced opaque owner field: 8*10+3+11=94.
        // Nested qualified replacement: (length("z")*10+4)*10+2=142.
        // Combine (73*100000+943*100+25)*100+88+94+142.
        assert_eq!(vm.output(), vec!["739432824"]);
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}
