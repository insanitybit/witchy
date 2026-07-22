//! RFC-0082 ownership boundary conformance against RFC-0083 loans.

use witchy_types::runtime_type::{
    AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource,
};

fn authenticated(source: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    let module = witchy_syntax::parser::parse_module(source).map_err(|error| error.to_string())?;
    let workspace = PackageCoordinate::new(
        PackageSource::Workspace,
        "example/dynamic-ownership-test",
        "0.1.0",
    )
    .map_err(|error| error.to_string())?;
    let toolchain = PackageCoordinate::new(
        PackageSource::Toolchain,
        "witchy/stdlib",
        "0.1.0",
    )
    .map_err(|error| error.to_string())?;
    let mut assignments = vec![(
        "main".to_string(),
        ModuleLoadIdentity::new(workspace, ["main"]).map_err(|error| error.to_string())?,
    )];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|std_module| {
        (
            (*std_module).to_string(),
            ModuleLoadIdentity::new(toolchain.clone(), ["std", *std_module])
                .expect("valid std module owner"),
        )
    }));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments)
        .map_err(|error| error.to_string())?;
    witchy_interp::pipeline::link_checked_authenticated(
        vec![("main".to_string(), module)],
        "main",
        owners,
    )
    .map_err(|error| error.to_string())
}

#[test]
fn borrowed_views_require_explicit_materialization_before_dynamic_storage() {
    let source = r#"
mode opt

import dynamic

fn view(text: let('a) String) -> View(String, 'a):
    text

fn main(console: Console):
    var owner = "borrowed"
    let borrowed = view(owner)
    let packed = dynamic.dynamic(borrowed)
    console.print(dynamic.type_name(dynamic.type_of(packed)))
"#;
    let error = authenticated(source).expect_err("borrowed Dynamic payload must be rejected");
    assert!(error.contains("cannot be stored in Dynamic"), "{error}");
    assert!(error.contains(".owned()"), "{error}");
}

#[test]
fn dynamic_construction_consumes_unique_payloads() {
    let source = r#"
import dynamic

fn main(console: Console):
    let values: unique List(Int) = [1, 2]
    let packed = dynamic.dynamic(values)
    console.print("${list.length(values)}")
    console.print(dynamic.type_name(dynamic.type_of(packed)))
"#;
    let error = authenticated(source).expect_err("Dynamic must consume its owned payload");
    assert!(error.contains("after it was moved"), "{error}");
}
