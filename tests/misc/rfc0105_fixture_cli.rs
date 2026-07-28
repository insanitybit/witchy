//! RFC-0105 fixture CLI: parity-by-default execution and deterministic CI UX.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use witchy_testkit::{
    canonical_plan_json, ConsoleFixture, FilesystemEntry, FilesystemFixture,
    FixturePlan, RandFixture,
};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

use super::temp_dir::TempDir;

trait FixturePlanFile {
    fn write_plan(&self, plan: &FixturePlan) -> PathBuf;
}

impl FixturePlanFile for TempDir {
    fn write_plan(&self, plan: &FixturePlan) -> PathBuf {
        self.write(
            "fixtures.json",
            canonical_plan_json(plan).expect("canonical fixture plan"),
        )
    }
}

fn run(args: &[&Path]) -> Output {
    let mut command = Command::new(BIN);
    for argument in args {
        command.arg(argument);
    }
    command.output().expect("spawn witchy fixture CLI")
}

fn text(output: &[u8]) -> String {
    String::from_utf8_lossy(output).into_owned()
}

fn diagnostic(output: &Output) -> String {
    format!("{}{}", text(&output.stdout), text(&output.stderr))
}

fn assert_sealed_rejected(output: &Output, path: &str) {
    let message = diagnostic(output);
    assert!(
        !output.status.success(),
        "{path} unexpectedly inherited test-only sealed construction authority: {message}"
    );
    assert!(
        message.contains("sealed type") && message.contains("Version"),
        "{path} failed for the wrong reason: {message}"
    );
}

fn console_plan() -> FixturePlan {
    FixturePlan {
        console: Some(ConsoleFixture::default()),
        ..FixturePlan::default()
    }
}

#[test]
fn list_and_filter_select_tests_without_running_unselected_bodies() {
    let temp = TempDir::new("selection");
    let suite = temp.write(
        "suite.witchy",
        "fn test_alpha(console: Console):\n    console.print(\"alpha-output\")\n\n\
         fn test_beta(console: Console):\n    console.print(\"beta-output\")\n",
    );
    let plan = temp.write_plan(&console_plan());

    let filtered = run(&[
        Path::new("test"),
        Path::new("--fixtures"),
        &plan,
        Path::new("--filter"),
        Path::new("alpha"),
        Path::new("--show-output"),
        &suite,
    ]);
    let filtered_text = text(&filtered.stdout);
    assert!(
        filtered.status.success(),
        "filtered fixture run failed: {filtered_text}{}",
        text(&filtered.stderr)
    );
    assert!(filtered_text.contains("test suite.test_alpha ... ok"));
    assert!(filtered_text.contains("  alpha-output"));
    assert!(!filtered_text.contains("test_beta"));
    assert!(!filtered_text.contains("beta-output"));

    let listed = run(&[
        Path::new("test"),
        Path::new("--fixtures"),
        &plan,
        Path::new("--list"),
        &suite,
    ]);
    let listed_text = text(&listed.stdout);
    assert!(listed.status.success(), "{listed_text}{}", text(&listed.stderr));
    assert!(listed_text.contains("suite.test_alpha"));
    assert!(listed_text.contains("suite.test_beta"));
    assert!(!listed_text.contains("alpha-output"));
    assert!(!listed_text.contains("beta-output"));
    assert!(listed_text.contains("2 test(s)"));
}

