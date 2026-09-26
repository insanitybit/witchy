use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use super::support::{TempRepo, git, workspace_root, write};

fn temp_repo() -> TempRepo {
    TempRepo::new("worktree-warm", |path| {
        git(path, &["init", "--quiet"]);
    })
}

fn run_warm(repo: &Path, args: &[&str], mode: Option<&str>) -> Output {
    let script = workspace_root().join("scripts/worktree-warm.sh");
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
    let script = workspace_root().join("scripts/worktree-warm.sh");
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
    assert!(stdout.contains("worktree-warm.sh --clone-target <source> <dest>"));
    assert!(stdout.contains("worktree-warm.sh --profile-target-dir <check|clippy> <dir>"));
    assert!(stdout.contains("worktree-warm.sh --help"));
    assert!(!stdout.contains("set -euo pipefail"));

    for (args, expected) in [
        (vec!["--target-dir"], "usage: worktree-warm.sh --target-dir <dir>"),
        (
            vec!["--clone-target", "source"],
            "usage: worktree-warm.sh --clone-target <source> <dest>",
        ),
        (
            vec!["--profile-target-dir", "check"],
            "usage: worktree-warm.sh --profile-target-dir <check|clippy> <dir>",
        ),
        (vec!["--unknown"], "unknown option '--unknown'"),
        (vec!["one", "two"], "usage: worktree-warm.sh [<worktree-path>"),
    ] {
        let output = run_warm(root, &args, None);
        assert_eq!(output.status.code(), Some(2), "args {args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected), "args {args:?}");
    }
}

#[test]
fn worktree_warm_clones_the_matching_active_gate_profile() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/state-paths.sh", include_str!("../../../../scripts/state-paths.sh"));
    write(root, "state/merge-queue/gate-target", "target-prewarm\n");
    write(
        root,
        ".claude/worktrees/merge-gate/target-prewarm-check/debug/.fingerprint/aws-lc-sys/marker",
        "warm native dependency\n",
    );
    write(
        root,
        ".claude/worktrees/merge-gate/target-prewarm-check/debug/deps/drop.rcgu.o",
        "dead object\n",
    );

    let output = run_warm(root, &["--profile-target-dir", "check", "target-agent-check"], Some("copy"));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(root.join("target-agent-check/debug/.fingerprint/aws-lc-sys/marker").is_file());
    assert!(!root.join("target-agent-check/debug/deps/drop.rcgu.o").exists());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("matching check profile"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn worktree_warm_clones_an_explicit_cargo_generation_without_dead_artifacts() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "active/debug/.fingerprint/aws-lc-sys/marker", "warm\n");
    write(root, "active/debug/deps/libwasmtime.rlib", "warm\n");
    write(root, "active/debug/deps/wasmtime.rcgu.o", "dead\n");
    write(root, "active/debug/incremental/witchy/stale", "dead\n");
    write(root, "active/witchy-testcache/v1/stale/std.wstd", "dead\n");

    let output = run_warm(root, &["--clone-target", "active", "inactive"], Some("copy"));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(root.join("inactive/debug/.fingerprint/aws-lc-sys/marker").is_file());
    assert!(root.join("inactive/debug/deps/libwasmtime.rlib").is_file());
    assert!(!root.join("inactive/debug/deps/wasmtime.rcgu.o").exists());
    assert!(!root.join("inactive/debug/incremental").exists());
    assert!(!root.join("inactive/witchy-testcache").exists());

    let second = run_warm(root, &["--clone-target", "active", "inactive"], Some("copy"));
    assert!(!second.status.success(), "an existing generation must not be overwritten");
    assert!(String::from_utf8_lossy(&second.stderr).contains("destination already exists"));
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
    write(root, "scripts/state-paths.sh", include_str!("../../../../scripts/state-paths.sh"));
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
    assert!(root.join("seed-gate/.witchy-mbx-incremental-seed").is_file());
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

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).expect("stat fixture").modified().expect("mtime")
}

#[test]
fn worktree_warm_prefers_the_build_seed_and_restores_matching_mtimes() {
    let repo = temp_repo();
    let root = repo.path();
    git(root, &["config", "user.email", "worktree-test@witchy.invalid"]);
    git(root, &["config", "user.name", "Witchy Worktree Test"]);
    write(root, "src/same.rs", "fn same() {}\n");
    write(root, "src/changed.rs", "fn before() {}\n");
    git(root, &["add", "src"]);
    git(root, &["commit", "--quiet", "-m", "fixture"]);
    // The "gate checkout" built its target long after these sources were
    // written, which is what an old mtime stands for.
    let status = Command::new("touch")
        .args(["-t", "200001010000", "src/same.rs", "src/changed.rs", "src"])
        .current_dir(root)
        .status()
        .expect("run touch");
    assert!(status.success());
    write(root, "target/debug/deps/seeded.rlib", "gate build\n");
    // A source outside the crate's directory is recorded by absolute path in
    // cargo's dep-info; the seeded copy must name the worktree's file.
    let gate = std::fs::canonicalize(root).expect("canonical fixture root");
    let outside = format!("{}/std/set.witchy", gate.display());
    std::fs::create_dir_all(root.join("target/debug/.fingerprint/unit-1"))
        .expect("create fingerprint dir");
    std::fs::write(root.join("target/debug/.fingerprint/unit-1/dep-lib-unit"), dep_info(&[
        (0, "src/lib.rs"),
        (1, &outside),
    ]))
    .expect("write dep-info");
    let scripts = workspace_root().join("scripts");
    let published = Command::new("python3")
        .arg(scripts.join("build-seed.py"))
        .args(["publish", ".", "target", "state/build-seed"])
        .current_dir(root)
        .output()
        .expect("run build-seed.py publish");
    assert!(published.status.success(), "{}", String::from_utf8_lossy(&published.stderr));
    let finished = Command::new("python3")
        .arg(scripts.join("build-seed.py"))
        .args(["finish", "state/build-seed"])
        .current_dir(root)
        .output()
        .expect("run build-seed.py finish");
    assert!(finished.status.success(), "{}", String::from_utf8_lossy(&finished.stderr));

    git(root, &["worktree", "add", "--quiet", "-b", "topic", "wt"]);
    let worktree = root.join("wt");
    write(&worktree, "src/changed.rs", "fn after() {}\n");
    git(&worktree, &["commit", "--quiet", "-am", "edit"]);
    // Checked out long before the seed was published: a differing file must
    // still come out newer than anything in the seed.
    let backdated = Command::new("touch")
        .args(["-t", "199901010000", "src/changed.rs"])
        .current_dir(&worktree)
        .status()
        .expect("run touch");
    assert!(backdated.success());

    let output = run_warm(root, &["wt"], Some("copy"));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("state/build-seed/seed-"), "{stdout}");
    assert!(stdout.contains("1/2 files match seed"), "{stdout}");
    assert!(worktree.join("target/debug/deps/seeded.rlib").is_file());
    assert!(worktree.join("target/.witchy-mbx-incremental-seed").is_file());
    assert_eq!(mtime(&worktree.join("src/same.rs")), mtime(&root.join("src/same.rs")));
    assert!(mtime(&worktree.join("src/changed.rs")) > mtime(&root.join("src/changed.rs")));
    assert_eq!(mtime(&worktree.join("src")), mtime(&root.join("src")));
    // Even a plain copy keeps target mtimes: a fingerprint stamped "now"
    // would look newer than a source the branch edited.
    assert_eq!(
        mtime(&worktree.join("target/debug/deps/seeded.rlib")),
        mtime(&root.join("target/debug/deps/seeded.rlib")),
    );
    let seeded_worktree = std::fs::canonicalize(&worktree).expect("canonical worktree");
    assert_eq!(
        std::fs::read(worktree.join("target/debug/.fingerprint/unit-1/dep-lib-unit"))
            .expect("read seeded dep-info"),
        dep_info(&[
            (0, "src/lib.rs"),
            (1, &format!("{}/std/set.witchy", seeded_worktree.display())),
        ]),
    );

    // A fresh worktree using an isolated Cargo target should take the same
    // published seed without waiting for a checkout-local target or gate lock.
    std::fs::remove_dir_all(worktree.join("target")).expect("remove default target");
    let isolated = run_warm(&worktree, &["--target-dir", "target-agent"], Some("copy"));
    assert!(isolated.status.success(), "{}", String::from_utf8_lossy(&isolated.stderr));
    assert!(worktree.join("target-agent/debug/deps/seeded.rlib").is_file());
    assert!(worktree.join("target-agent/.witchy-mbx-incremental-seed").is_file());
    assert_eq!(
        std::fs::read(worktree.join("target-agent/debug/.fingerprint/unit-1/dep-lib-unit"))
            .expect("read isolated target dep-info"),
        dep_info(&[
            (0, "src/lib.rs"),
            (1, &format!("{}/std/set.witchy", seeded_worktree.display())),
        ]),
    );
}

