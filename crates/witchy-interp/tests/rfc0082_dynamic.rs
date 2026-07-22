use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

fn checked(source: &str) -> witchy_interp::pipeline::CheckedModule {
    checked_result(source).expect("authenticated checked link")
}

fn checked_result(
    source: &str,
) -> Result<witchy_interp::pipeline::CheckedModule, witchy_interp::pipeline::PipelineError> {
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
}

#[test]
fn descriptors_and_exact_decode_cover_nominal_generic_and_structural_data() {
    let checked = checked(
        "import dynamic\nimport reflect\n\ntype User derive(Reflect):\n    name: String\n    age: Int\n\ntype Box(a) derive(Reflect):\n    Box(a)\n\nfn main(console: Console):\n    let value = dynamic.dynamic(7)\n    console.print(dynamic.type_name(dynamic.type_of(value)))\n    console.print(dynamic.type_name(dynamic.runtime_type(Int)))\n    console.print(dynamic.type_name(dynamic.runtime_type(User)))\n    let exact: Option(Int) = dynamic.try_decode(value)\n    match exact:\n        Some(number) -> console.print(\"${number}\")\n        None -> console.print(\"missing-int\")\n    let mismatch: Option(String) = dynamic.try_decode(value)\n    match mismatch:\n        Some(text) -> console.print(text)\n        None -> console.print(\"none\")\n    let decoded: Result(Int, dynamic.DynamicError) = dynamic.decode(value)\n    match decoded:\n        Ok(number) -> console.print(\"decoded-${number}\")\n        Err(_) -> console.print(\"decode-failed\")\n    let wrong: Result(String, dynamic.DynamicError) = dynamic.decode(value)\n    match wrong:\n        Ok(text) -> console.print(text)\n        Err(dynamic.TypeMismatch(actual)) -> console.print(\"mismatch-${dynamic.type_name(actual)}\")\n    let person = dynamic.dynamic(User(\"Ada\", 42))\n    let decoded_person: Option(User) = dynamic.try_decode(person)\n    match decoded_person:\n        Some(user) -> console.print(user.name)\n        None -> console.print(\"missing-user\")\n    let words = dynamic.dynamic([\"alpha\", \"beta\"])\n    let decoded_words: Option(List(String)) = dynamic.try_decode(words)\n    match decoded_words:\n        Some(items) -> console.print(list.at(items, 1))\n        None -> console.print(\"missing-words\")\n    let boxed = dynamic.dynamic(Box(11))\n    let decoded_box: Option(Box(Int)) = dynamic.try_decode(boxed)\n    match decoded_box:\n        Some(Box(number)) -> console.print(\"box-${number}\")\n        None -> console.print(\"missing-box\")\n    let record = dynamic.dynamic(.{name: \"Nia\", age: 9})\n    let decoded_record: Option(.{age: Int, name: String}) = dynamic.try_decode(record)\n    match decoded_record:\n        Some(found) -> console.print(found.name)\n        None -> console.print(\"missing-record\")\n    let choice: .[Count(Int) | Label(String)] = .Count(5)\n    let encoded_choice = dynamic.dynamic(choice)\n    let decoded_choice: Option(.[Count(Int) | Label(String)]) = dynamic.try_decode(encoded_choice)\n    match decoded_choice:\n        Some(.Count(number)) -> console.print(\"count-${number}\")\n        Some(.Label(label)) -> console.print(label)\n        None -> console.print(\"missing-choice\")\n",
    );
    let output = witchy_interp::interpreter::run_checked_module(checked, ".", Vec::new())
        .expect("run authenticated Dynamic fixture");
    assert_eq!(
        output,
        ["Int", "Int", "User", "7", "none", "decoded-7", "mismatch-Int", "Ada", "beta", "box-11", "Nia", "count-5"]
    );
}

#[test]
fn runtime_type_rejects_transitively_retained_capabilities_with_a_path() {
    let module = checked(
        "import dynamic\n\ntype Session:\n    transport: Net\n\nfn main(console: Console):\n    console.print(dynamic.type_name(dynamic.runtime_type(Session)))\n",
    );
    let error = witchy_interp::interpreter::run_checked_module(module, ".", Vec::new())
    .expect_err("a runtime descriptor must not retain authority")
    .to_string();
    assert!(error.contains("Session"), "missing declaration path: {error}");
    assert!(error.contains("transport"), "missing field path: {error}");
    assert!(error.contains("Net"), "missing capability leaf: {error}");
}

#[test]
fn dynamic_decode_without_an_expected_type_is_a_loud_source_error() {
    let error = checked_result(
        "import dynamic\n\nfn main():\n    let value = dynamic.dynamic(7)\n    dynamic.try_decode(value)\n",
    )
    .expect_err("decode result type must be selected by context")
    .to_string();
    assert!(
        error.contains("infer") || error.contains("expected") || error.contains("concrete type"),
        "missing expected-type diagnostic: {error}"
    );
}
