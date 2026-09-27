//! Deferred opt work keeps its source mode across a normal caller and replay.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn cross_mode_recipe_replays_with_independent_expected_result_and_no_leaks() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let worker = "mode opt\nimport task\n\npub async fn compatible() -> Int:\n    7\n\npub async fn execute(own recipe: task.Task(Int)) -> Int:\n    recipe.await\n";
    let main = "import worker\n\nasync fn ordinary() -> Int:\n    9\n\nasync fn main(console: Console):\n    let recipe = worker.compatible()\n    let first = recipe.await\n    let replay = recipe.await\n    let boundary = worker.execute(worker.compatible()).await\n    let ordinary_result = ordinary().await\n    console.print(\"${first * 1000 + replay * 100 + boundary * 10 + ordinary_result}\")\n";
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
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).expect("owners");
    let checked = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("worker".into(), worker.as_bytes().to_vec()),
             ("main".into(), main.as_bytes().to_vec())],
        "main", witchy_syntax::linker::ComptimeExpander::new(
            |_, _, _| Ok(witchy_syntax::origin::OriginTable::default())),
        &std::collections::HashSet::from(["main".into(), "worker".into()]), owners,
    ).expect("checked cross-mode task sources");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admitted unoptimized task Wasm");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() }, 128,
    ).expect("admitted task launch");
    vm.run().expect("cross-mode deferred execution");
    // Construction captures once; each poll reruns the opt worker and returns
    // 7. The separate opt-consumer call also returns 7, then normal work 9.
    assert_eq!(vm.output(), vec!["7779"]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}
