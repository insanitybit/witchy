//! RFC-0077 real-capability test tier: CLI grants stay explicit and package-owned.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "witchy-rfc0077-{tag}-{}-{sequence}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&path).expect("create temp directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn mkdir(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(&path).expect("create nested directory");
        path
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create file parent");
        }
        std::fs::write(&path, source).expect("write fixture");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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
        "import testing\n\nfn test_reads_real_file(root: Dir[Read]):\n    testing.assert_eq(root.read(\"secret.txt\"), \"integration-secret\")\n",
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
fn mock_capabilities_work_in_both_test_tiers_without_real_grants() {
    let temp = TempDir::new("mocks");
    let suite = temp.write(
        "mock_suite.witchy",
        "import testing\n\nfn test_mock_dir():\n    let root = testing.mock_dir([(\"config.txt\", \"mocked\")])\n    testing.assert_eq(root.read(\"config.txt\"), \"mocked\")\n",
    );

    for integration in [false, true] {
        let mut command = vec![OsString::from("test")];
        if integration {
            command.push(OsString::from("--integration"));
        }
        command.push(suite.clone().into_os_string());
        let output = run(command);
        assert!(output.status.success(), "mock failed with integration={integration}: {}", text(&output));
    }
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
        "import testing\n\nfn test_dependency_reads_real_file(root: Dir[Read]):\n    testing.assert_eq(root.read(\"secret.txt\"), \"dependency-must-not-read-this\")\n",
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
        "pub fn answer() -> Int:\n    42\n\npub fn effectful(root: Dir[Read]) -> String:\n    root.read(\"secret.txt\")\n",
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
