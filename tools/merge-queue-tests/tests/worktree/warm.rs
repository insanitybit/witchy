use std::path::Path;
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
        "witchy-testcache",
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
    assert!(stdout.contains("worktree-warm.sh --help"));
    assert!(!stdout.contains("set -euo pipefail"));

    for (args, expected) in [
        (vec!["--target-dir"], "usage: worktree-warm.sh --target-dir <dir>"),
        (
            vec!["--clone-target", "source"],
            "usage: worktree-warm.sh --clone-target <source> <dest>",
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
    assert!(String::from_utf8_lossy(&copied.stdout).contains("(copy, same-checkout units; incremental/"));
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
fn worktree_warm_leaves_a_fresh_default_target_for_mbx() {
    let repo = temp_repo();
    let root = repo.path();
    git(root, &["config", "user.email", "worktree-test@witchy.invalid"]);
    git(root, &["config", "user.name", "Witchy Worktree Test"]);
    write(root, "README", "fixture\n");
    git(root, &["add", "README"]);
    git(root, &["commit", "--quiet", "-m", "fixture"]);
    git(root, &["worktree", "add", "--quiet", "-b", "topic", "wt"]);

    let output = run_warm(root, &["wt"], Some("copy"));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Cargo 1.100 + mbx managed seeding")
    );
    assert!(!root.join("wt/target").exists());
}

#[test]
fn worktree_warm_uses_the_action_cache_without_a_same_checkout_source() {
    let repo = temp_repo();
    let root = repo.path();
    let output = seed(root, "target-agent", Some("copy"));
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(!root.join("target-agent").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mbx action cache"));
}

#[test]
fn cargo_shim_defaults_to_learned_mode_unless_overridden() {
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

    let first = run("first", None, None);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        "incremental=0 learned=1 priority=low\n"
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

/// Cargo 1.100 target seeding requires managed targets. The shim enables both
/// features by default, preserves explicit caller overrides, disables the
/// misleading APFS target and combined budgets, caps actions/incremental state, and
/// still runs its pinned mbx.
#[test]
fn cargo_shim_runs_the_pinned_mbx_with_managed_target_seeding() {
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
        format!("#!/bin/sh\nprintf '{who} views=%s seed=%s target=%s action=%s incremental=%s total=%s store=%s\\n' \"${{MBX_TARGET_VIEWS-unset}}\" \"${{MBX_TARGET_SEED-unset}}\" \"${{MBX_TARGET_MAX_SIZE-unset}}\" \"${{MBX_GC_MAX_SIZE-unset}}\" \"${{MBX_GC_INCREMENTAL_MAX_SIZE-unset}}\" \"${{MBX_GC_MAX_TOTAL_SIZE-unset}}\" \"${{MBX_CACHE_DIR##*/}}\"\n")
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
            .env_remove("MBX_TARGET_VIEWS")
            .env_remove("MBX_TARGET_SEED")
            .env_remove("MBX_TARGET_MAX_SIZE")
            .env_remove("MBX_GC_MAX_SIZE")
            .env_remove("MBX_GC_INCREMENTAL_MAX_SIZE")
            .env_remove("MBX_GC_MAX_TOTAL_SIZE")
            .env_remove("MBX_CACHE_DIR")
            .env_remove("WITCHY_BUILD_CACHE")
            .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
            .env_remove("WITCHY_GATE_SCOPE")
            .output()
            .expect("run cargo shim fixture")
    };

    let output = run();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "pinned views=1 seed=1 target=none action=8GiB incremental=8GiB total=none store=store-9.9.9\n"
    );

    // Before the pinned version is installed, the legacy link still runs,
    // under the same guard.
    std::fs::remove_file(root.join("state/mbx/9.9.9/mbx")).expect("uninstall pinned mbx");
    let output = run();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "link views=1 seed=1 target=none action=8GiB incremental=8GiB total=none store=store-bin\n"
    );

    let output = Command::new(root.join("scripts/bin/cargo"))
        .args(["check", "-p", "witchy"])
        .current_dir(root)
        .env("PATH", &path)
        .env("MBX_STATS_REPORT", root.join("mbx-report.json"))
        .env("MBX_TARGET_VIEWS", "0")
        .env("MBX_TARGET_SEED", "0")
        .env("MBX_TARGET_MAX_SIZE", "12GiB")
        .env("MBX_GC_MAX_SIZE", "4GiB")
        .env("MBX_GC_INCREMENTAL_MAX_SIZE", "3GiB")
        .env("MBX_GC_MAX_TOTAL_SIZE", "20GiB")
        .env_remove("MBX_CACHE_DIR")
        .env_remove("WITCHY_BUILD_CACHE")
        .env_remove("WITCHY_CARGO_SHIM_ACTIVE")
        .env_remove("WITCHY_GATE_SCOPE")
        .output()
        .expect("run cargo shim override fixture");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "link views=0 seed=0 target=12GiB action=4GiB incremental=3GiB total=20GiB store=store-bin\n"
    );
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
        Some("views=1"),
        "the background sweep must see the shim's managed-target policy",
    );

    // Launching a collector touches this stamp synchronously, before detaching
    // it. Give the stamp a distinct but still recent mtime so an unwanted touch
    // cannot hide within the clock resolution. This checks the launch decision
    // without waiting for a possibly delayed child to append its record.
    let stamp = root.join("state/mbx/store-9.9.9/.witchy-gc-launched");
    let recent = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    std::fs::OpenOptions::new().write(true).open(&stamp).expect("open gc stamp")
        .set_times(std::fs::FileTimes::new().set_modified(recent))
        .expect("set recent gc stamp mtime");
    let stamp_mtime = std::fs::metadata(&stamp).expect("gc stamp metadata")
        .modified().expect("gc stamp mtime");

    // Within the interval a second build neither sweeps nor turns gc on.
    let (output, _) = run(false, None);
    assert_eq!(String::from_utf8_lossy(&output.stdout), "build gc_auto=false\n");
    assert_eq!(
        std::fs::metadata(&stamp).expect("preserved gc stamp").modified().expect("gc stamp mtime"),
        stamp_mtime,
        "the sweep ignored its interval and touched the launch stamp",
    );
    assert_eq!(wait_for_runs(1), 1, "the sweep ignored its interval");

    // A caller that asks for mbx's own automatic sweep keeps it, and the
    // shim does not add a second collector.
    std::fs::remove_file(&stamp).expect("reset gc stamp");
    let (output, _) = run(false, Some("true"));
    assert_eq!(String::from_utf8_lossy(&output.stdout), "build gc_auto=true\n");
    assert!(!stamp.exists(), "an explicit MBX_GC_AUTO still scheduled a sweep");
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