#[test]
fn build_seed_drops_only_units_with_compile_time_manifest_paths() {
    let repo = temp_repo();
    let root = repo.path();
    git(root, &["config", "user.email", "worktree-test@witchy.invalid"]);
    git(root, &["config", "user.name", "Witchy Worktree Test"]);
    write(
        root,
        "Cargo.toml",
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    );
    write(root, "src/lib.rs", "const ROOT: &str = env!(\"CARGO_MANIFEST_DIR\");\n");
    write(root, "src/portable.rs", "pub fn portable() {}\n");
    git(root, &["add", "Cargo.toml", "src"]);
    git(root, &["commit", "--quiet", "-m", "fixture"]);

    let bound = "target/debug/.fingerprint/fixture-0123456789abcdef";
    let portable = "target/debug/.fingerprint/fixture-fedcba9876543210";
    write(root, &format!("{bound}/dep-lib-fixture"), "");
    std::fs::write(
        root.join(format!("{bound}/dep-lib-fixture")),
        dep_info(&[(0, "src/lib.rs")]),
    )
    .expect("write path-bound dep-info");
    write(root, &format!("{bound}/test-lib-fixture"), "fingerprint\n");
    write(root, &format!("{portable}/dep-lib-fixture"), "");
    std::fs::write(
        root.join(format!("{portable}/dep-lib-fixture")),
        dep_info(&[(0, "src/portable.rs")]),
    )
    .expect("write portable dep-info");
    write(root, &format!("{portable}/test-lib-fixture"), "fingerprint\n");

    let scripts = workspace_root().join("scripts");
    let published = Command::new("python3")
        .arg(scripts.join("build-seed.py"))
        .args(["publish", ".", "target", "state/build-seed"])
        .current_dir(root)
        .output()
        .expect("publish build seed");
    assert!(published.status.success(), "{}", String::from_utf8_lossy(&published.stderr));
    let finished = Command::new("python3")
        .arg(scripts.join("build-seed.py"))
        .args(["finish", "state/build-seed"])
        .current_dir(root)
        .output()
        .expect("finish build seed");
    assert!(finished.status.success(), "{}", String::from_utf8_lossy(&finished.stderr));

    git(root, &["worktree", "add", "--quiet", "-b", "topic", "wt"]);
    let output = run_warm(root, &["wt"], Some("copy"));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let worktree = root.join("wt");
    assert!(!worktree.join(bound).exists(), "checkout-bound fingerprint survived");
    assert!(worktree.join(portable).is_dir(), "portable fingerprint was discarded");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 path-bound unit(s) dropped"),
        "{}",
        String::from_utf8_lossy(&output.stdout),
    );
}

