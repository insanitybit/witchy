//! RFC-0077 real-capability test tier: CLI grants stay explicit and package-owned.

use std::ffi::OsString;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

use super::temp_dir::TempDir;

fn run(args: impl IntoIterator<Item = OsString>) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn real_dir_access_requires_integration_and_an_explicit_grant() {
    let temp = TempDir::new("dir");
    let granted = temp.mkdir("granted");
    std::fs::write(granted.join("secret.txt"), "integration-secret").expect("seed real file");
    let suite = temp.write(
        "suite.witchy",
        "import testing\n\nfn test_reads_real_file(root: Dir[Read]):\n    testing.assert_eq(root.read(\"secret.txt\") ?? \"\", \"integration-secret\")\n",
    );

    let plain = run([OsString::from("test"), suite.clone().into_os_string()]);
    assert!(!plain.status.success(), "plain test unexpectedly received authority: {}", text(&plain));
    assert!(text(&plain).contains("--integration"), "plain failure must name the opt-in: {}", text(&plain));

    let missing = run([
        OsString::from("test"),
        OsString::from("--integration"),
        suite.clone().into_os_string(),
    ]);
    assert!(!missing.status.success(), "missing Dir grant unexpectedly passed: {}", text(&missing));
    assert!(
        text(&missing).contains("requires 1 `Dir` grant") && text(&missing).contains("--dir <root>"),
        "missing grant must be actionable: {}",
        text(&missing)
    );

    let implicit = run([
        OsString::from("test"),
        OsString::from("--dir"),
        granted.clone().into_os_string(),
        suite.clone().into_os_string(),
    ]);
    assert!(!implicit.status.success(), "--dir without --integration unexpectedly passed");
    assert!(text(&implicit).contains("require `witchy test --integration`"), "{}", text(&implicit));

    let integrated = run([
        OsString::from("test"),
        OsString::from("--integration"),
        OsString::from("--dir"),
        granted.into_os_string(),
        suite.into_os_string(),
    ]);
    assert!(integrated.status.success(), "explicit integration grant failed: {}", text(&integrated));
    assert!(text(&integrated).contains("test suite.test_reads_real_file ... ok"), "{}", text(&integrated));
}

#[test]
fn net_parameters_require_an_explicit_allowlist() {
    let temp = TempDir::new("net");
    let suite = temp.write(
        "net_suite.witchy",
        "import testing\n\nfn test_receives_net(net: Net[Connect, Tcp]):\n    testing.assert(true, \"Net parameter was forwarded\")\n",
    );

    let missing = run([
        OsString::from("test"),
        OsString::from("--integration"),
        suite.clone().into_os_string(),
    ]);
    assert!(!missing.status.success(), "missing Net allowlist unexpectedly passed: {}", text(&missing));
    assert!(text(&missing).contains("requires a `Net` grant") && text(&missing).contains("--net <addr>"), "{}", text(&missing));

    let granted = run([
        OsString::from("test"),
        OsString::from("--integration"),
        OsString::from("--net"),
        OsString::from("127.0.0.1:9"),
        suite.into_os_string(),
    ]);
    assert!(granted.status.success(), "explicit Net allowlist failed: {}", text(&granted));
}