#[test]
fn json_output_is_single_document_with_captured_parity_output() {
    let temp = TempDir::new("json");
    let suite = temp.write(
        "suite.witchy",
        "fn test_json(console: Console):\n    console.print(\"captured\")\n",
    );
    let plan = temp.write_plan(&console_plan());
    let output = run(&[
        Path::new("test"),
        Path::new("--fixtures"),
        &plan,
        Path::new("--format"),
        Path::new("json"),
        &suite,
    ]);
    assert!(
        output.status.success(),
        "{}{}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("one JSON result document");
    assert_eq!(document["schema"], 2);
    assert_eq!(document["summary"]["status"], "passed");
    assert_eq!(document["summary"]["passed"], 1);
    assert_eq!(document["tests"][0]["name"], "suite.test_json");
    assert_eq!(document["tests"][0]["status"], "passed");
    assert_eq!(document["tests"][0]["output"][0], "captured");
    assert_eq!(document["tests"][0]["transcript"]["version"], 1);
    assert_eq!(document["tests"][0]["transcript"]["stdout"][0], "captured");
}

#[test]
fn failed_fixture_json_retains_partial_output_and_transcript() {
    let temp = TempDir::new("failed-json");
    let suite = temp.write(
        "suite.witchy",
        "import testing\n\n\
         fn test_failure(console: Console):\n    \
         console.print(\"before-failure\")\n    \
         testing.fail_with(\"expected failure\")\n",
    );
    let plan = temp.write_plan(&console_plan());
    let output = run(&[
        Path::new("test"),
        Path::new("--fixtures"),
        &plan,
        Path::new("--format"),
        Path::new("json"),
        &suite,
    ]);
    assert!(
        !output.status.success(),
        "failing fixture unexpectedly passed: {}",
        text(&output.stdout)
    );
    assert_eq!(output.status.code(), Some(1));
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("failed JSON result document");
    assert_eq!(document["schema"], 2);
    assert_eq!(document["summary"]["status"], "failed");
    assert_eq!(document["tests"][0]["status"], "failed");
    assert_eq!(document["tests"][0]["output"][0], "before-failure");
    assert_eq!(
        document["tests"][0]["transcript"]["stdout"][0],
        "before-failure"
    );
    assert!(
        document["tests"][0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("expected failure")),
        "{}",
        document["tests"][0]["error"]
    );
}

#[test]
fn seed_override_is_reproducible_and_duplicate_json_keys_fail_closed() {
    let temp = TempDir::new("seed");
    let suite = temp.write(
        "suite.witchy",
        "import rand\n\n\
         fn test_seeded(console: Console, entropy: Rand):\n    \
         console.print(\"${rand.u64(entropy)}\")\n",
    );
    let plan = temp.write_plan(&FixturePlan {
        console: Some(ConsoleFixture::default()),
        rand: Some(RandFixture {
            seed: None,
            script: Vec::new(),
        }),
        ..FixturePlan::default()
    });
    let seeded = |seed: &str| {
        run(&[
            Path::new("test"),
            Path::new("--fixtures"),
            &plan,
            Path::new("--seed"),
            Path::new(seed),
            Path::new("--format"),
            Path::new("json"),
            &suite,
        ])
    };
    let first = seeded("42");
    let second = seeded("42");
    let different = seeded("43");
    assert!(first.status.success(), "{}{}", text(&first.stdout), text(&first.stderr));
    assert!(second.status.success(), "{}{}", text(&second.stdout), text(&second.stderr));
    assert!(
        different.status.success(),
        "{}{}",
        text(&different.stdout),
        text(&different.stderr)
    );
    assert_eq!(first.stdout, second.stdout, "same seed must reproduce exactly");
    let first_json: serde_json::Value =
        serde_json::from_slice(&first.stdout).expect("first seeded JSON");
    let different_json: serde_json::Value =
        serde_json::from_slice(&different.stdout).expect("different seeded JSON");
    assert_ne!(
        first_json["tests"][0]["output"],
        different_json["tests"][0]["output"],
        "different seeds should change the deterministic Rand stream"
    );

    let malformed = temp.write("duplicate.json", "{\"version\":1,\"version\":1}");
    let rejected = run(&[
        Path::new("test"),
        Path::new("--fixtures"),
        &malformed,
        &suite,
    ]);
    let diagnostic = format!("{}{}", text(&rejected.stdout), text(&rejected.stderr));
    assert!(!rejected.status.success(), "duplicate keys unexpectedly accepted");
    assert_eq!(rejected.status.code(), Some(2));
    assert!(diagnostic.contains("duplicate"), "{diagnostic}");
}

#[test]
fn flagship_fixture_example_runs_with_backend_parity_and_checked_output() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let plan = root.join("projects/fixture-showcase/release.fixture.json");
    let example = root.join("projects/fixture-showcase");
    let output = run(&[
        Path::new("test"),
        Path::new("--fixtures"),
        &plan,
        Path::new("--backend"),
        Path::new("both"),
        Path::new("--filter"),
        Path::new("fixture_world"),
        Path::new("--show-output"),
        &example,
    ]);
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        text(&output.stderr)
    );
    assert!(
        stdout.contains("test fixture_showcase_fixture_test.test_fixture_world ... ok")
    );
    assert!(stdout.contains("release api at 1700000000000ms in staging"));
    assert!(stdout.contains("1 passed; 0 failed"));
}

