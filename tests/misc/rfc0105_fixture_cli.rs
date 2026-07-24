//! RFC-0105 fixture CLI: parity-by-default execution and deterministic CI UX.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use witchy_testkit::{
    canonical_plan_json, ConsoleFixture, FixturePlan, RandFixture,
};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "witchy-rfc0105-{tag}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create fixture CLI temp directory");
        Self(path)
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.0.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture CLI parent");
        }
        std::fs::write(&path, contents).expect("write fixture CLI input");
        path
    }

    fn write_plan(&self, plan: &FixturePlan) -> PathBuf {
        self.write(
            "fixtures.json",
            &canonical_plan_json(plan).expect("canonical fixture plan"),
        )
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
    let plan = root.join("examples/fixture_showcase/release.fixture.json");
    let example = root.join("examples/fixture_showcase");
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
    assert!(stdout.contains("test fixture_showcase_test.test_fixture_world ... ok"));
    assert!(stdout.contains("release api at 1700000000000ms in staging"));
    assert!(stdout.contains("1 passed; 0 failed"));
}
