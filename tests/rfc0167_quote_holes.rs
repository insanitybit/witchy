//! Parser-issued static typed holes cross the authenticated compiler session.
#[test]
fn parser_issued_typed_quote_holes_generate_admitted_normal_program() {
    let source = r#"import meta
comptime:
    let a = quote expr:
        5
    let b = quote expr:
        7
    let sum = quote expr:
        ${a} + ${b}
    emit_item(quote item:
        fn answer() -> Int:
            ${sum}
    )
fn main(console: Console):
    console.print("${answer()}")
"#;
    use witchy_types::runtime_type::{
        AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
    };
    let workspace =
        PackageCoordinate::new(PackageSource::Workspace, "test/quote-hole", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let mut assignments = vec![(
        "main".to_owned(),
        ModuleLoadIdentity::new(workspace, ["main"]).unwrap(),
    )];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
        (
            (*name).to_owned(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
        )
    }));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap();
    let (checked, _) =
        witchy::canonical_comptime::check_and_expand_source_bundle_with_imported_blocks(
            vec![("main".into(), source.as_bytes().to_vec())],
            "main",
            "main",
            ["main".to_owned()].into_iter().collect(),
            owners,
            witchy::canonical_comptime::source_limits(),
        )
        .unwrap();
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit expanded quote holes");
    let mut runtime = witchy::runtime::Runtime::batch().unwrap();
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            witchy::runtime::Capabilities {
                print: true,
                quiet: true,
                ..witchy::runtime::Capabilities::none()
            },
            128,
        )
        .unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), ["12"]);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}
