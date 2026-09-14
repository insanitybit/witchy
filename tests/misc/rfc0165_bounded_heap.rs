//! Ordinary repeated allocation must be bounded by live storage, not the
//! lifetime sum of allocations. This is deliberately not a chars/decoder or
//! collection-growth benchmark: each helper reserves once and uses one slot.
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{
    interpret_entry_program, InterpreterConfig, InterpreterValue,
};
use witchy_wir::canonical_safewir::FunctionId;

const SOURCE: &str = r#"
fn allocation(seed: Int) -> Int:
    var values: List(Int) = list.with_capacity(8192)
    values.push(seed)
    values.at(0)

fn main(console: Console):
    var total = 0
    for i in 0..320:
        total = total + allocation(i)
    console.print("${total}")
"#;

#[test]
fn repeated_owned_allocations_complete_with_sixteen_mib_and_zero_leaks() {
    let checked = witchy::resolve_std_only_checked(SOURCE)
        .expect("ordinary fixed-capacity source typechecks");
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("source-bound entry");
            let function = FunctionId::new(
                entry.export().strip_prefix('f').unwrap().parse().unwrap(),
            );
            let capabilities = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            let result = interpret_entry_program(
                validated, function, capabilities, &[],
                InterpreterConfig { max_steps: 20_000_000, ..Default::default() },
            ).expect("independent canonical oracle executes the entire loop");
            assert_eq!(result.output, ["51040"]);
            assert_eq!(result.leaked_objects, 0);
        },
    ).expect_lowered("source survives initial/final validation and exact-byte admission");

    let mut runtime = Runtime::batch().expect("native runtime");
    // First establish exact behavior and RC cleanup with sufficient address
    // space. Zero leaks here must not be mistaken for bounded heap reuse.
    for limit_mib in [64, 16] {
        let mut vm = runtime.spawn_admitted(
            &artifact,
            Capabilities { print: true, quiet: true, ..Capabilities::none() },
            limit_mib * 16, // Wasm pages are 64 KiB.
        ).expect("instantiate the same artifact with an explicit memory limit");
        let execution = vm.run();
        eprintln!("bounded-heap: limit={limit_mib}MiB execution={execution:?}");
        execution.unwrap_or_else(|error| panic!(
            "320 allocate/use/drop calls must complete under {limit_mib} MiB; \
             generous-limit RC cleanup is not bounded-heap evidence: {error:#}"
        ));
        assert_eq!(vm.output(), ["51040"], "limit={limit_mib} MiB");
        let leaks = vm.canonical_heap_leaks().expect("canonical ownership audit");
        eprintln!("bounded-heap: limit={limit_mib}MiB output=51040 owned_leaks={leaks:?}");
        assert_eq!(leaks, Some(0), "limit={limit_mib} MiB");
    }
}