#[test]
fn cargo_shim_preserves_incremental_mode_for_a_warm_seed_unless_overridden() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    write(root, "fake-bin/cargo", "#!/bin/sh\nexit 0\n");
    write(
        root,
        "state/mbx/bin/mbx",
        "#!/bin/sh\nprintf 'incremental=%s learned=%s priority=%s\\n' \"${MBX_INCREMENTAL-unset}\" \"${MBX_LEARNED_INCREMENTAL-unset}\" \"${MBX_SCHEDULER_PRIORITY-unset}\"\n",
    );
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    write(root, "seeded/.witchy-mbx-incremental-seed", "gate build seed\n");

    let run = |target: &str, mode: Option<&str>, priority: Option<&str>| {
        let path = std::env::join_paths(
            std::iter::once(root.join("fake-bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .expect("fixture PATH");
        let mut command = Command::new(root.join("scripts/bin/cargo"));
        command
            .arg("build")
            .current_dir(root)
            .env("PATH", path)
            .env("CARGO_TARGET_DIR", target)
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env_remove("MBX_INCREMENTAL")
            .env_remove("MBX_LEARNED_INCREMENTAL")
            .env_remove("MBX_SCHEDULER_PRIORITY")
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_GATE_SCOPE");
        if let Some(mode) = mode {
            command.env("WITCHY_MBX_MODE", mode);
        } else {
            command.env_remove("WITCHY_MBX_MODE");
        }
        if let Some(priority) = priority {
            command.env("MBX_SCHEDULER_PRIORITY", priority);
        }
        command.output().expect("run cargo shim fixture")
    };

    let seeded = run("seeded", None, None);
    assert!(seeded.status.success(), "{}", String::from_utf8_lossy(&seeded.stderr));
    assert_eq!(
        String::from_utf8_lossy(&seeded.stdout),
        "incremental=1 learned=unset priority=low\n"
    );

    let cold = run("cold", None, None);
    assert!(cold.status.success(), "{}", String::from_utf8_lossy(&cold.stderr));
    assert_eq!(
        String::from_utf8_lossy(&cold.stdout),
        "incremental=0 learned=1 priority=low\n"
    );

    let explicit = run("seeded", Some("restore"), Some("normal"));
    assert!(explicit.status.success(), "{}", String::from_utf8_lossy(&explicit.stderr));
    assert_eq!(
        String::from_utf8_lossy(&explicit.stdout),
        "incremental=unset learned=unset priority=normal\n"
    );
}

/// The shim refreshes every incremental-mode target from the build seed, not
/// only one worktree-warm marked: agent-check forces incremental mode on an
/// unmarked target-codex, where mbx bypasses workspace crates. A learned-mode
/// target (mbx shares workspace crates itself) and an explicit --target-dir are
/// left alone.
#[test]
fn cargo_shim_refreshes_incremental_targets_from_the_build_seed() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    write(root, "scripts/build-seed.py", "import sys\nprint('refresh ' + sys.argv[4].rsplit('/', 1)[-1], file=sys.stderr)\n");
    write(root, "fake-bin/cargo", "#!/bin/sh\nexit 0\n");
    write(root, "state/mbx/bin/mbx", "#!/bin/sh\nexit 0\n");
    write(root, "state/build-seed/current/manifest.json", "{}\n");
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    write(root, "seeded/.witchy-mbx-incremental-seed", "gate build seed\n");

    let run = |target: &str, mode: Option<&str>, extra: &[&str]| {
        let path = std::env::join_paths(
            std::iter::once(root.join("fake-bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .expect("fixture PATH");
        let mut command = Command::new(root.join("scripts/bin/cargo"));
        command
            .arg("build")
            .args(extra)
            .current_dir(root)
            .env("PATH", path)
            .env("CARGO_TARGET_DIR", target)
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env_remove("CARGO_BUILD_TARGET_DIR")
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_GATE_SCOPE")
            .env_remove("WITCHY_SEED_REFRESH");
        if let Some(mode) = mode {
            command.env("WITCHY_MBX_MODE", mode);
        } else {
            command.env_remove("WITCHY_MBX_MODE");
        }
        let output = command.output().expect("run cargo shim fixture");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    assert!(run("seeded", None, &[]).contains("refresh seeded"));
    assert!(run("target-codex", Some("incremental"), &[]).contains("refresh target-codex"));
    assert!(!run("cold", None, &[]).contains("refresh"));
    assert!(!run("target-codex", Some("incremental"), &["--target-dir", "elsewhere"]).contains("refresh"));
    // The seed carries only the gate's native graph: an isolated selection's
    // `-pure` sidecar is never refreshed from it.
    let pure = run("target-codex", Some("incremental"), &["-p", "witchy-lower"]);
    assert!(!pure.contains("refresh"), "{pure}");
}

#[test]
fn cargo_shim_niced_agent_builds_leave_the_gate_at_full_priority() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    write(root, "fake-bin/cargo", "#!/bin/sh\necho \"nice=$(ps -o nice= -p $$ | tr -d ' ')\" >&2\n");
    for executable in ["scripts/bin/cargo", "fake-bin/cargo"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let run = |envs: &[(&str, &str)]| {
        let path = std::env::join_paths(
            std::iter::once(root.join("fake-bin"))
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
        )
        .expect("fixture PATH");
        let mut command = Command::new(root.join("scripts/bin/cargo"));
        command
            .arg("metadata")
            .current_dir(root)
            .env("PATH", path)
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_CARGO_NICED")
            .env_remove("WITCHY_AGENT_NICE")
            .env_remove("WITCHY_GATE_SCOPE");
        for (key, value) in envs {
            command.env(key, value);
        }
        let output = command.output().expect("run cargo shim fixture");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let base: i32 = String::from_utf8_lossy(
        &Command::new("sh").args(["-c", "ps -o nice= -p $$"]).output().expect("ps").stdout,
    )
    .trim()
    .parse()
    .expect("own nice");
    let agent = run(&[]);
    assert!(agent.contains(&format!("nice={}", (base + 10).min(20))), "{agent}");
    let gate = run(&[("WITCHY_GATE_SCOPE", "all")]);
    assert!(gate.contains(&format!("nice={base}")), "{gate}");
    let opted_out = run(&[("WITCHY_AGENT_NICE", "off")]);
    assert!(opted_out.contains(&format!("nice={base}")), "{opted_out}");
}

#[test]
fn cargo_shim_restores_gate_dependencies_without_joining_the_agent_pool() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    write(root, "fake-bin/cargo", "#!/bin/sh\nprintf 'plain cargo\\n'\n");
    write(
        root,
        "state/mbx/bin/mbx",
        "#!/bin/sh\nprintf 'incremental=%s scheduler=%s priority=%s\\n' \"${MBX_INCREMENTAL-unset}\" \"${MBX_SCHEDULER-unset}\" \"${MBX_SCHEDULER_PRIORITY-unset}\"\n",
    );
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let path = std::env::join_paths(
        std::iter::once(root.join("fake-bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("fixture PATH");
    let output = Command::new(root.join("scripts/bin/cargo"))
        .args(["test", "-p", "witchy", "--no-run"])
        .current_dir(root)
        .env("PATH", path)
        .env("WITCHY_GATE_SCOPE", "all")
        .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
        .env_remove("WITCHY_BUILD_CACHE")
        .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
        .env_remove("WITCHY_MBX_MODE")
        .env_remove("MBX_INCREMENTAL")
        .env_remove("MBX_SCHEDULER")
        .env_remove("MBX_SCHEDULER_PRIORITY")
        .output()
        .expect("run serialized gate cargo shim fixture");

    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "incremental=1 scheduler=false priority=normal\n"
    );
}

#[test]
fn cargo_shim_keeps_isolated_targets_off_the_workspace_native_feature_graph() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    let print_features = "#!/bin/sh\nprintf 'unification=%s resolver=%s channel=%s\\n' \"${WITCHY_FEATURE_UNIFICATION-unset}\" \"${CARGO_RESOLVER_FEATURE_UNIFICATION-unset}\" \"${__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS-unset}\"\n";
    write(root, "fake-bin/cargo", print_features);
    write(root, "state/mbx/bin/mbx", print_features);
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let path = std::env::join_paths(
        std::iter::once(root.join("fake-bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("fixture PATH");
    let run = |args: &[&str]| {
        Command::new(root.join("scripts/bin/cargo"))
            .args(args)
            .current_dir(root)
            .env("PATH", &path)
            .env("WITCHY_GATE_SCOPE", "all")
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env_remove("MBX_INCREMENTAL")
            .env_remove("MBX_LEARNED_INCREMENTAL")
            .env_remove("WITCHY_MBX_MODE")
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_FEATURE_UNIFICATION")
            .env_remove("CARGO_UNSTABLE_FEATURE_UNIFICATION")
            .env_remove("CARGO_RESOLVER_FEATURE_UNIFICATION")
            .env_remove("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS")
            .output()
            .expect("run cargo shim feature fixture")
    };

    let snapshot = run(&["build", "-p", "witchy-snapshot-tools"]);
    assert!(snapshot.status.success(), "{}", String::from_utf8_lossy(&snapshot.stderr));
    assert_eq!(
        String::from_utf8_lossy(&snapshot.stdout),
        "unification=off resolver=unset channel=unset\n"
    );

    let types = run(&["nextest", "run", "-p", "witchy-types"]);
    assert!(types.status.success(), "{}", String::from_utf8_lossy(&types.stderr));
    assert_eq!(
        String::from_utf8_lossy(&types.stdout),
        "unification=off resolver=unset channel=unset\n"
    );

    for package in ["witchy-cap-model", "witchy-syntax", "witchy-canonical-source"] {
        let frontend = run(&["nextest", "run", "-p", package]);
        assert!(
            frontend.status.success(),
            "{}",
            String::from_utf8_lossy(&frontend.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&frontend.stdout),
            "unification=off resolver=unset channel=unset\n"
        );
    }

    for package in ["witchy-caps", "witchy-interp", "witchy-runtime"] {
        let native_owner = run(&["nextest", "run", "-p", package]);
        assert!(
            native_owner.status.success(),
            "{}",
            String::from_utf8_lossy(&native_owner.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&native_owner.stdout),
            "unification=unset resolver=workspace channel=nightly\n"
        );
    }

    let mixed_native = run(&[
        "nextest",
        "run",
        "-p",
        "witchy-types",
        "-p",
        "witchy-runtime",
    ]);
    assert!(
        mixed_native.status.success(),
        "{}",
        String::from_utf8_lossy(&mixed_native.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&mixed_native.stdout),
        "unification=unset resolver=workspace channel=nightly\n"
    );

    let structural = run(&[
        "nextest",
        "run",
        "-p",
        "witchy-wir",
        "--test",
        "structural_contracts",
    ]);
    assert!(structural.status.success(), "{}", String::from_utf8_lossy(&structural.stderr));
    assert_eq!(
        String::from_utf8_lossy(&structural.stdout),
        "unification=off resolver=unset channel=unset\n"
    );

    let pure_wir = run(&[
        "nextest",
        "run",
        "-p",
        "witchy-wir",
        "--test",
        "unit",
        "--no-default-features",
    ]);
    assert!(pure_wir.status.success(), "{}", String::from_utf8_lossy(&pure_wir.stderr));
    assert_eq!(
        String::from_utf8_lossy(&pure_wir.stdout),
        "unification=off resolver=unset channel=unset\n"
    );

    let native_wir = run(&["nextest", "run", "-p", "witchy-wir"]);
    assert!(native_wir.status.success(), "{}", String::from_utf8_lossy(&native_wir.stderr));
    assert_eq!(
        String::from_utf8_lossy(&native_wir.stdout),
        "unification=unset resolver=workspace channel=nightly\n"
    );

    let native = run(&["build", "-p", "witchy"]);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(
        String::from_utf8_lossy(&native.stdout),
        "unification=unset resolver=workspace channel=nightly\n"
    );
}

#[test]
fn cargo_shim_keeps_isolated_and_native_artifacts_in_separate_targets() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    let print_target = "#!/bin/sh\nprintf '%s learned=%s\n' \"${CARGO_TARGET_DIR-unset}\" \"${MBX_LEARNED_INCREMENTAL-unset}\"\n";
    write(root, "fake-bin/cargo", print_target);
    write(root, "state/mbx/bin/mbx", print_target);
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let path = std::env::join_paths(
        std::iter::once(root.join("fake-bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("fixture PATH");
    let run = |args: &[&str]| {
        Command::new(root.join("scripts/bin/cargo"))
            .args(args)
            .current_dir(root)
            .env("PATH", &path)
            .env("CARGO_TARGET_DIR", "shared-target")
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env_remove("MBX_INCREMENTAL")
            .env_remove("MBX_LEARNED_INCREMENTAL")
            .env_remove("WITCHY_MBX_MODE")
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_GATE_SCOPE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .output()
            .expect("run cargo shim target fixture")
    };

    let lower = run(&["build", "-p", "witchy-lower"]);
    assert!(lower.status.success(), "{}", String::from_utf8_lossy(&lower.stderr));
    let lower_target = String::from_utf8_lossy(&lower.stdout);
    assert!(
        Path::new(lower_target.trim()).is_absolute()
            && lower_target.ends_with("/shared-target-pure learned=unset\n"),
        "isolated graph did not use the pure sidecar: {lower_target}"
    );
    std::fs::create_dir_all(root.join("shared-target-pure/debug/.fingerprint"))
        .expect("mark pure target populated");
    let warm_lower = run(&["build", "-p", "witchy-lower"]);
    assert!(
        warm_lower.status.success(),
        "{}",
        String::from_utf8_lossy(&warm_lower.stderr)
    );
    assert!(
        String::from_utf8_lossy(&warm_lower.stdout).ends_with(
            "/shared-target-pure learned=1\n"
        ),
        "populated pure target did not return to learned mode: {}",
        String::from_utf8_lossy(&warm_lower.stdout),
    );

    let types = run(&["nextest", "run", "-p", "witchy-types"]);
    assert!(types.status.success(), "{}", String::from_utf8_lossy(&types.stderr));
    assert_eq!(
        String::from_utf8_lossy(&types.stdout),
        String::from_utf8_lossy(&warm_lower.stdout)
    );

    let native = run(&["build", "-p", "witchy"]);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(
        String::from_utf8_lossy(&native.stdout),
        "shared-target learned=1\n"
    );

    let explicit = run(&["build", "-p", "witchy-lower", "--target-dir", "chosen"]);
    assert!(explicit.status.success(), "{}", String::from_utf8_lossy(&explicit.stderr));
    assert_eq!(
        String::from_utf8_lossy(&explicit.stdout),
        "shared-target learned=1\n"
    );
}

#[test]
fn cargo_shim_neutralizes_an_ambient_rustc_wrapper_for_plain_and_mbx_routes() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    let print_wrappers = "#!/bin/sh\nprintf '%s|%s\\n' \"${RUSTC_WRAPPER-unset}\" \"${CARGO_BUILD_RUSTC_WRAPPER-unset}\"\n";
    write(root, "fake-bin/cargo", print_wrappers);
    write(root, "state/mbx/bin/mbx", print_wrappers);
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let path = std::env::join_paths(
        std::iter::once(root.join("fake-bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("fixture PATH");
    let run = |cache: &str| {
        Command::new(root.join("scripts/bin/cargo"))
            .args(["build", "-p", "witchy"])
            .current_dir(root)
            .env("PATH", &path)
            .env("WITCHY_BUILD_CACHE", cache)
            .env("RUSTC_WRAPPER", "ambient-sccache")
            .env("CARGO_BUILD_RUSTC_WRAPPER", "ambient-sccache")
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_GATE_SCOPE")
            .output()
            .expect("run cargo shim fixture")
    };

    for cache in ["off", "mbx"] {
        let output = run(cache);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout), "unset|\n");
    }
}

#[test]
fn cargo_shim_schedules_queue_tooling_through_mbx() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    write(root, "fake-bin/cargo", "#!/bin/sh\nprintf 'cargo:%s\\n' \"$*\"\n");
    write(root, "state/mbx/bin/mbx", "#!/bin/sh\nprintf 'mbx:%s\\n' \"$*\"\n");
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let path = std::env::join_paths(
        std::iter::once(root.join("fake-bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("fixture PATH");
    let run = |args: &[&str]| {
        Command::new(root.join("scripts/bin/cargo"))
            .args(args)
            .current_dir(root)
            .env("PATH", &path)
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_GATE_SCOPE")
            .output()
            .expect("run cargo shim fixture")
    };

    for args in [
        &["nextest", "run", "-p", "merge-queue-tests"][..],
        &["test", "--package=merge-queue-tests", "--no-run"][..],
        &["test", "-p", "witchy", "--no-run"][..],
        &["test", "-p", "merge-queue-tests", "-pwitchy"][..],
    ] {
        let output = run(&args);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).starts_with("mbx:"));
    }
}

/// mbx 1.18 moves an unnamed `target/` under its managed root on the first
/// build unless target views are off; every worktree path convention here
/// (build seed, pool targets, reclaim) depends on `<worktree>/target` staying
/// a real directory. The shim also runs the version its own checkout pins,
/// so an older shim never inherits a newer mbx without that guard.
#[test]
fn cargo_shim_runs_the_pinned_mbx_with_target_views_off() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    write(root, "scripts/mbx-install.sh", "#!/bin/sh\nMBX_VERSION=\"9.9.9\"\n");
    write(root, "fake-bin/cargo", "#!/bin/sh\nexit 0\n");
    let print = |who: &str| {
        format!("#!/bin/sh\nprintf '{who} views=%s store=%s\\n' \"${{MBX_TARGET_VIEWS-unset}}\" \"${{MBX_CACHE_DIR##*/}}\"\n")
    };
    write(root, "state/mbx/9.9.9/mbx", &print("pinned"));
    write(root, "state/mbx/bin/mbx", &print("link"));
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/9.9.9/mbx", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let path = std::env::join_paths(
        std::iter::once(root.join("fake-bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("fixture PATH");
    let run = || {
        Command::new(root.join("scripts/bin/cargo"))
            .args(["check", "-p", "witchy"])
            .current_dir(root)
            .env("PATH", &path)
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env("MBX_TARGET_VIEWS", "1")
            .env_remove("MBX_CACHE_DIR")
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_GATE_SCOPE")
            .output()
            .expect("run cargo shim fixture")
    };

    let output = run();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "pinned views=0 store=store-9.9.9\n");

    // Before the pinned version is installed, the legacy link still runs,
    // under the same guard.
    std::fs::remove_file(root.join("state/mbx/9.9.9/mbx")).expect("uninstall pinned mbx");
    let output = run();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "link views=0 store=store-bin\n");
}

/// mbx's automatic GC sweep runs in the foreground after Cargo exits, and on
/// 2026-09-26 it held merge-gate test steps open for 15-31 minutes after
/// nextest's summary. The shim turns it off for every build and instead
/// launches `mbx gc` detached, at most once per interval, with its stdio
/// away from the caller's (a gate log or pipe must never wait on it).
#[test]
fn cargo_shim_keeps_mbx_gc_off_the_build_critical_path() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    write(root, "scripts/mbx-install.sh", "#!/bin/sh\nMBX_VERSION=\"9.9.9\"\n");
    write(root, "fake-bin/cargo", "#!/bin/sh\nexit 0\n");
    // `mbx gc` sleeps before recording itself: a shim that waited on it, or
    // shared its stdout, would print "swept" or finish late.
    write(
        root,
        "state/mbx/9.9.9/mbx",
        "#!/bin/sh\nif [ \"$1\" = gc ]; then sleep 2; echo swept; \
         echo \"$$ views=${MBX_TARGET_VIEWS-unset}\" >>\"$MBX_CACHE_DIR/../gc-runs\"; exit 0; fi\n\
         printf 'build gc_auto=%s\\n' \"${MBX_GC_AUTO-unset}\"\n",
    );
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/9.9.9/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let path = std::env::join_paths(
        std::iter::once(root.join("fake-bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("fixture PATH");
    let run = |gate: bool, gc_auto: Option<&str>| {
        let mut command = Command::new(root.join("scripts/bin/cargo"));
        command
            .args(["nextest", "run", "--workspace"])
            .current_dir(root)
            .env("PATH", &path)
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env("WITCHY_AGENT_NICE", "off")
            .env_remove("MBX_CACHE_DIR")
            .env_remove("MBX_GC_AUTO")
            .env_remove("WITCHY_MBX_GC")
            .env_remove("WITCHY_MBX_GC_INTERVAL")
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_GATE_SCOPE");
        if gate {
            command.env("WITCHY_GATE_SCOPE", "all");
        }
        if let Some(value) = gc_auto {
            command.env("MBX_GC_AUTO", value);
        }
        let started = std::time::Instant::now();
        let output = command.output().expect("run cargo shim fixture");
        (output, started.elapsed())
    };
    let runs = root.join("state/mbx/gc-runs");
    let wait_for_runs = |count: usize| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let seen = std::fs::read_to_string(&runs).map(|s| s.lines().count()).unwrap_or(0);
            if seen >= count || std::time::Instant::now() > deadline {
                return seen;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    };

    let (output, elapsed) = run(true, None);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "build gc_auto=false\n");
    assert!(
        elapsed < std::time::Duration::from_millis(1800),
        "the build waited on the background sweep: {elapsed:?}",
    );
    assert_eq!(wait_for_runs(1), 1, "the due sweep never ran");
    assert_eq!(
        std::fs::read_to_string(&runs).expect("gc record").split_whitespace().nth(1),
        Some("views=0"),
        "the background sweep must see the shim's target-views guard",
    );

    // Within the interval a second build neither sweeps nor turns gc on.
    let (output, _) = run(false, None);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "build gc_auto=false\n");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert_eq!(wait_for_runs(1), 1, "the sweep ignored its interval");

    // A caller that asks for mbx's own automatic sweep keeps it, and the
    // shim does not add a second collector.
    std::fs::remove_file(root.join("state/mbx/store-9.9.9/.witchy-gc-launched"))
        .expect("reset gc stamp");
    let (output, _) = run(false, Some("true"));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "build gc_auto=true\n");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert_eq!(wait_for_runs(1), 1, "an explicit MBX_GC_AUTO still launched a sweep");
}

#[test]
fn cargo_shim_bounds_direct_agent_test_fanout_but_not_the_serialized_gate() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../../../scripts/feature-unification.sh"),
    );
    let print_fanout =
        "#!/bin/sh\nprintf 'nextest=%s rust=%s\\n' \"${NEXTEST_TEST_THREADS-unset}\" \"${RUST_TEST_THREADS-unset}\"\n";
    write(root, "fake-bin/cargo", print_fanout);
    write(
        root,
        "state/mbx/bin/mbx",
        print_fanout,
    );
    for executable in ["scripts/bin/cargo", "fake-bin/cargo", "state/mbx/bin/mbx"] {
        let status = Command::new("chmod")
            .args(["+x", executable])
            .current_dir(root)
            .status()
            .expect("mark fixture executable");
        assert!(status.success());
    }
    let path = std::env::join_paths(
        std::iter::once(root.join("fake-bin"))
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())),
    )
    .expect("fixture PATH");
    let run = |subcommand: &str, gate: bool, explicit: bool| {
        let mut command = Command::new(root.join("scripts/bin/cargo"));
        command
            .args([subcommand, "run"])
            .current_dir(root)
            .env("PATH", &path)
            .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("NEXTEST_TEST_THREADS")
            .env_remove("RUST_TEST_THREADS");
        if gate {
            command.env("WITCHY_GATE_SCOPE", "rust");
        } else {
            command.env_remove("WITCHY_GATE_SCOPE");
        }
        if explicit {
            command.env("NEXTEST_TEST_THREADS", "7").env("RUST_TEST_THREADS", "5");
        }
        command.output().expect("run cargo shim fanout fixture")
    };

    for (subcommand, expected) in [
        ("nextest", "nextest=2 rust=unset\n"),
        ("test", "nextest=unset rust=2\n"),
    ] {
        let output = run(subcommand, false, false);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    }

    let explicit = run("nextest", false, true);
    assert!(explicit.status.success(), "{}", String::from_utf8_lossy(&explicit.stderr));
    assert_eq!(String::from_utf8_lossy(&explicit.stdout), "nextest=7 rust=5\n");

    let gate = run("nextest", true, false);
    assert!(gate.status.success(), "{}", String::from_utf8_lossy(&gate.stderr));
    assert_eq!(
        String::from_utf8_lossy(&gate.stdout),
        "nextest=unset rust=unset\n",
    );

    let plain = Command::new(root.join("scripts/bin/cargo"))
        .args(["nextest", "run"])
        .current_dir(root)
        .env("PATH", &path)
        .env("WITCHY_BUILD_CACHE", "off")
        .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
        .env_remove("WITCHY_GATE_SCOPE")
        .env_remove("NEXTEST_TEST_THREADS")
        .env_remove("RUST_TEST_THREADS")
        .output()
        .expect("run plain cargo shim fanout fixture");
    assert!(plain.status.success(), "{}", String::from_utf8_lossy(&plain.stderr));
    assert_eq!(String::from_utf8_lossy(&plain.stdout), "nextest=2 rust=unset\n");
}

#[test]
fn build_seed_finish_never_moves_current_back_to_an_older_snapshot() {
    let repo = temp_repo();
    let root = repo.path();
    let seed_root = root.join("state/build-seed");
    write(
        root,
        "state/build-seed/seed-newer/manifest.json",
        r#"{"snapshot_ns":2,"fill_from":null,"paths":[]}"#,
    );
    write(
        root,
        "state/build-seed/.seed-older/manifest.json",
        r#"{"snapshot_ns":1,"fill_from":null,"paths":[]}"#,
    );
    let linked = Command::new("ln")
        .args(["-s", "seed-newer", "current"])
        .current_dir(&seed_root)
        .status()
        .expect("create current seed link");
    assert!(linked.success());

    let script = workspace_root().join("scripts/build-seed.py");
    let finished = Command::new("python3")
        .arg(script)
        .args(["finish", "state/build-seed"])
        .current_dir(root)
        .output()
        .expect("finish staged seed");
    assert!(finished.status.success(), "{}", String::from_utf8_lossy(&finished.stderr));
    assert_eq!(std::fs::read_link(seed_root.join("current")).unwrap(), Path::new("seed-newer"));
    assert!(seed_root.join("seed-older").is_dir(), "older staging seed should still finish");
}

#[test]
fn build_seed_preserves_mtimes_only_for_content_identical_candidate_files() {
    let repo = temp_repo();
    let root = repo.path();
    git(root, &["config", "user.email", "worktree-test@witchy.invalid"]);
    git(root, &["config", "user.name", "Witchy Worktree Test"]);
    write(root, "src/unchanged.rs", "pub const VALUE: u8 = 1;\n");
    write(root, "src/changed.rs", "pub const VALUE: u8 = 1;\n");
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", "first candidate"]);

    let unchanged_before = std::fs::metadata(root.join("src/unchanged.rs"))
        .expect("unchanged metadata")
        .modified()
        .expect("unchanged mtime");
    let changed_before = std::fs::metadata(root.join("src/changed.rs"))
        .expect("changed metadata")
        .modified()
        .expect("changed mtime");
    let directory_before = std::fs::metadata(root.join("src"))
        .expect("directory metadata")
        .modified()
        .expect("directory mtime");
    let snapshot = root.join("candidate-mtimes.json");
    let script = workspace_root().join("scripts/build-seed.py");
    let snap = Command::new("python3")
        .arg(&script)
        .arg("snapshot-mtimes")
        .arg(root)
        .arg(&snapshot)
        .output()
        .expect("snapshot candidate mtimes");
    assert!(snap.status.success(), "{}", String::from_utf8_lossy(&snap.stderr));

    std::thread::sleep(std::time::Duration::from_millis(20));
    // Model checkout + replay: Git rewrites even content-identical files, one
    // source really changes, and transient directory entries perturb the dir.
    write(root, "src/unchanged.rs", "pub const VALUE: u8 = 1;\n");
    write(root, "src/changed.rs", "pub const VALUE: u8 = 2;\n");
    write(root, "src/transient", "checkout scratch\n");
    std::fs::remove_file(root.join("src/transient")).expect("remove transient file");
    git(root, &["commit", "--quiet", "-am", "amended candidate"]);

    let restore = Command::new("python3")
        .arg(&script)
        .arg("restore-mtimes")
        .arg(root)
        .arg(&snapshot)
        .output()
        .expect("restore candidate mtimes");
    assert!(restore.status.success(), "{}", String::from_utf8_lossy(&restore.stderr));
    let stdout = String::from_utf8_lossy(&restore.stdout);
    assert!(stdout.contains("preserved mtimes for 1 unchanged tracked file(s)"), "{stdout}");
    assert_eq!(
        std::fs::metadata(root.join("src/unchanged.rs")).unwrap().modified().unwrap(),
        unchanged_before,
    );
    assert_ne!(
        std::fs::metadata(root.join("src/changed.rs")).unwrap().modified().unwrap(),
        changed_before,
    );
    assert_eq!(
        std::fs::metadata(root.join("src")).unwrap().modified().unwrap(),
        directory_before,
    );

    // A dirty worktree is not safe to reason about from index blob ids.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(root, "src/unchanged.rs", "pub const VALUE: u8 = 3;\n");
    let dirty_mtime = std::fs::metadata(root.join("src/unchanged.rs"))
        .unwrap()
        .modified()
        .unwrap();
    let refused = Command::new("python3")
        .arg(&script)
        .arg("restore-mtimes")
        .arg(root)
        .arg(&snapshot)
        .output()
        .expect("refuse dirty candidate restore");
    assert!(!refused.status.success());
    assert_eq!(
        std::fs::metadata(root.join("src/unchanged.rs")).unwrap().modified().unwrap(),
        dirty_mtime,
    );
}

/// Cargo's encoded fingerprint dep-info, as scripts/build-seed.py decodes it.
fn dep_info(files: &[(u8, &str)]) -> Vec<u8> {
    let mut out = vec![1, 0, 0, 0, 0xff, 1];
    out.extend((files.len() as u32).to_le_bytes());
    for (kind, path) in files {
        out.push(*kind);
        out.extend((path.len() as u32).to_le_bytes());
        out.extend(path.as_bytes());
        out.push(0);
    }
    out.extend(0u32.to_le_bytes());
    out
}

/// Real Cargo in `dir`, building into `dir/<target>`; returns stderr.
fn seed_cargo(dir: &Path, target: &str, args: &[&str]) -> String {
    let output = Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", dir.join(target))
        .env("CARGO_BUILD_RUSTC_WRAPPER", "")
        .env("CARGO_TERM_COLOR", "never")
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_INCREMENTAL")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .output()
        .expect("run cargo");
    assert!(output.status.success(), "cargo {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn run_bin(dir: &Path, target: &str) -> String {
    let run = Command::new(dir.join(target).join("debug/bin")).output().expect("run bin");
    String::from_utf8_lossy(&run.stdout).into_owned()
}

/// A two-crate workspace (`bin` depends on `dep`) whose master then changes
/// `dep`: the gate builds it and publishes the seed. The returned worktree
/// built `target` at the old master, committed its own edit to `bin`, and
/// `before_rebase` runs just before it rebases onto the new master.
fn seed_refresh_fixture(before_rebase: impl FnOnce(&Path)) -> (TempRepo, PathBuf, PathBuf) {
    let repo = TempRepo::new("seed-refresh", |_| {});
    let gate = repo.path().join("gate");
    std::fs::create_dir(&gate).expect("create gate checkout");
    git(&gate, &["init", "--quiet"]);
    git(&gate, &["config", "user.email", "worktree-test@witchy.invalid"]);
    git(&gate, &["config", "user.name", "Witchy Worktree Test"]);
    write(&gate, ".gitignore", "/target*\n/state\n/Cargo.lock\n");
    write(&gate, "Cargo.toml", "[workspace]\nmembers = [\"dep\", \"bin\"]\nresolver = \"2\"\n");
    write(&gate, "dep/Cargo.toml", "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    write(&gate, "dep/src/lib.rs", "pub fn value() -> u32 { 1 }\n");
    write(
        &gate,
        "bin/Cargo.toml",
        "[package]\nname = \"bin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n",
    );
    write(&gate, "bin/src/main.rs", "fn main() { println!(\"{} base\", dep::value()); }\n");
    git(&gate, &["add", "."]);
    git(&gate, &["commit", "--quiet", "-m", "base"]);
    let base = git(&gate, &["rev-parse", "HEAD"]);

    let wt = repo.path().join("wt");
    git(&gate, &["worktree", "add", "--quiet", "-b", "topic", wt.to_str().expect("utf8"), &base]);
    seed_cargo(&wt, "target", &["build", "--offline"]);
    write(&wt, "bin/src/main.rs", "fn main() { println!(\"{} topic\", dep::value()); }\n");
    git(&wt, &["commit", "--quiet", "-am", "topic edit"]);

    write(&gate, "dep/src/lib.rs", "pub fn value() -> u32 { 2 }\n");
    git(&gate, &["commit", "--quiet", "-am", "master moves dep"]);
    seed_cargo(&gate, "target", &["build", "--offline"]);
    let scripts = workspace_root().join("scripts");
    for args in [&["publish", ".", "target", "state/build-seed"][..], &["finish", "state/build-seed"]] {
        let output = Command::new("python3")
            .arg(scripts.join("build-seed.py"))
            .args(args)
            .current_dir(&gate)
            .output()
            .expect("run build-seed.py");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    before_rebase(&wt);
    git(&wt, &["rebase", "--quiet", &git(&gate, &["rev-parse", "HEAD"])]);
    let seed = std::fs::canonicalize(gate.join("state/build-seed/current")).expect("seed");
    (repo, wt, seed)
}

fn seed_refresh(seed: &Path, wt: &Path) -> String {
    let output = Command::new("python3")
        .arg(workspace_root().join("scripts/build-seed.py"))
        .arg("refresh")
        .arg(seed)
        .arg(wt)
        .output()
        .expect("run build-seed.py refresh");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    stderr
}

/// `refresh` serves the unedited `dep` from the seed (Cargo reports it
/// Fresh) while `bin` still compiles, and a later edit to `dep` rebuilds it.
/// A second target directory built on the old base, after the gate built
/// the new one, must not be fooled into reusing its old `dep`: refresh never
/// moves a source's mtime.
#[test]
fn build_seed_refresh_serves_unedited_crates_from_the_current_seed() {
    let (_repo, wt, seed) = seed_refresh_fixture(|wt| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        seed_cargo(wt, "target-other", &["build", "--offline"]);
        assert_eq!(run_bin(wt, "target-other"), "1 topic\n");
    });
    let refreshed = seed_refresh(&seed, &wt);
    assert!(refreshed.contains("refreshed 1 unit(s)"), "{refreshed}");
    // Unchanged seed and HEAD: the stamp makes a second call a no-op.
    assert!(seed_refresh(&seed, &wt).is_empty());

    let built = seed_cargo(&wt, "target", &["build", "--offline", "-v"]);
    assert!(built.contains("Fresh dep"), "{built}");
    assert!(built.contains("Compiling bin"), "{built}");
    assert_eq!(run_bin(&wt, "target"), "2 topic\n");

    let other = seed_cargo(&wt, "target-other", &["build", "--offline", "-v"]);
    assert!(other.contains("Compiling dep"), "{other}");
    assert_eq!(run_bin(&wt, "target-other"), "2 topic\n");

    // An edit after the refresh is newer than the refreshed unit.
    write(&wt, "dep/src/lib.rs", "pub fn value() -> u32 { 3 }\n");
    let rebuilt = seed_cargo(&wt, "target", &["build", "--offline", "-v"]);
    assert!(rebuilt.contains("Compiling dep"), "{rebuilt}");
    assert_eq!(run_bin(&wt, "target"), "3 topic\n");
}

/// The refresh stamp is keyed on the seed and the branch's merge-base with
/// master, so the branch's own commits (every edit loop commits before
/// agent-check) skip the re-hash, while the build still sees the edit.
#[test]
fn build_seed_refresh_skips_the_branchs_own_commits() {
    let (_repo, wt, seed) = seed_refresh_fixture(|_| {});
    let master = git(&wt, &["rev-parse", "HEAD~1"]);
    git(&wt, &["update-ref", "refs/heads/master", &master]);
    let refreshed = seed_refresh(&seed, &wt);
    assert!(refreshed.contains("refreshed 1 unit(s)"), "{refreshed}");

    write(&wt, "bin/src/main.rs", "fn main() { println!(\"{} second\", dep::value()); }\n");
    git(&wt, &["commit", "--quiet", "-am", "second topic edit"]);
    assert!(seed_refresh(&seed, &wt).is_empty(), "a branch-only commit must not re-hash");
    let built = seed_cargo(&wt, "target", &["build", "--offline", "-v"]);
    assert!(built.contains("Compiling bin"), "{built}");
    assert_eq!(run_bin(&wt, "target"), "2 second\n");
}

/// An uncommitted edit made before the refresh (the index still matches the
/// seed) keeps its crate out of the refresh, and it compiles.
#[test]
fn build_seed_refresh_leaves_an_uncommitted_edit_to_compile() {
    let (_repo, wt, seed) = seed_refresh_fixture(|_| {});
    write(&wt, "dep/src/lib.rs", "pub fn value() -> u32 { 7 }\n");
    let refreshed = seed_refresh(&seed, &wt);
    assert!(refreshed.contains("refreshed 0 unit(s)"), "{refreshed}");
    let built = seed_cargo(&wt, "target", &["build", "--offline", "-v"]);
    assert!(built.contains("Compiling dep"), "{built}");
    assert_eq!(run_bin(&wt, "target"), "7 topic\n");
}

/// Writes a path-dependency source with a fixed old mtime, so a Cargo target
/// that compiled an EARLIER version of it still judges that unit fresh.
fn write_aged(root: &Path, relative: &str, contents: &str) {
    write(root, relative, contents);
    let aged = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    std::fs::File::options()
        .write(true)
        .open(root.join(relative))
        .and_then(|file| file.set_modified(aged))
        .expect("age third-party source");
}

/// The same third-party unit (same fingerprint hash) can be a different
/// artifact in the seed than in the agent's target: the hash names inputs,
/// not bytes, and mbx restores or rebuilds diverge (the same wasmtime unit
/// held four different rmetas across agent targets on 2026-09-26). Refresh
/// once swapped in the seed's `witchy-wir`, compiled against the seed's
/// wasmtime, beside the target's own wasmtime, and every dependent failed
/// with E0460/E0463 "can't find crate for `witchy_wir`". Here `third` is
/// such a dependency: the worktree's target compiled one version, the gate
/// another, and Cargo finds the old one fresh. Refresh must swap in the
/// seed's `third` with `dep` (or leave `dep` out), never `dep` alone.
#[test]
fn build_seed_refresh_swaps_a_divergent_third_party_dependency_with_its_dependent() {
    let repo = TempRepo::new("seed-refresh-third-party", |_| {});
    write_aged(repo.path(), "ext/third/src/lib.rs", "pub fn t() -> u32 { 10 }\n");
    write(repo.path(), "ext/third/Cargo.toml", "[package]\nname = \"third\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    let gate = repo.path().join("gate");
    std::fs::create_dir(&gate).expect("create gate checkout");
    git(&gate, &["init", "--quiet"]);
    git(&gate, &["config", "user.email", "worktree-test@witchy.invalid"]);
    git(&gate, &["config", "user.name", "Witchy Worktree Test"]);
    write(&gate, ".gitignore", "/target*\n/state\n/Cargo.lock\n");
    write(&gate, "Cargo.toml", "[workspace]\nmembers = [\"dep\", \"bin\"]\nresolver = \"2\"\n");
    write(
        &gate,
        "dep/Cargo.toml",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nthird = { path = \"../../ext/third\" }\n",
    );
    write(&gate, "dep/src/lib.rs", "pub fn value() -> u32 { third::t() + 1 }\n");
    write(
        &gate,
        "bin/Cargo.toml",
        "[package]\nname = \"bin\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ndep = { path = \"../dep\" }\n",
    );
    write(&gate, "bin/src/main.rs", "fn main() { println!(\"{} base\", dep::value()); }\n");
    git(&gate, &["add", "."]);
    git(&gate, &["commit", "--quiet", "-m", "base"]);
    let base = git(&gate, &["rev-parse", "HEAD"]);

    let wt = repo.path().join("wt");
    git(&gate, &["worktree", "add", "--quiet", "-b", "topic", wt.to_str().expect("utf8"), &base]);
    seed_cargo(&wt, "target", &["build", "--offline"]);
    assert_eq!(run_bin(&wt, "target"), "11 base\n");
    write(&wt, "bin/src/main.rs", "fn main() { println!(\"{} topic\", dep::value()); }\n");
    git(&wt, &["commit", "--quiet", "-am", "topic edit"]);

    // The gate compiles another `third` under the same unit hash.
    write_aged(repo.path(), "ext/third/src/lib.rs", "pub fn t() -> u32 { 20 }\n");
    write(&gate, "dep/src/lib.rs", "pub fn value() -> u32 { third::t() + 2 }\n");
    git(&gate, &["commit", "--quiet", "-am", "master moves dep"]);
    seed_cargo(&gate, "target", &["build", "--offline"]);
    let scripts = workspace_root().join("scripts");
    for args in [&["publish", ".", "target", "state/build-seed"][..], &["finish", "state/build-seed"]] {
        let output = Command::new("python3")
            .arg(scripts.join("build-seed.py"))
            .args(args)
            .current_dir(&gate)
            .output()
            .expect("run build-seed.py");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    }
    git(&wt, &["rebase", "--quiet", &git(&gate, &["rev-parse", "HEAD"])]);
    let seed = std::fs::canonicalize(gate.join("state/build-seed/current")).expect("seed");

    let refreshed = seed_refresh(&seed, &wt);
    // Before the fix this build failed: E0460 "found possibly newer version
    // of crate `third` which `dep` depends on".
    let built = seed_cargo(&wt, "target", &["build", "--offline", "-v"]);
    assert_eq!(run_bin(&wt, "target"), "22 topic\n");
    assert!(refreshed.contains("refreshed 2 unit(s)"), "{refreshed}");
    assert!(built.contains("Fresh dep"), "{built}");
    assert!(built.contains("Compiling bin"), "{built}");
}
