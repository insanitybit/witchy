//! RFC-0082 owned Dynamic boundary parity for borrowed, unique, and shared data.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter, parser, pipeline};
use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

fn checked(source: &str) -> witchy_types::pipeline::CheckedModule {
    let module = parser::parse_module(source).expect("parse Dynamic ownership fixture");
    let workspace = PackageCoordinate::new(
        PackageSource::Workspace,
        "example/dynamic-ownership-parity",
        "0.1.0",
    )
    .expect("workspace package coordinate");
    let toolchain = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/stdlib",
        "0.1.0",
    )
    .expect("toolchain package coordinate");
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(workspace, ["main"]).expect("main module owner"),
    )];
    assignments.extend(witchy::linker::STD_MODULES.iter().map(|std_module| {
        (
            (*std_module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *std_module])
                .expect("std module owner"),
        )
    }));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .expect("authenticated module owners");
    pipeline::link_checked_authenticated(vec![("main".to_string(), module)], "main", owners)
        .expect("authenticated checked link")
}

#[test]
fn materialized_borrows_and_owned_values_match_between_backends() {
    let source = r#"
mode opt

import borrow
import dynamic

fn view(text: let('a) String) -> View(String, 'a):
    text

fn main(console: Console):
    var owner = "borrowed"
    let borrowed = view(owner)
    let borrowed_packed = dynamic.dynamic(borrowed.owned())
    let borrowed_decoded: Option(String) = dynamic.try_decode(borrowed_packed)
    match borrowed_decoded:
        Some(text) -> console.print("materialized-${text}")
        None -> console.print("materialized-decode-failed")

    let unique_values: unique List(Int) = [1, 2, 3]
    let unique_packed = dynamic.dynamic(unique_values)
    let unique_decoded: Option(List(Int)) = dynamic.try_decode(unique_packed)
    match unique_decoded:
        Some(values) -> console.print("unique-${list.length(values)}")
        None -> console.print("unique-decode-failed")

    let shared = "shared"
    let alias = shared
    let shared_packed = dynamic.dynamic(shared)
    console.print("alias-${alias}")
    let shared_decoded: Option(String) = dynamic.try_decode(shared_packed)
    match shared_decoded:
        Some(text) -> console.print("shared-${text}")
        None -> console.print("shared-decode-failed")
"#;
    let checked = checked(source);
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret Dynamic ownership fixture");
    let expected = ["materialized-borrowed", "unique-3", "alias-shared", "shared-shared"];
    assert_eq!(interpreted, expected);

    let wasm = codegen::compile_checked_module_binary(&checked)
        .expect_lowered("compile Dynamic ownership fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn compiled Dynamic ownership fixture");
    actor.run().expect("run compiled Dynamic ownership fixture");
    assert_eq!(actor.output(), expected);
}
