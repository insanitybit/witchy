use witchy::runtime::{Capabilities, Runtime};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("source fixture path");
    let source = std::fs::read_to_string(path)?;
    let checked = witchy::resolve_std_only_checked(&source)?;
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted profile fixture");
    let mut runtime = Runtime::batch_with_profiler(wasmtime::ProfilingStrategy::PerfMap)?;
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities {
            print: true,
            print_int: true,
            clock: true,
            quiet: true,
            ..Capabilities::none()
        },
        256,
    )?;
    vm.run()?;
    for line in vm.output() {
        println!("{line}");
    }
    assert_eq!(vm.canonical_heap_leaks()?, Some(0));
    Ok(())
}
