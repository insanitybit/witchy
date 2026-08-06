use std::path::Path;
use std::process::{Command, Output};

use super::support::{TempRepo, git, write};

fn temp_repo() -> TempRepo {
    TempRepo::new("worktree-warm", |path| {
        git(path, &["init", "--quiet"]);
    })
}

fn run_warm(repo: &Path, args: &[&str], mode: Option<&str>) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/worktree-warm.sh");
    let mut command = Command::new("bash");
    command.arg(script).args(args).current_dir(repo);
    if let Some(mode) = mode {
        command.env("WITCHY_WORKTREE_WARM_COPY_MODE", mode);
    }
    command.output().expect("run worktree-warm.sh")
}

fn seed(repo: &Path, destination: &str, mode: Option<&str>) -> Output {
    run_warm(repo, &["--target-dir", destination], mode)
}

fn assert_seed(repo: &Path, destination: &str) {
    let target = repo.join(destination);
    for relative in [
        "CACHEDIR.TAG",
        "debug/.fingerprint/demo/marker",
        "debug/deps/keep with space.rlib",
        "debug/deps/.hidden-dependency",
        "debug/build/build-script.rcgu.o",
        "wasm32-unknown-unknown/debug/deps/keep-wasm.rlib",
        "nextest/ci/config.toml",
    ] {
        assert!(target.join(relative).is_file(), "seed omitted {relative}");
    }
    for relative in [
        "debug/deps/drop.rcgu.o",
        "debug/incremental",
        "debug/debug",
        "wasm32-unknown-unknown/debug/deps/drop-wasm.rcgu.o",
        "wasm32-unknown-unknown/debug/incremental",
        "wasm32-unknown-unknown/debug/debug",
    ] {
        assert!(!target.join(relative).exists(), "seed retained {relative}");
    }
}

#[test]
fn worktree_warm_help_and_usage_errors_are_stable() {
    let repo = temp_repo();
    let root = repo.path();

    let help = run_warm(root, &["--help"], Some("invalid-but-help-must-not-probe-it"));
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(stdout.contains("worktree-warm.sh <path>"));
    assert!(stdout.contains("worktree-warm.sh --target-dir <dir>"));
    assert!(stdout.contains("worktree-warm.sh --help"));
    assert!(!stdout.contains("set -euo pipefail"));

    for (args, expected) in [
        (vec!["--target-dir"], "usage: worktree-warm.sh --target-dir <dir>"),
        (vec!["--unknown"], "unknown option '--unknown'"),
        (vec!["one", "two"], "usage: worktree-warm.sh [<worktree-path>"),
    ] {
        let output = run_warm(root, &args, None);
        assert_eq!(output.status.code(), Some(2), "args {args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected), "args {args:?}");
    }
}

#[test]
fn worktree_warm_uses_portable_copy_modes_and_filters_dead_artifacts() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "target/CACHEDIR.TAG", "cache\n");
    write(root, "target/debug/.fingerprint/demo/marker", "fingerprint\n");
    write(root, "target/debug/deps/keep with space.rlib", "library\n");
    write(root, "target/debug/deps/.hidden-dependency", "metadata\n");
    write(root, "target/debug/deps/drop.rcgu.o", "dead\n");
    write(root, "target/debug/incremental/stale", "dead\n");
    write(root, "target/debug/build/build-script.rcgu.o", "retained\n");
    write(root, "target/wasm32-unknown-unknown/debug/deps/keep-wasm.rlib", "library\n");
    write(root, "target/wasm32-unknown-unknown/debug/deps/drop-wasm.rcgu.o", "dead\n");
    write(root, "target/wasm32-unknown-unknown/debug/incremental/stale", "dead\n");
    write(root, "target/nextest/ci/config.toml", "profile = 'ci'\n");

    let copied = seed(root, "seed-copy", Some("copy"));
    assert!(
        copied.status.success(),
        "copy mode failed: {}",
        String::from_utf8_lossy(&copied.stderr)
    );
    assert!(String::from_utf8_lossy(&copied.stdout).contains("(copy, incremental/"));
    assert_seed(root, "seed-copy");

    let detected = seed(root, "seed-detected", None);
    assert!(
        detected.status.success(),
        "detected copy mode failed: {}",
        String::from_utf8_lossy(&detected.stderr)
    );
    assert_seed(root, "seed-detected");

    let invalid = seed(root, "seed-invalid", Some("unknown"));
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("expected apfs, reflink, or copy"));
    assert!(!root.join("seed-invalid").exists());
}
