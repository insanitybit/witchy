//! Source recipe identity and real admitted execution, independent of CLI grants.
use crate::{codegen, link_test_file};
use witchy_interp::pipeline::{check_bare_test_driver, check_integration_test_driver};

struct Files(std::path::PathBuf);
impl Files {
    fn new(source: &str) -> Self {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("witchy-integration-recipe-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("suite.witchy"), source).unwrap();
        Self(path)
    }
    fn source(&self) -> String { self.0.join("suite.witchy").to_str().unwrap().to_owned() }
}
impl Drop for Files {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[test]
fn integration_recipe_admits_executes_and_releases_without_unused_main_grants() {
    let files = Files::new(r#"
import testing
fn main(console: Console, root: Dir[Read]):
    console.print(root.read("must-not-run"))
fn test_values():
    let values = ["owned", "payload"]
    testing.assert_eq(values[1], "payload")
fn test_unselected():
    testing.fail_with("wrong entry")
"#);
    let (suite, _) = link_test_file(&files.source()).unwrap();
    let driver = check_integration_test_driver(&suite, "suite.test_values").unwrap();
    let artifact = codegen::compile_checked_test_driver_artifact(&driver).expect_lowered("integration admission");
    let entry = artifact.entry().unwrap();
    assert!(entry.capabilities().is_empty(), "unused main grants cannot survive");
    assert_eq!(entry.result(), witchy_wir::CanonicalEntryResult::Unit);
    assert!(!driver.canonical_source().unwrap().is_console_fixture());
    let validated = codegen::validate_checked_test_driver_for_oracle(&driver).unwrap();
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry.export().strip_prefix('f').unwrap().parse().unwrap(),
    );
    let oracle = witchy_wir::canonical_interpreter::interpret_entry_program(
        &validated, function, Vec::new(), &[], Default::default(),
    ).unwrap();
    assert!(oracle.output.is_empty());
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = crate::runtime::Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities::none(), crate::RUN_MEMORY_PAGES).unwrap();
    vm.run().unwrap();
    assert!(vm.output().is_empty());
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn integration_recipe_identity_rejects_cross_recipe_and_selected_source_witnesses() {
    let raw = "fn main():\n    let value = 0\nfn test_first():\n    let value = 1\nfn test_second():\n    let value = 2\n";
    let files = Files::new(raw);
    let (suite, _) = link_test_file(&files.source()).unwrap();
    let first = check_integration_test_driver(&suite, "suite.test_first").unwrap();
    let second = check_integration_test_driver(&suite, "suite.test_second").unwrap();
    let bare = check_bare_test_driver(&suite, "suite.test_first").unwrap();
    for different in [&second, &bare] {
        assert_ne!(first.identity().unwrap().derived_source_digest(), different.identity().unwrap().derived_source_digest());
        assert!(!first.canonical_source().unwrap().source_witness().verifies_closure_witness(
            different.canonical_source().unwrap().closure_witness(),
        ));
    }
    std::fs::write(files.source(), format!("// changed retained identity\n{raw}")).unwrap();
    let retained = check_integration_test_driver(&suite, "suite.test_first").unwrap();
    assert_eq!(first.identity().unwrap().derived_source_digest(), retained.identity().unwrap().derived_source_digest());
    let (fresh, _) = link_test_file(&files.source()).unwrap();
    let fresh = check_integration_test_driver(&fresh, "suite.test_first").unwrap();
    assert_ne!(first.identity().unwrap().original_source_digest(), fresh.identity().unwrap().original_source_digest());
    assert!(!first.canonical_source().unwrap().source_witness().verifies_closure_witness(
        fresh.canonical_source().unwrap().closure_witness(),
    ));
    let ast_only = witchy_types::pipeline::check_test_driver_module(suite.checked_module(), suite.module().clone()).unwrap();
    assert!(matches!(codegen::compile_checked_test_driver_artifact(&ast_only), codegen::LoweringOutcome::Unsupported(_)));
    assert!(codegen::validate_checked_test_driver_for_oracle(&ast_only).is_err());
}

#[test]
fn integration_recipe_refuses_foreign_targets_data_and_non_unit_results() {
    let files = Files::new("import sibling\nfn test_data(value: Int):\n    let used = value\nfn test_result():\n    42\n");
    std::fs::write(files.0.join("sibling.witchy"), "pub fn test_other():\n    let value = 1\n").unwrap();
    let (suite, _) = link_test_file(&files.source()).unwrap();
    for target in ["sibling.test_other", "suite.test_missing", "suite.test_other"] {
        assert!(check_integration_test_driver(&suite, target).unwrap_err().to_string().contains("entry"));
    }
    let data = check_integration_test_driver(&suite, "suite.test_data").unwrap_err().to_string();
    assert!(data.contains("Console, Dir or Net") || data.contains("main"), "{data}");
    let result = check_integration_test_driver(&suite, "suite.test_result").unwrap_err().to_string();
    assert!(result.contains("expected `()`"), "{result}");
}
