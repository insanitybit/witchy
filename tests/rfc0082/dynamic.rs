use witchy::runtime::{Capabilities, Runtime};
use witchy::codegen;

use super::authenticated::{checked_result, checked_source_result};

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    checked_result(source).expect("authenticated checked link")
}

fn checked_source(source: &str) -> witchy_types::pipeline::CheckedModule {
    checked_source_result(source).expect("authenticated exact-source link")
}

#[test]
fn derived_public_field_projection_and_missing_interpret_authenticated() {
    let checked = checked(
        r#"
import dynamic
import reflect

type User derive(Reflect):
    name: String
    age: Int

fn main(console: Console):
    let user = dynamic.dynamic(User("Ada", 42))
    match dynamic.field(user, "name"):
        Ok(field) ->
            let value: Option(String) = dynamic.try_decode(field)
            match value:
                Some(name) -> console.print("name-${name}")
                None -> console.print("decode-error")
        Err(_) -> console.print("projection-error")
    match dynamic.field(user, "missing"):
        Err(dynamic.MissingField(name)) -> console.print("missing-${name}")
        _ -> console.print("wrong-missing-error")
"#,
    );
    let interpreted = witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret authenticated Dynamic fields");
    assert_eq!(interpreted, ["name-Ada", "missing-missing"]);
}

#[test]
fn derived_public_field_projection_runs_on_wasm() {
    let checked = checked_source(
        r#"
import dynamic
import reflect

type User derive(Reflect):
    name: String
    age: Int

sealed type Vault derive(Reflect):
    token: String

fn main(console: Console):
    let person = dynamic.dynamic(User("Ada", 42))
    for info in dynamic.fields(dynamic.type_of(person)):
        console.print(dynamic.field_name(info) + ":" + dynamic.type_name(dynamic.field_type(info)))
    match dynamic.field(person, "name"):
        Ok(projected) ->
            let decoded: Result(String, dynamic.DynamicError) = dynamic.decode(projected)
            match decoded:
                Ok(name) -> console.print("field-${name}")
                Err(_) -> console.print("decode-error")
        Err(_) -> console.print("projection-error")
    match dynamic.field(person, "missing"):
        Err(dynamic.MissingField(name)) -> console.print("missing-${name}")
        _ -> console.print("wrong-missing-error")
    let vault = dynamic.dynamic(Vault("hidden"))
    match dynamic.field(vault, "token"):
        Err(dynamic.SealedType(_)) -> console.print("sealed-denied")
        _ -> console.print("wrong-sealed-error")
"#,
    );
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit authenticated Dynamic fields");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, quiet: true, ..Default::default() },
            128,
        )
        .expect("spawn");
    actor.run().expect("run compiled Dynamic fields");
    assert_eq!(
        actor.output(),
        ["name:String", "age:Int", "field-Ada", "missing-missing", "sealed-denied"]
    );
}

#[test]
fn mixed_explicit_and_inferred_reflection_agrees_on_both_backends() {
    let checked = checked_source(
        r#"
import dynamic
import reflect

type Mixed(a) derive(Reflect):
    first: a
    second: b

fn main(console: Console):
    let mixed: Mixed(String, Bool) = Mixed("mixed", true)
    let packed = dynamic.dynamic(mixed)
    let decoded: Option(Mixed(String, Bool)) = dynamic.try_decode(packed)
    match decoded:
        Some(found) -> console.print("${found.first}-${found.second}")
        None -> console.print("decode failed")
"#,
    );

    let interpreted = witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("run authenticated Dynamic reflection on interpreter");
    assert_eq!(interpreted, ["mixed-true"]);

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit mixed explicit and inferred reflection");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, quiet: true, ..Default::default() },
            128,
        )
        .expect("spawn");
    actor.run().expect("run compiled Dynamic reflection");
    assert_eq!(actor.output(), ["mixed-true"]);
}
