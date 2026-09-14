//! Existing offline PM handlers execute through compiler-owned source entries.
use std::process::{Command, Output};

#[path = "../support/temp_dir.rs"]
mod temp_dir;
use temp_dir::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn run(root: &TempDir, args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(root.path())
        .env("WITCHY_HOME", root.path().join("home"))
        .env_remove("COVEN_URL")
        .args(args)
        .output()
        .expect("execute real embedded PM")
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn offline_pm_test_executes_checked_test_body() {
    let root = TempDir::new("offline-pm-test");
    root.write("suite.witchy", "import testing\nfn test_checked_body():\n    testing.assert_int_eq(21 * 2, 42)\n");
    let output = run(&root, &["pm", "test", "suite.witchy"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert!(text(&output).contains("test suite.test_checked_body ... ok"), "{}", text(&output));
}

#[test]
fn offline_pm_test_preserves_terminal_child_failure() {
    let root = TempDir::new("offline-pm-test-abort");
    root.write("suite.witchy", "fn test_abort():\n    fail(\"offline-pm-body-executed\")\n");
    let output = run(&root, &["pm", "test", "suite.witchy"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("offline-pm-body-executed"), "{}", text(&output));
    assert!(text(&output).contains("test suite.test_abort"), "{}", text(&output));
    assert!(!text(&output).contains("test suite.test_abort ... ok"), "{}", text(&output));
}

#[test]
fn offline_pm_test_does_not_delegate_its_project_dir_to_test_parameters() {
    let root = TempDir::new("offline-pm-test-grants");
    root.write("suite.witchy", "fn test_real_dir(root: Dir[Read]):\n    let _ = root.read(\"private.txt\")\n");
    root.write("private.txt", "not an implicit test grant");
    let output = run(&root, &["pm", "test", "suite.witchy"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("--integration"), "{}", text(&output));
    assert!(!text(&output).contains("test suite.test_real_dir ... ok"), "{}", text(&output));
}

#[test]
fn offline_pm_deps_prints_existing_manifest_contract() {
    let root = TempDir::new("offline-pm-deps");
    root.write("app/witchy.toml", "[rune]\nname = \"app\"\nversion = \"1.0.0\"\n[dependencies]\nhelper = { path = \"../helper\" }\n");
    let output = run(&root, &["pm", "deps", "app"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "helper -> path:../helper\n");
    assert!(output.stderr.is_empty(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn offline_pm_tree_preserves_transitive_path_traversal() {
    let root = TempDir::new("offline-pm-tree");
    root.write("app/witchy.toml", "[rune]\nname = \"app\"\nversion = \"1.0.0\"\n[dependencies]\nhelper = { path = \"../helper\" }\n");
    root.write("helper/witchy.toml", "[rune]\nname = \"helper\"\nversion = \"1.0.0\"\n[dependencies]\nleaf = { path = \"../leaf\" }\n");
    root.write("leaf/witchy.toml", "[rune]\nname = \"leaf\"\nversion = \"1.0.0\"\n[dependencies]\n");
    let output = run(&root, &["pm", "tree", "app"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "app\n  helper (path:../helper)\n    leaf (path:../leaf)\n");
    assert!(output.stderr.is_empty(), "{}", String::from_utf8_lossy(&output.stderr));
}
