use witchy::runtime::{Capabilities, Runtime};

#[test]
fn perf_map_runs_gc_async_without_native_dwarf_transformation() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            witchy::opt::set_for_tests(None);
        }
    }
    let _reset = Reset;
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let source = include_str!("../../benchmarks/async_general/packet_string.witchy")
        .replace("__MESSAGES__", "16");
    let checked = witchy::resolve_std_only_checked(&source).expect("checked string channel source");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted GC async artifact");
    let mut runtime = Runtime::batch_with_profiler(wasmtime::ProfilingStrategy::PerfMap)
        .expect("perf map engine");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                clock: true,
                quiet: true,
                ..Capabilities::none()
            },
            256,
        )
        .expect("profiled GC VM");
    vm.run().expect("run without GC DWARF transformation");
    assert_eq!(vm.output()[0], "270");
    assert!(vm.output()[1].starts_with("bench_ns="));
    assert_eq!(
        vm.canonical_heap_leaks().expect("profiled heap audit"),
        Some(0)
    );
}