#[test]
fn vm_with_dir_preserves_fixture_state_across_both_backends() {
    let temp = TempDir::new("vm-with-dir");
    let suite = temp.write(
        "suite.witchy",
        "import bytes\nimport vm\n\n\
         fn worker(dir: Dir, name: Bytes) -> Bytes:\n    \
         let value = dir.read(bytes.to_string(name))\n    \
         dir.write(\"output.txt\", value + \"!\")\n    \
         bytes.from_string(dir.read(\"output.txt\"))\n\n\
         fn test_vm(console: Console, root: Dir):\n    \
         let sandbox = root.subtree(\"sandbox\")\n    \
         let result = vm.with_dir(sandbox, worker, bytes.from_string(\"input.txt\"))\n    \
         console.print(bytes.to_string(result))\n    \
         console.print(sandbox.read(\"output.txt\"))\n",
    );
    let plan = temp.write_plan(&FixturePlan {
        console: Some(ConsoleFixture::default()),
        filesystem: Some(FilesystemFixture {
            entries: BTreeMap::from([
                ("sandbox".to_owned(), FilesystemEntry::Directory),
                (
                    "sandbox/input.txt".to_owned(),
                    FilesystemEntry::File {
                        hex: "736861726564".to_owned(),
                    },
                ),
            ]),
            rights: vec!["Read".to_owned(), "Write".to_owned()],
            entry_policy: None,
            script: Vec::new(),
        }),
        ..FixturePlan::default()
    });
    let output = run(&[
        Path::new("test"),
        Path::new("--fixtures"),
        &plan,
        Path::new("--backend"),
        Path::new("both"),
        Path::new("--show-output"),
        &suite,
    ]);
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        text(&output.stderr)
    );
    assert!(stdout.contains("test suite.test_vm ... ok"));
    assert_eq!(stdout.matches("  shared!").count(), 2, "{stdout}");
}

#[test]
fn plain_tests_cannot_receive_real_authority_implicitly() {
    let temp = TempDir::new("plain-authority");
    let ambient = temp.write("ambient/secret.txt", "must remain ambient");
    let suite = temp.write(
        "suite.witchy",
        "fn test_read_ambient(root: Dir):\n    let _ = root.read(\"secret.txt\")\n",
    );

    let output = run(&[Path::new("test"), &suite]);
    let message = diagnostic(&output);
    assert!(!output.status.success(), "plain authority unexpectedly granted: {message}");
    assert_eq!(output.status.code(), Some(1), "{message}");
    assert!(
        message.contains("declares capability parameter")
            && message.contains("witchy test --integration"),
        "{message}"
    );
    assert_eq!(
        std::fs::read_to_string(ambient).expect("ambient sentinel remains readable"),
        "must remain ambient"
    );
}

#[test]
fn test_only_sealed_construction_does_not_escape_to_production_commands() {
    let temp = TempDir::new("production-boundary");
    temp.write(
        "sealed_lib.witchy",
        "sealed type Version:\n    Version(Int, Int, Int)\n\n\
         pub fn major(version: Version) -> Int:\n    \
         match version:\n        Version(value, _, _) -> value\n",
    );
    let suite = temp.write(
        "suite.witchy",
        "import sealed_lib\nimport testing\n\n\
         fn test_constructs_edge_case():\n    \
         let version = sealed_lib.Version(99, 0, 0)\n    \
         testing.assert_int_eq(sealed_lib.major(version), 99)\n",
    );

    let test = run(&[Path::new("test"), &suite]);
    assert!(
        test.status.success(),
        "authenticated test control failed: {}",
        diagnostic(&test)
    );

    assert_sealed_rejected(&run(&[&suite]), "run");
    assert_sealed_rejected(&run(&[Path::new("check"), &suite]), "check");

    let artifact = temp.join("escaped.wasm");
    assert_sealed_rejected(
        &run(&[
            Path::new("compile"),
            &suite,
            Path::new("--out"),
            &artifact,
        ]),
        "compile",
    );
    assert!(!artifact.exists(), "rejected compile left a production artifact");

    let build_out = temp.join("build-out");
    assert_sealed_rejected(
        &run(&[
            Path::new("build-step"),
            &suite,
            Path::new("--out"),
            &build_out,
        ]),
        "build-step",
    );
    assert!(
        !build_out.exists(),
        "rejected build step created an output directory"
    );

    assert_sealed_rejected(
        &run(&[Path::new("pm"), Path::new("build"), &suite]),
        "package-manager build",
    );
}

#[test]
fn test_linking_does_not_authorize_sealed_construction_during_comptime() {
    let temp = TempDir::new("comptime-boundary");
    temp.write(
        "sealed_lib.witchy",
        "sealed type Version:\n    Version(Int, Int, Int)\n",
    );
    let suite = temp.write(
        "suite.witchy",
        "import sealed_lib\n\n\
         comptime:\n    \
         emit(\"fn leaked() -> Int:\")\n    \
         emit(\"    let version = sealed_lib.Version(99, 0, 0)\")\n    \
         emit(\"    99\")\n\n\
         fn test_control():\n    let value = 1\n",
    );

    assert_sealed_rejected(
        &run(&[Path::new("test"), &suite]),
        "test-mode comptime",
    );
    assert_sealed_rejected(
        &run(&[Path::new("check"), &suite]),
        "production comptime",
    );
}
