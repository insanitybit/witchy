//! Bare-driver vertical and proof-boundary probes. Fixture/integration launch
//! recipes are intentionally not represented by the bare source recipe.
use crate::{codegen, link_test_file};
use witchy_interp::pipeline::check_bare_test_driver;

struct Files(std::path::PathBuf);
impl Files {
    fn new(source: &str) -> Self {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("witchy-canonical-bare-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("suite.witchy"), source).unwrap();
        Self(directory)
    }
    fn path(&self) -> String { self.0.join("suite.witchy").to_str().unwrap().to_owned() }
}
impl Drop for Files {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[test]
fn canonical_bare_driver_admits_and_executes_with_independent_oracle() {
    let files = Files::new(r#"
import testing
fn identity(values: List(String)) -> List(String):
    values
fn main(console: Console):
    console.print("production main must not run")
fn test_good():
    let words = identity(["kept"])
    testing.assert_int_eq(words[0].length(), 4)
fn test_other():
    testing.fail_with("unselected test must not run")
"#);
    let (suite, _) = link_test_file(&files.path()).expect("authenticated raw suite");
    let driver = check_bare_test_driver(&suite, "suite.test_good").expect("compiler-owned recipe");
    let identity = driver.identity().expect("recipe identity");
    assert_eq!(identity.recipe_version(), 3);
    assert_eq!(identity.original_source_digest(), suite.original_source_digest());
    let source = driver.canonical_source().expect("fresh canonical source");
    assert_eq!(source.source_witness().source_bundle_digest(), identity.derived_source_digest());
    assert_ne!(identity.original_source_digest(), identity.derived_source_digest());
    let artifact = codegen::compile_checked_test_driver_artifact(&driver).expect_lowered("bare driver admission");
    // This is a second lowering/validation, not the encoder's graph or bytes.
    let validated = codegen::validate_checked_test_driver_for_oracle(&driver).expect("independent validation");
    let entry = artifact.entry().expect("admitted entry");
    assert_eq!(entry.result(), witchy_wir::CanonicalEntryResult::Unit);
    assert!(entry.capabilities().is_empty(), "discarded main grants must not survive");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap(),
    );
    let oracle = witchy_wir::canonical_interpreter::interpret_entry_program(
        &validated, function, Vec::new(), &[], Default::default(),
    ).expect("canonical oracle executes the selected test");
    assert!(oracle.output.is_empty());
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = crate::runtime::Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities::none(), crate::RUN_MEMORY_PAGES)
        .expect("admitted zero-grant spawn");
    vm.run().expect("selected test actually executes");
    assert!(vm.output().is_empty());
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn canonical_bare_driver_refuses_inferred_non_nil_results() {
    for body in ["1", "\"value\"", "true"] {
        let files = Files::new(&format!("fn test_value():\n    {body}\n"));
        let (suite, _) = link_test_file(&files.path()).expect("source with inferred result checks");
        let error = check_bare_test_driver(&suite, "suite.test_value")
            .expect_err("the generated Nil entry cannot discard an inferred result").to_string();
        assert!(error.contains("expected `()`"), "{body}: {error}");
    }
}

#[test]
fn canonical_bare_driver_refuses_unknown_imported_and_parameterized_targets() {
    let files = Files::new(r#"
import dependency
fn test_good():
    let value = 0
fn test_parameter(value: Int):
    let used = value
async fn test_async():
    let value = 1
"#);
    std::fs::write(files.0.join("dependency.witchy"), "pub fn test_foreign():\n    let value = 9\n").unwrap();
    let (suite, _) = link_test_file(&files.path()).expect("original source checks");
    for target in ["suite.test_missing", "dependency.test_foreign", "suite.test_foreign", "suite.test_parameter"] {
        let error = check_bare_test_driver(&suite, target).expect_err(target).to_string();
        assert!(error.contains("test") && (error.contains("unsupported") || error.contains("entry")), "{target}: {error}");
    }
    let async_driver = check_bare_test_driver(&suite, "suite.test_async")
        .expect("bare async tests use the compiler-owned completion recipe");
    assert_eq!(async_driver.identity().unwrap().selected_declaration(), "suite.test_async");
}

#[test]
fn canonical_bare_driver_identity_binds_retained_bytes_and_selected_recipe() {
    let source = "fn test_first():\n    let value = 1\nfn test_second():\n    let value = 2\n";
    let files = Files::new(source);
    let (suite, _) = link_test_file(&files.path()).unwrap();
    // Discovery's retained bytes, not a subsequent filesystem read, own the recipe.
    std::fs::write(files.0.join("suite.witchy"), format!("// different raw source\n{source}")).unwrap();
    let first = check_bare_test_driver(&suite, "suite.test_first").unwrap();
    let again = check_bare_test_driver(&suite, "suite.test_first").unwrap();
    let second = check_bare_test_driver(&suite, "suite.test_second").unwrap();
    assert_eq!(first.identity().unwrap().derived_source_digest(), again.identity().unwrap().derived_source_digest());
    assert_ne!(first.identity().unwrap().derived_source_digest(), second.identity().unwrap().derived_source_digest());
    assert_ne!(first.identity().unwrap().declaration_ordinal(), second.identity().unwrap().declaration_ordinal());
    let (changed, _) = link_test_file(&files.path()).unwrap();
    let changed = check_bare_test_driver(&changed, "suite.test_first").unwrap();
    assert_ne!(first.identity().unwrap().original_source_digest(), changed.identity().unwrap().original_source_digest());
    assert_ne!(first.identity().unwrap().derived_source_digest(), changed.identity().unwrap().derived_source_digest());
    assert!(!first.canonical_source().unwrap().source_witness().verifies_closure_witness(
        second.canonical_source().unwrap().closure_witness(),
    ), "closure witnesses must be freshly bound to this recipe, never copied");
}

#[test]
fn canonical_bare_driver_does_not_upgrade_ast_only_driver_authority() {
    let files = Files::new("fn main():\n    let value = 0\nfn test_good():\n    let value = 1\n");
    let (suite, _) = link_test_file(&files.path()).unwrap();
    assert!(suite.checked_module().canonical_source().is_none());
    let ast_only = witchy_types::pipeline::check_test_driver_module(
        suite.checked_module(), suite.module().clone(),
    ).expect("legacy AST-only driver checks for evaluation");
    assert!(ast_only.canonical_source().is_none());
    assert!(ast_only.identity().is_none());
    assert!(matches!(codegen::compile_checked_test_driver_artifact(&ast_only), codegen::LoweringOutcome::Unsupported(_)));
    assert!(codegen::validate_checked_test_driver_for_oracle(&ast_only).is_err());
    assert!(matches!(codegen::compile_checked_test_driver_binary(&ast_only), codegen::LoweringOutcome::Unsupported(_)),
        "legacy fixture-byte path stays closed");
}
