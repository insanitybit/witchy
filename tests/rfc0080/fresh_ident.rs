//! RFC-0080 hygiene seam: compiler-owned fresh identifiers are deterministic,
//! source-unspellable, and distinct across calls and expansion evaluators.

use witchy::ast;
use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};

const SOURCE: &str = r#"
import meta

comptime:
    let int = quote type:
        Int
    let left = meta.fresh("tmp")
    let right = meta.fresh("tmp")
    let body = quote expr:
        ${meta.expr_name(left)} + ${meta.expr_name(right)}
    emit_item(meta.function(true, meta.ident("sum_fresh"), [meta.param(left, int), meta.param(right, int)], Some(int), body))

comptime:
    let int = quote type:
        Int
    let value = meta.fresh("tmp")
    emit_item(meta.function(true, meta.ident("identity_fresh"), [meta.param(value, int)], Some(int), meta.expr_name(value)))

comptime fn fresh_tag(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    let _unused = meta.fresh("tag_local")
    quote expr:
        7

comptime fn second_tag(parts: List(String), holes: List(meta.TagHole)) -> meta.ExprSyntax:
    let _unused = meta.fresh("tag_local")
    quote expr:
        9

fn main(console: Console):
    console.print("${sum_fresh(5, 7)}")
    console.print("${identity_fresh(9)}")
    console.print("${fresh_tag"a" * 10 + second_tag"b"}")
"#;

fn checked(source: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    let workspace = PackageCoordinate::new(PackageSource::Workspace, "test/fresh-ident", "1").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", "1").unwrap();
    let mut assignments = vec![("main".to_owned(), ModuleLoadIdentity::new(workspace, ["main"]).unwrap())];
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| (
        (*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap(),
    )));
    let owners = AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap();
    witchy::canonical_comptime::check_source_bundle_with_local_tag(
        vec![("main".into(), source.as_bytes().to_vec())], "main",
        ["main".to_owned()].into_iter().collect(), owners,
        witchy::canonical_comptime::source_limits(),
    )
}

fn fresh_parameter_names(module: &ast::Module) -> Vec<String> {
    let mut names = Vec::new();
    for item in &module.items {
        let ast::Item::Function(function) = item else {
            continue;
        };
        if function.name.ends_with("sum_fresh") || function.name.ends_with("identity_fresh") {
            names.extend(function.params.iter().map(|param| param.name.clone()));
        }
    }
    names
}

#[test]
fn fresh_identifiers_are_deterministic_distinct_and_backend_neutral() {
    let first = checked(SOURCE).expect("authenticated fresh-name program");
    let names = fresh_parameter_names(first.module());
    assert_eq!(names.len(), 3, "expected both generated functions: {names:?}");
    assert!(
        names.iter().all(|name| name.starts_with("__witchy_fresh_")),
        "fresh names must live in the source-reserved compiler namespace: {names:?}"
    );
    assert_ne!(names[0], names[1], "two calls in one evaluator must be distinct");
    assert_ne!(names[0], names[2], "separate comptime blocks must have distinct scopes");
    assert_eq!(
        names,
        fresh_parameter_names(checked(SOURCE).expect("repeat authenticated expansion").module()),
        "fresh-name allocation must be reproducible for identical source"
    );

    struct ResetOptimization;
    impl Drop for ResetOptimization {
        fn drop(&mut self) { witchy::opt::set_for_tests(None); }
    }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    let artifact = witchy::codegen::compile_checked_module_artifact(&first)
        .expect_lowered("admit fresh-name program");
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut actor = runtime.spawn_admitted(&artifact, witchy::runtime::Capabilities {
        print: true, quiet: true, ..witchy::runtime::Capabilities::none()
    }, 128).expect("spawn admitted fresh-name program");
    actor.run().expect("run admitted fresh-name program");
    assert_eq!(actor.output(), ["12", "9", "79"]);
    assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn fresh_rejects_an_invalid_identifier_hint() {
    let source = r#"
import meta

comptime:
    let _name = meta.fresh("bad-name")

fn main(console: Console):
    console.print("unreachable")
"#;
    let error = checked(source)
        .expect_err("meta.fresh must validate its human-readable hint");
    assert!(error.contains("meta.fresh") && error.contains("bad-name"), "{error}");
}

#[test]
fn source_cannot_spoof_an_admitted_fresh_parameter() {
    let source = "fn forged(__witchy_fresh_00_0_tmp: Int) -> Int:\n    __witchy_fresh_00_0_tmp\nfn main() -> Int:\n    forged(7)\n";
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("user source cannot claim an admitted generated identifier");
    assert!(error.to_string().contains("reserved for the compiler"), "{error}");
}

#[test]
fn second_tag_with_holes_refuses_after_the_first_admitted_tag() {
    let source = SOURCE.replace("second_tag\"b\"", "second_tag\"b${1}\"");
    let error = checked(&source).expect_err("a later tag with holes cannot enter the static route");
    assert!(error.contains("one static part and no holes"), "{error}");
}
