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
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts");
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
fn cargo_shim_preserves_incremental_mode_for_a_warm_seed_unless_overridden() {
    let repo = temp_repo();
    let root = repo.path();
    write(root, "scripts/bin/cargo", include_str!("../../scripts/bin/cargo"));
    write(
        root,
        "scripts/feature-unification.sh",
        include_str!("../../scripts/feature-unification.sh"),
    );
    write(root, "fake-bin/cargo", "#!/bin/sh\nexit 0\n");
    write(
        root,
        "state/mbx/bin/mbx",
        "#!/bin/sh\nprintf 'incremental=%s learned=%s\\n' \"${MBX_INCREMENTAL-unset}\" \"${MBX_LEARNED_INCREMENTAL-unset}\"\n",
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

    let run = |target: &str, mode: Option<&str>| {
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
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_GATE_SCOPE");
        if let Some(mode) = mode {
            command.env("WITCHY_MBX_MODE", mode);
        } else {
            command.env_remove("WITCHY_MBX_MODE");
        }
        command.output().expect("run cargo shim fixture")
    };

    let seeded = run("seeded", None);
    assert!(seeded.status.success(), "{}", String::from_utf8_lossy(&seeded.stderr));
    assert_eq!(String::from_utf8_lossy(&seeded.stdout), "incremental=1 learned=unset\n");

    let cold = run("cold", None);
    assert!(cold.status.success(), "{}", String::from_utf8_lossy(&cold.stderr));
    assert_eq!(String::from_utf8_lossy(&cold.stdout), "incremental=0 learned=1\n");

    let explicit = run("seeded", Some("restore"));
    assert!(explicit.status.success(), "{}", String::from_utf8_lossy(&explicit.stderr));
    assert_eq!(String::from_utf8_lossy(&explicit.stdout), "incremental=unset learned=unset\n");
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

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/build-seed.py");
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
