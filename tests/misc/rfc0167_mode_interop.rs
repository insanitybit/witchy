//! Reference-free normal/opt interoperability through the production source route.

use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

const WORKER: &str = r#"mode opt

pub fn append(own values: List(Int), value: Int) -> List(Int):
    values.push(value)
    values
"#;

const API: &str = r#"mode opt

import worker

pub fn extend(own values: List(Int), value: Int) -> List(Int):
    worker.append(values, value)
"#;

const MAIN: &str = r#"import api

fn main(console: Console):
    let values = [1, 2]
    let before = values
    let extended = api.extend(values, 3)
    console.print("${extended}:${before}")
"#;

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        witchy::opt::set_for_tests(None);
    }
}

fn checked_graph() -> witchy_types::pipeline::CheckedModule {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![
            ("worker".into(), WORKER.as_bytes().to_vec()),
            ("api".into(), API.as_bytes().to_vec()),
            ("main".into(), MAIN.as_bytes().to_vec()),
        ],
        "main",
        witchy_interp::comptime::expand_compile_time,
    )
    .expect("checked normal -> opt -> opt source graph")
}

#[test]
fn reference_free_normal_opt_chain_preserves_modes_and_aliased_snapshot_without_opts() {
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_eq!(
        witchy::opt::active_schema_key().enabled_mask(),
        0,
        "this fixture is the unoptimized canonical-Wasm baseline",
    );

    let checked = checked_graph();
    let module = checked.module();

    assert_eq!(
        module.modes,
        ["@opt:api".to_owned(), "@opt:worker".to_owned()],
        "linking a normal entry retains each imported module's source mode as internal metadata",
    );
    assert!(!witchy_lower::analysis::function_is_opt(module, "main"));
    assert!(witchy_lower::analysis::function_is_opt(module, "api.extend"));
    assert!(witchy_lower::analysis::function_is_opt(module, "worker.append"));

    let misses = witchy_lower::analysis::try_module_no_copy_misses(module)
        .expect("checked access facts remain available at both boundaries");
    assert!(
        misses.iter().all(|miss| miss.function != "api.extend"),
        "the opt -> opt call must enter directly with its checked owned contract: {misses:?}",
    );
    let repairs: Vec<_> = witchy_lower::analysis::module_boundary_repairs(module)
        .into_iter()
        .filter(|repair| repair.function == "main" || repair.function == "api.extend")
        .collect();
    assert!(repairs.is_empty(), "the reference-free owned chain needs no unique adapter: {repairs:?}");
    witchy::enforce_performance_modes(module, "main")
        .expect("both imported opt functions satisfy their checked ownership contracts");

    let expected = vec!["[1, 2, 3]:[1, 2]".to_owned()];
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("unoptimized normal/opt chain admits through canonical SafeWIR");
    let entry = artifact.entry().expect("canonical entry");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = witchy::codegen::validate_checked_module_for_oracle(&checked)
        .expect("canonical SafeWIR validation");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(&validated, function, arguments, Default::default())
        .expect("canonical interpreter normal/opt chain");
    assert_eq!(oracle.output, expected, "independent canonical result");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter heap audit");

    let mut runtime = witchy::runtime::Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            witchy::runtime::Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            128,
        )
        .expect("admitted unoptimized canonical-Wasm instance");
    vm.run().expect("run unoptimized canonical-Wasm instance");
    assert_eq!(vm.output(), expected, "canonical Wasm matches the independent expected result");
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

