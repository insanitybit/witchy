//! Compiler-only item quotation crosses source checking and SafeWIR admission.
//! The runtime host is exercised by the separate admitted-Wasm integration test.
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

fn owners() -> AuthenticatedModuleOwners {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/comptime-items", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let assignments = std::iter::once(("generator".into(), ModuleLoadIdentity::new(workspace, ["generator"]).unwrap()))
        .chain(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
            (*name).into(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
        )));
    AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap()
}

#[test]
fn checked_item_quote_and_emit_require_compiler_session_source() {
    for mode in ["", "mode opt\n"] {
        let source = format!("{mode}import meta\ncomptime:\n    let item = quote item:\n        fn answer() -> Int:\n            42\n    emit_item(item)\nfn main() -> Int:\n    0\n");
        let session = witchy_types::pipeline::ComptimeSourceSession::from_source_bundle(
            vec![("generator".into(), source.as_bytes().to_vec())], "generator",
            ["generator".into()].into_iter().collect(), owners(),
        ).expect("loader-owned original source");
        let recipe = session.check_block(0).expect("first source-owned comptime block");
        witchy_lower::codegen::compile_checked_comptime_recipe_artifact(&recipe)
            .expect_lowered("item quotation and emission have authenticated canonical lowering");
    }

    let ordinary_source = "import meta\nfn main() -> Int:\n    let item = quote item:\n        fn answer() -> Int:\n            42\n    emit_item(item)\n    0\n";
    let ordinary = witchy_types::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        vec![("generator".into(), ordinary_source.as_bytes().to_vec())], "generator",
        witchy_syntax::linker::ComptimeExpander::new(|_, _, _| Ok(Default::default())),
        &["generator".to_owned()].into_iter().collect(), owners(),
    ).expect_err("ordinary source cannot install compiler-only emission");
    assert!(ordinary.to_string().contains("compile-time-only"), "{ordinary}");
}
