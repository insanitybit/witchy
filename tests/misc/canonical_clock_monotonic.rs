use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

#[test]
fn monotonic_clock_runs_with_clock_grant_on_admitted_wasm() {
    let source = "fn main(console: Console, clock: Clock):\n    let first = clock.now_monotonic()\n    let wall = clock.now()\n    let second = clock.now_monotonic()\n    console.print(\"${second >= first && wall > 0}\")\n";
    let checked = witchy::resolve_std_only_checked(source).expect("Clock source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("both Clock imports admit");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { clock: true, print: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("Clock grant launches");
    vm.run().expect("Clock calls execute");
    assert_eq!(vm.output(), vec!["true"]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
    let mut denied = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("instance construction precedes capability minting");
    assert!(denied.run().is_err(), "Clock cannot run without its grant");
}
