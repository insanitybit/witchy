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

fn seed_from_locked_gate(repo: &Path, destination: &str) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/worktree-warm.sh");
    Command::new("bash")
        .arg(script)
        .args(["--target-dir", destination])
        .current_dir(repo)
        .env("WITCHY_WORKTREE_WARM_COPY_MODE", "copy")
        .env("WITCHY_WORKTREE_WARM_GATE_LOCKED", "1")
        .output()
        .expect("run locked worktree-warm.sh")
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
        "witchy-testcache/v1/current/std.wstd",
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
    write(root, "target/witchy-testcache/v1/current/std.wstd", "cached std\n");

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

#[test]
fn worktree_warm_uses_the_active_gate_generation_when_main_target_is_absent() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/state-paths.sh", include_str!("../../scripts/state-paths.sh"));
    write(root, "state/merge-queue/gate-target", "target-prewarm\n");
    write(
        root,
        ".claude/worktrees/merge-gate/target-prewarm/debug/deps/keep.rlib",
        "warm dependency\n",
    );
    write(
        root,
        ".claude/worktrees/merge-gate/target-prewarm/witchy-testcache/v1/stale/std.wstd",
        "stale cache\n",
    );

    let output = seed_from_locked_gate(root, "seed-gate");
    assert!(
        output.status.success(),
        "gate fallback failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("seed-gate/debug/deps/keep.rlib").is_file());
    assert!(
        !root.join("seed-gate/witchy-testcache").exists(),
        "gate-worktree executable cache cannot match a relinked checkout",
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("merge-gate/target-prewarm (copy, incremental/")
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("stale witchy-testcache skipped"));
}
