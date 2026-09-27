use witchy_runtime::runtime::{Capabilities, Runtime, comptime::ComptimeLimits};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

fn owners() -> AuthenticatedModuleOwners {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/comptime", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let mut assignments = vec![("comptime".into(), ModuleLoadIdentity::new(workspace, ["comptime"]).unwrap())];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
    )));
    AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap()
}

#[test]
fn canonical_comptime_requires_loader_authenticated_meta_identity() {
    let forged = PackageCoordinate::new(PackageSource::Workspace, "witchy/std", "1").unwrap();
    let owners = AuthenticatedModuleOwners::from_loader_assignments([
        ("comptime".to_owned(), ModuleLoadIdentity::new(forged.clone(), ["comptime"]).unwrap()),
        ("meta".to_owned(), ModuleLoadIdentity::new(forged, ["std", "meta"]).unwrap()),
    ]).unwrap();
    let error = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), b"fn main() -> Int:\n    42\n".to_vec())], "scope", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners,
    ).expect_err("a same-spelled workspace module is not toolchain authority");
    assert!(error.to_string().contains("requires toolchain witchy/std meta ownership"), "{error}");
}

#[test]
fn canonical_comptime_refuses_unmetered_host_services() {
    let source = "fn main() -> Int:\n    let text = \"${1.25}\"\n    if text == \"1.25\":\n        42\n    else:\n        0\n";
    let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), source.as_bytes().to_vec())], "bounded-service", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners(),
    ).expect("valid compiler-service source");
    let rejection = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
        .expect_rejected("unmetered host computation must not escape deterministic compiler budget");
    assert!(rejection.to_string().contains("no deterministic compiler-session budget contract"), "{rejection}");
}

#[test]
fn canonical_fresh_names_execute_with_session_and_budget_boundaries() {
    let source = "import meta\nfn main() -> Int:\n    let a = meta.fresh(\"x\")\n    let b = meta.fresh(\"x\")\n    if a == b:\n        0\n    else:\n        42\n";
    let recipe = witchy_types::pipeline::check_comptime_recipe_from_source_bundle(
        vec![("comptime".into(), source.as_bytes().to_vec())], "a/b", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners(),
    ).expect("source-backed compiler recipe");
    let artifact = witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
        .expect_lowered("admitted unoptimized compiler recipe");
    let limits = ComptimeLimits { fuel: 2_000_000, memory_pages: 64, fresh_identifiers: 2, host_output_bytes: 4096 };
    let first = Runtime::run_comptime(&artifact, limits).expect("actual admitted compiler execution");
    let second = Runtime::run_comptime(&artifact, limits).expect("fresh deterministic session");
    assert_eq!(first.result, 42); // distinct issuance ordinals, not an interpreter oracle
    assert_eq!(first.heap_leaks, 0);
    assert_eq!(first.fresh_identifiers.len(), 2);
    assert_eq!(first.fresh_identifiers, ["__witchy_fresh_612f62_0_x", "__witchy_fresh_612f62_1_x"]);
    assert_ne!(first.fresh_identifiers[0], first.fresh_identifiers[1]);
    assert_eq!(first.fresh_identifiers, second.fresh_identifiers);
    assert_eq!(first.fuel_consumed, second.fuel_consumed);
    assert!(first.fuel_consumed > 0);
    assert!(Runtime::batch_quick().unwrap().spawn_admitted(&artifact, Capabilities::none(), 64).is_err());
    assert!(Runtime::batch_quick().unwrap().spawn_admitted_with_external_libraries(&artifact, Capabilities::none(), 64, &[]).is_err());
    assert!(Runtime::run_comptime(&artifact, ComptimeLimits { fuel: 0, ..limits }).is_err());
    assert!(Runtime::run_comptime(&artifact, ComptimeLimits { fresh_identifiers: 1, ..limits }).is_err());
    assert!(Runtime::run_comptime(&artifact, ComptimeLimits { host_output_bytes: 1, ..limits }).is_err());

    // The same valid typed body, without recipe authority, cannot introduce a
    // compiler-only host call by claiming its ABI alone.
    let ordinary = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("comptime".into(), source.as_bytes().to_vec())], "comptime", witchy_interp::comptime::EXPANDER,
        &["comptime".to_owned()].into_iter().collect(), owners(),
    ).expect("ordinary checked source");
    let rejection = witchy_lower::codegen::compile_checked_module_artifact(&ordinary)
        .expect_rejected("compiler imports require source-bound recipe authority");
    assert!(rejection.to_string().contains("checked comptime launch contract"), "{rejection}");
}