#[test]
fn resolved_dependency_tests_receive_zero_real_grants() {
    let temp = TempDir::new("dependency");
    let granted = temp.mkdir("granted");
    std::fs::write(granted.join("secret.txt"), "dependency-must-not-read-this")
        .expect("seed protected file");
    temp.write(
        "witchy.toml",
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ndep = \"1.0.0\"\n",
    );
    temp.write(
        "witchy.lock",
        "[[rune]]\nname = \"dep\"\nalias = \"dep\"\nversion = \"1.0.0\"\nsource = \"coven\"\nhash = \"sha256:test\"\nruntime_footprint = []\n",
    );
    temp.write(
        "src/app.witchy",
        "import testing\n\nfn test_owned_logic():\n    testing.assert_int_eq(2 + 2, 4)\n",
    );
    temp.write(
        "vendor/dep/witchy.toml",
        "[rune]\nname = \"dep\"\nversion = \"1.0.0\"\n\n[dependencies]\n",
    );
    temp.write(
        "vendor/dep/src/dep.witchy",
        "import testing\n\nfn test_dependency_reads_real_file(root: Dir[Read]):\n    testing.assert_eq(root.read(\"secret.txt\") ?? \"\", \"dependency-must-not-read-this\")\n",
    );

    let output = run([
        OsString::from("test"),
        OsString::from("--integration"),
        OsString::from("--dir"),
        granted.into_os_string(),
        temp.path().as_os_str().to_owned(),
    ]);
    let diagnostic = text(&output);
    assert!(!output.status.success(), "dependency inherited the caller grant: {diagnostic}");
    assert!(diagnostic.contains("dependency tests receive zero real grants"), "{diagnostic}");
    assert!(diagnostic.contains("test app.test_owned_logic ... ok"), "owned test should still run: {diagnostic}");
}

#[test]
fn linked_library_capabilities_do_not_widen_a_nullary_test_entry() {
    let temp = TempDir::new("linked");
    temp.write(
        "helper.witchy",
        "pub fn answer() -> Int:\n    42\n\npub fn effectful(root: Dir[Read]) -> Result(String, CapError):\n    root.read(\"secret.txt\")\n",
    );
    let suite = temp.write(
        "suite.witchy",
        "import helper\nimport testing\n\nfn test_linked_pure_path():\n    testing.assert_int_eq(helper.answer(), 42)\n",
    );
    let output = run([
        OsString::from("test"),
        OsString::from("--integration"),
        suite.into_os_string(),
    ]);
    assert!(output.status.success(), "linked library widened or broke the entry grant: {}", text(&output));
}

#[test]
fn source_integration_recipe_runs_selected_console_capture_and_reports_abort() {
    let temp = TempDir::new("integration-recipe-console");
    let suite = temp.write("suite.witchy", r#"import testing
fn main(console: Console):
    console.print("wrong production entry")
fn test_capture(console: Console):
    let emit = fn(): console.print("selected capture")
    emit()
fn test_abort(console: Console):
    console.print("before integration abort")
    testing.fail_with("integration recipe abort")
"#);
    let success = run([
        "test".into(), "--integration".into(), "--show-output".into(),
        "--filter".into(), "test_capture".into(), suite.clone().into_os_string(),
    ]);
    assert!(success.status.success(), "{}", text(&success));
    assert!(text(&success).contains("selected capture"), "{}", text(&success));
    assert!(!text(&success).contains("wrong production entry"), "{}", text(&success));
    let failure = run([
        "test".into(), "--integration".into(), "--filter".into(), "test_abort".into(),
        suite.into_os_string(),
    ]);
    assert!(!failure.status.success(), "{}", text(&failure));
    assert!(text(&failure).contains("integration recipe abort"), "{}", text(&failure));
}

#[test]
fn source_integration_async_recipe_drives_body_and_preserves_its_abort() {
    // Keep the success and abort outcomes in one integration-suite invocation
    // so this checks both compiled test-driver results together.
    let temp = TempDir::new("integration-recipe-async");
    let suite = temp.write("suite.witchy", r#"import task
import chan
import testing
async fn test_pass():
    let (tx, rx) = chan.channel(1).await
    chan.send(tx, 42).await
    chan.send(tx, 43).await
    let value = chan.recv(rx).await
    let next = chan.recv(rx).await
    task.yield_now().await
    testing.assert_eq("${value}", "Some(42)")
    testing.assert_eq("${next}", "Some(43)")
async fn test_abort():
    task.yield_now().await
    testing.fail_with("async integration body ran")
"#);
    let output = run(["test".into(), "--integration".into(), suite.into_os_string()]);
    let text = text(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("test suite.test_pass ... ok"), "{text}");
    assert!(text.contains("test suite.test_abort ... FAILED"), "{text}");
    assert!(text.contains("async integration body ran"), "{text}");
    assert!(text.contains("1 passed; 1 failed"), "{text}");
}
