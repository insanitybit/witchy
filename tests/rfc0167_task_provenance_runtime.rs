//! Deferred opt work keeps its source mode across a normal caller and replay.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

fn authenticated_owners() -> AuthenticatedModuleOwners {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "example/task-producer", "0.1.0")
        .expect("workspace coordinate");
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "0.1.0")
        .expect("toolchain coordinate");
    let mut assignments = ["main", "worker"].map(|name| (
        name.to_owned(), ModuleLoadIdentity::new(workspace.clone(), [name]).expect("user owner"),
    )).to_vec();
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).expect("std owner"),
    )));
    AuthenticatedModuleOwners::from_loader_assignments(assignments).expect("owners")
}

#[test]
fn cross_mode_recipe_replays_with_independent_expected_result_and_no_leaks() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let worker = "mode opt\nimport task\n\ntype Stored:\n    recipe: task.Task(Int)\n\npub async fn compatible() -> Int:\n    7\n\npub fn store(own recipe: task.Task(Int)) -> Stored:\n    Stored(recipe: recipe)\n\npub async fn execute(own recipe: task.Task(Int)) -> Int:\n    recipe.await\n";
    let main = "import worker\n\nasync fn ordinary() -> Int:\n    9\n\nasync fn main(console: Console):\n    let recipe = worker.compatible()\n    let first = recipe.await\n    let replay = recipe.await\n    let boundary = worker.execute(worker.compatible()).await\n    let stored = worker.store(worker.compatible())\n    let projected = worker.execute(stored.recipe).await\n    let ordinary_result = ordinary().await\n    console.print(\"${first * 10000 + replay * 1000 + boundary * 100 + projected * 10 + ordinary_result}\")\n";
    let checked = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("worker".into(), worker.as_bytes().to_vec()),
             ("main".into(), main.as_bytes().to_vec())],
        "main", witchy_syntax::linker::ComptimeExpander::new(
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())),
        &std::collections::HashSet::from(["main".into(), "worker".into()]), authenticated_owners(),
    ).expect("checked cross-mode task sources");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted unoptimized task Wasm");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() }, 128,
    ).expect("admitted task launch");
    vm.run().expect("cross-mode deferred execution");
    // Each replay and the stored imported-type projection executes the same
    // authenticated opt body, then the ordinary body supplies the final 9.
    assert_eq!(vm.output(), vec!["77779"]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}

#[test]
fn an_opt_defined_record_does_not_authenticate_a_normal_task() {
    let worker = "mode opt\nimport task\n\ntype Stored:\n    recipe: task.Task(Int)\n\npub fn store(own recipe: task.Task(Int)) -> Stored:\n    Stored(recipe: recipe)\n\npub fn relay[a](own value: a) -> a:\n    value\n\npub async fn execute(own recipe: task.Task(Int)) -> Int:\n    recipe.await\n";
    let main = "import worker\n\nasync fn ordinary() -> Int:\n    9\n\nasync fn main(console: Console):\n    let stored = worker.relay(worker.store(ordinary()))\n    console.print(\"${worker.execute(stored.recipe).await}\")\n";
    let checked = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("worker".into(), worker.as_bytes().to_vec()),
             ("main".into(), main.as_bytes().to_vec())],
        "main", witchy_syntax::linker::ComptimeExpander::new(
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())),
        &std::collections::HashSet::from(["main".into(), "worker".into()]), authenticated_owners(),
    );
    // A record's defining mode is not evidence about the task stored in it.
    // The exact checked-use boundary must reject the normal Task before WIR.
    let error = checked.expect_err("normal Task must not acquire opt authority through imported storage");
    assert!(format!("{error:?}").contains("opt task execution requires checked opt-compatible task value provenance"),
        "unexpected rejection: {error:?}");
}

#[test]
fn generic_identity_preserves_actual_task_origin_across_normal_to_opt_call() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let worker = "mode opt\nimport task\n\ntype Stored:\n    recipe: task.Task(Int)\n\npub async fn compatible() -> Int:\n    7\n\npub fn store(own recipe: task.Task(Int)) -> Stored:\n    Stored(recipe: recipe)\n\npub fn relay[a](own value: a) -> a:\n    value\n\npub async fn execute(own recipe: task.Task(Int)) -> Int:\n    recipe.await\n";
    let main = "import worker\n\nasync fn main(console: Console):\n    let direct = worker.execute(worker.relay(worker.compatible())).await\n    let stored = worker.relay(worker.store(worker.compatible()))\n    let imported = worker.execute(stored.recipe).await\n    let pair = worker.relay((worker.compatible(), 3))\n    let tuple = worker.execute(pair.0).await\n    let nested = worker.relay((3, (worker.compatible(), 4)))\n    let nested_task = worker.execute(nested.1.0).await\n    console.print(\"${direct * 1000 + imported * 100 + tuple * 10 + nested_task}\")\n";
    let checked = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("worker".into(), worker.as_bytes().to_vec()),
             ("main".into(), main.as_bytes().to_vec())],
        "main", witchy_syntax::linker::ComptimeExpander::new(
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())),
        &std::collections::HashSet::from(["main".into(), "worker".into()]), authenticated_owners(),
    ).expect("checked generic cross-mode task sources");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted unoptimized generic task Wasm");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() }, 128,
    ).expect("admitted task launch");
    vm.run().expect("generic cross-mode deferred execution");
    assert_eq!(vm.output(), vec!["7777"]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}
