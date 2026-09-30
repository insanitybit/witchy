//! RFC-0080 compiler-owned expression payloads with compatibility projection.

use witchy::{format, parser};
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

const SOURCE: &str = r#"
import meta

fn identity(x: Int) -> Int:
    x

comptime fn generated_item() -> meta.ItemSyntax:
    let callee = quote expr:
        identity
    let argument = quote expr:
        42
    let call = meta.expr_call(callee, [argument])
    quote item:
        pub fn generated() -> Int:
            ${call}

comptime:
    emit_item(generated_item())

fn main(console: Console):
    console.print("${generated()}")
"#;

#[test]
fn owned_expressions_survive_direct_flow_and_project_for_builders() {
    let parsed = parser::parse_module(SOURCE).expect("parse compiler-owned expressions");
    assert_eq!(parsed.compiler_expr_syntax.len(), 2);
    assert_eq!(parsed.compiler_item_syntax.len(), 1);

    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/owned-expr", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let mut assignments = vec![("main".to_owned(), ModuleLoadIdentity::new(workspace, ["main"]).unwrap())];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
    )));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap();
    let (_checked, expanded_for_tooling) = witchy::canonical_comptime::check_and_expand_source_bundle_with_imported_blocks(
        vec![("main".into(), SOURCE.as_bytes().to_vec())], "main", "main",
        ["main".to_owned()].into_iter().collect(), owners,
        witchy::canonical_comptime::source_limits(),
    ).expect("expand compiler-owned expressions through authenticated source");
    let expanded_source = format::module(&expanded_for_tooling, &[]);
    assert!(expanded_source.contains("pub fn generated() -> Int:"), "{expanded_source}");
    assert!(expanded_source.contains("identity(42)"), "{expanded_source}");
    assert!(!expanded_source.contains("@quote_expr"), "{expanded_source}");

    let expected = vec!["42".to_string()];
    super::assert_admitted_source_output(SOURCE, &expected, "compile expanded program", 64);
}
