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
fn dormant_reflect_projection_helpers_do_not_require_dynamic_authentication() {
    let output = witchy_interp::interpreter::run(
        "import reflect\n\ntype User derive(Reflect):\n    name: String\n\nfn main(console: Console):\n    console.print(\"ordinary-reflection-loaded\")\n",
    )
    .expect("a dormant compiler-private projection helper is not a Dynamic operation");
    assert_eq!(output, ["ordinary-reflection-loaded"]);
}

#[test]
fn descriptors_and_exact_decode_cover_nominal_generic_and_structural_data() {
    let checked = checked(
        "import dynamic\nimport reflect\n\ntype User derive(Reflect):\n    name: String\n    age: Int\n\ntype Box(a) derive(Reflect):\n    Box(a)\n\nfn main(console: Console):\n    let value = dynamic.dynamic(7)\n    console.print(dynamic.type_name(dynamic.type_of(value)))\n    console.print(dynamic.type_name(dynamic.runtime_type(Int)))\n    console.print(dynamic.type_name(dynamic.runtime_type(User)))\n    let exact: Option(Int) = dynamic.try_decode(value)\n    match exact:\n        Some(number) -> console.print(\"${number}\")\n        None -> console.print(\"missing-int\")\n    let mismatch: Option(String) = dynamic.try_decode(value)\n    match mismatch:\n        Some(text) -> console.print(text)\n        None -> console.print(\"none\")\n    let decoded: Result(Int, dynamic.DynamicError) = dynamic.decode(value)\n    match decoded:\n        Ok(number) -> console.print(\"decoded-${number}\")\n        Err(_) -> console.print(\"decode-failed\")\n    let wrong: Result(String, dynamic.DynamicError) = dynamic.decode(value)\n    match wrong:\n        Ok(text) -> console.print(text)\n        Err(dynamic.TypeMismatch(actual)) -> console.print(\"mismatch-${dynamic.type_name(actual)}\")\n        Err(_) -> console.print(\"unexpected-dynamic-error\")\n    let person = dynamic.dynamic(User(\"Ada\", 42))\n    let decoded_person: Option(User) = dynamic.try_decode(person)\n    match decoded_person:\n        Some(user) -> console.print(user.name)\n        None -> console.print(\"missing-user\")\n    let words = dynamic.dynamic([\"alpha\", \"beta\"])\n    let decoded_words: Option(List(String)) = dynamic.try_decode(words)\n    match decoded_words:\n        Some(items) -> console.print(list.at(items, 1))\n        None -> console.print(\"missing-words\")\n    let boxed = dynamic.dynamic(Box(11))\n    let decoded_box: Option(Box(Int)) = dynamic.try_decode(boxed)\n    match decoded_box:\n        Some(Box(number)) -> console.print(\"box-${number}\")\n        None -> console.print(\"missing-box\")\n    let record = dynamic.dynamic(.{name: \"Nia\", age: 9})\n    let decoded_record: Option(.{age: Int, name: String}) = dynamic.try_decode(record)\n    match decoded_record:\n        Some(found) -> console.print(found.name)\n        None -> console.print(\"missing-record\")\n    let choice: .[Count(Int) | Label(String)] = .Count(5)\n    let encoded_choice = dynamic.dynamic(choice)\n    let decoded_choice: Option(.[Count(Int) | Label(String)]) = dynamic.try_decode(encoded_choice)\n    match decoded_choice:\n        Some(.Count(number)) -> console.print(\"count-${number}\")\n        Some(.Label(label)) -> console.print(label)\n        None -> console.print(\"missing-choice\")\n",
    );
    let output = witchy_interp::interpreter::run_checked_module(checked, ".", Vec::new())
        .expect("run authenticated Dynamic fixture");
    assert_eq!(
        output,
        ["Int", "Int", "User", "7", "none", "decoded-7", "mismatch-Int", "Ada", "beta", "box-11", "Nia", "count-5"]
    );
}

#[test]
fn public_field_enumeration_and_projection_are_checked_data_operations() {
    let checked = checked(
        r#"
import dynamic
import reflect

type User derive(Reflect):
    name: String
    age: Int

type Cell(a) derive(Reflect):
    value: a

sealed type Vault derive(Reflect):
    token: String

fn main(console: Console):
    let user = dynamic.dynamic(User("Ada", 42))
    for info in dynamic.fields(dynamic.type_of(user)):
        console.print(dynamic.field_name(info) + ":" + dynamic.type_name(dynamic.field_type(info)))
    match dynamic.field(user, "name"):
        Ok(projected) ->
            let decoded: Result(String, dynamic.DynamicError) = dynamic.decode(projected)
            match decoded:
                Ok(name) -> console.print("name-${name}")
                Err(_) -> console.print("decode-error")
        Err(_) -> console.print("projection-error")
    match dynamic.field(user, "missing"):
        Err(dynamic.MissingField(name)) -> console.print("missing-${name}")
        _ -> console.print("wrong-missing-error")
    match dynamic.field(user, ""):
        Err(dynamic.MalformedRequest(_)) -> console.print("malformed-request")
        _ -> console.print("wrong-request-error")
    let secret = dynamic.dynamic(Vault("hidden"))
    console.print("sealed-fields-${list.length(dynamic.fields(dynamic.type_of(secret)))}")
    match dynamic.field(secret, "token"):
        Err(dynamic.SealedType(_)) -> console.print("sealed-denied")
        _ -> console.print("wrong-sealed-error")
    let anonymous = dynamic.dynamic(.{title: "Engineer", level: 7})
    match dynamic.field(anonymous, "title"):
        Ok(projected) ->
            let title: Result(String, dynamic.DynamicError) = dynamic.decode(projected)
            match title:
                Ok(value) -> console.print("anon-${value}")
                Err(_) -> console.print("anon-decode-error")
        Err(_) -> console.print("anon-projection-error")
    let cell = dynamic.dynamic(Cell(9))
    match dynamic.field(cell, "value"):
        Ok(projected) ->
            let number: Result(Int, dynamic.DynamicError) = dynamic.decode(projected)
            match number:
                Ok(value) -> console.print("generic-${value}")
                Err(_) -> console.print("generic-decode-error")
        Err(_) -> console.print("generic-projection-error")
"#,
    );
    let output = witchy_interp::interpreter::run_checked_module(checked, ".", Vec::new())
        .expect("run authenticated Dynamic field fixture");
    assert_eq!(
        output,
        [
            "name:String",
            "age:Int",
            "name-Ada",
            "missing-missing",
            "malformed-request",
            "sealed-fields-0",
            "sealed-denied",
            "anon-Engineer",
            "generic-9",
        ]
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
