use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

fn checked(source: &str) -> witchy_interp::pipeline::CheckedModule {
    let module = witchy_syntax::parser::parse_module(source).expect("parse Dynamic fixture");
    let workspace = PackageCoordinate::new(
        PackageSource::Workspace,
        "example/dynamic-interpreter-test",
        "0.1.0",
    )
    .expect("workspace coordinate");
    let toolchain = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/stdlib",
        "0.1.0",
    )
    .expect("toolchain coordinate");
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(workspace, ["main"]).expect("main owner"),
    )];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|module| {
        (
            (*module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *module]).expect("std owner"),
        )
    }));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .expect("authenticated owners");
    witchy_interp::pipeline::link_checked_authenticated(
        vec![("main".into(), module)],
        "main",
        owners,
    )
    .expect("authenticated checked link")
}

#[test]
fn descriptor_exact_decode_and_mismatch_are_checked_data() {
    let checked = checked(
        "import dynamic\nimport reflect\n\ntype User derive(Reflect):\n    name: String\n    age: Int\n\nfn main(console: Console):\n    let value = dynamic.dynamic(7)\n    console.print(dynamic.type_name(dynamic.type_of(value)))\n    let exact: Option(Int) = dynamic.try_decode(value)\n    match exact:\n        Some(number) -> console.print(\"${number}\")\n        None -> console.print(\"missing-int\")\n    let mismatch: Option(String) = dynamic.try_decode(value)\n    match mismatch:\n        Some(text) -> console.print(text)\n        None -> console.print(\"none\")\n    let person = dynamic.dynamic(User(\"Ada\", 42))\n    let decoded_person: Option(User) = dynamic.try_decode(person)\n    match decoded_person:\n        Some(user) -> console.print(user.name)\n        None -> console.print(\"missing-user\")\n    let words = dynamic.dynamic([\"alpha\", \"beta\"])\n    let decoded_words: Option(List(String)) = dynamic.try_decode(words)\n    match decoded_words:\n        Some(items) -> console.print(list.at(items, 1))\n        None -> console.print(\"missing-words\")\n",
    );
    let output = witchy_interp::interpreter::run_checked_module(checked, ".", Vec::new())
        .expect("run authenticated Dynamic fixture");
    assert_eq!(output, ["Int", "7", "none", "Ada", "beta"]);
}
