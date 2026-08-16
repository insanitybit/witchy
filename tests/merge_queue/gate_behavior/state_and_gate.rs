use super::*;

fn resolve_merge_queue_state(root: &Path, envs: &[(&str, &Path)]) -> PathBuf {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut command = Command::new("bash");
    command
        .args([
            "-c",
            ". \"$1\"; witchy_merge_queue_state_dir \"$2\"",
            "state-path-test",
        ])
        .arg(repo.join("scripts/state-paths.sh"))
        .arg(root);
    for (name, value) in envs {
        command.env(name, value);
    }
    let output = command.output().expect("resolve merge queue state path");
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).expect("state path is utf8").trim())
}

#[test]
fn state_path_prefers_fresh_canonical_layout_and_preserves_legacy_until_cutover() {
    let temp = TempDir::new();
    let root = temp.path().join("root");
    fs::create_dir(&root).expect("create fake repository root");

    assert_eq!(
        resolve_merge_queue_state(&root, &[]),
        root.join("state/merge-queue"),
    );
    fs::create_dir_all(root.join("scratch/merge-queue")).expect("create legacy state");
    assert_eq!(
        resolve_merge_queue_state(&root, &[]),
        root.join("scratch/merge-queue"),
    );
    fs::remove_dir_all(root.join("scratch/merge-queue")).expect("remove legacy fixture");
    fs::create_dir_all(root.join("state/merge-queue")).expect("create canonical state");
    std::os::unix::fs::symlink(
        "../state/merge-queue",
        root.join("scratch/merge-queue"),
    )
    .expect("create legacy compatibility link");
    assert_eq!(
        resolve_merge_queue_state(&root, &[]),
        root.join("state/merge-queue"),
    );

    fs::remove_file(root.join("scratch/merge-queue")).expect("remove compatibility link");
    std::os::unix::fs::symlink("../wrong-queue", root.join("scratch/merge-queue"))
        .expect("create invalid legacy link");
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let invalid = Command::new("bash")
        .args([
            "-c",
            ". \"$1\"; witchy_merge_queue_state_dir \"$2\"",
            "state-path-test",
        ])
        .arg(repo.join("scripts/state-paths.sh"))
        .arg(&root)
        .output()
        .expect("reject invalid legacy link");
    assert!(!invalid.status.success());
    fs::remove_file(root.join("scratch/merge-queue")).expect("remove invalid legacy link");

    let custom_root = temp.path().join("custom-state");
    assert_eq!(
        resolve_merge_queue_state(&root, &[("WITCHY_STATE_DIR", &custom_root)]),
        custom_root.join("merge-queue"),
    );
    let exact = temp.path().join("exact-queue");
    assert_eq!(
        resolve_merge_queue_state(&root, &[("MERGE_QUEUE_STATE_DIR", &exact)]),
        exact,
    );
}

#[test]
fn migrate_state_requires_a_drained_queue_and_leaves_legacy_compatibility() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let root = temp.path().join("root");
    let legacy = root.join("scratch/merge-queue");
    let queue = legacy.join("queue");
    fs::create_dir_all(&queue).expect("create legacy queue");
    fs::create_dir_all(legacy.join("logs")).expect("create legacy logs");
    fs::write(
        legacy.join("journal.jsonl"),
        "{\"ts\":\"2026-07-16T00:00:00Z\",\"event\":\"submitted\",\"branch\":\"old\"}\n",
    )
    .expect("write legacy journal");
    fs::write(legacy.join("logs/old.log"), "old gate log\n").expect("write old log");

    let ungated = Command::new(repo.join("scripts/merge-queue.sh"))
        .arg("migrate-state")
        .env("MERGE_QUEUE_TEST_ROOT", &root)
        .output()
        .expect("reject ungated test root");
    assert!(!ungated.status.success(), "test root bypassed its explicit guard");

    let run_migration = || {
        Command::new(repo.join("scripts/merge-queue.sh"))
            .arg("migrate-state")
            .env("MERGE_QUEUE_TEST_ROOT", &root)
            .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
            .output()
            .expect("run isolated state migration")
    };

    fs::write(queue.join("pending.json"), "{}\n").expect("write pending queue item");
    let refused = run_migration();
    assert!(!refused.status.success(), "migration ignored a non-empty queue");
    assert!(legacy.is_dir());
    assert!(!root.join("state/merge-queue").exists());
    fs::remove_file(queue.join("pending.json")).expect("drain fixture queue");

    let migrated = run_migration();
    assert!(
        migrated.status.success(),
        "migration failed: {}",
        String::from_utf8_lossy(&migrated.stderr),
    );
    assert_eq!(
        fs::read_link(&legacy).expect("legacy path is a symlink"),
        PathBuf::from("../state/merge-queue"),
    );
    assert_eq!(
        fs::read_to_string(legacy.join("logs/old.log")).expect("read log through legacy link"),
        "old gate log\n",
    );
    let journal = fs::read_to_string(root.join("state/merge-queue/journal.jsonl"))
        .expect("read migrated journal");
    assert!(journal.contains("\"branch\":\"old\""));
    assert!(journal.contains("\"event\":\"state_migrated\""));
    assert!(root.join("state/agents").is_dir());
    assert!(root.join("state/README.txt").is_file());
    assert!(!root.join("state/merge-queue/gate.lock").exists());
    assert!(!root.join("state/merge-queue/change.lock").exists());
    assert!(!root.join("state/merge-queue/coordinator.lock").exists());
    assert!(!root.join("state/merge-queue/coordinator.pid").exists());

    let repeated = run_migration();
    assert!(repeated.status.success(), "completed migration is not idempotent");
    let journal = fs::read_to_string(root.join("state/merge-queue/journal.jsonl"))
        .expect("read journal after repeated migration");
    assert_eq!(journal.matches("\"event\":\"state_migrated\"").count(), 1);
}

#[test]
fn gate_report_is_read_only_and_aggregates_batches_failures_and_phases() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let logs = state.join("logs");
    fs::create_dir_all(&logs).expect("create report fixture logs");

    let green_log = logs.join("green.log");
    fs::write(
        &green_log,
        "==> [1] tests (workspace) (t+0s)\n\
         Finished `test` profile [unoptimized] target(s) in 20s\n\
         Starting 100 tests across 10 binaries\n\
         Summary [ 30.000s] 100 tests run: 100 passed\n\
         [1] tests (workspace) took 70s\n\
         [2] witchy fmt (std+examples) took 2s\n\
         WITCHY_TIMING {\"schema\":1,\"kind\":\"foreground\",\"step\":1,\"name\":\"tests (workspace)\",\"status\":\"green\",\"started_epoch\":10,\"finished_epoch\":81,\"elapsed_s\":71,\"gate_elapsed_s\":71}\n\
         WITCHY_TIMING {\"schema\":1,\"kind\":\"foreground\",\"step\":2,\"name\":\"witchy fmt (std+examples)\",\"status\":\"green\",\"started_epoch\":81,\"finished_epoch\":84,\"elapsed_s\":3,\"gate_elapsed_s\":74}\n\
         WITCHY_TIMING {\"schema\":1,\"kind\":\"background\",\"step\":3,\"name\":\"clippy (bug lints)\",\"status\":\"green\",\"started_epoch\":10,\"finished_epoch\":60,\"elapsed_s\":50,\"gate_elapsed_s\":50}\n\
         ==> [6] queue infrastructure (isolated) (t+74s)\n\
         Finished `test` profile [unoptimized] target(s) in 1s\n\
         Starting 37 tests across 1 binary\n\
         Summary [ 73.000s] 37 tests run: 37 passed\n\
         [6] queue infrastructure (isolated) took 74s\n",
    )
    .expect("write green fixture log");
    let timeout_log = logs.join("timeout.log");
    fs::write(&timeout_log, "[1] tests (workspace) still running\n")
        .expect("write timeout fixture log");
    let red_log = logs.join("red.log");
    fs::write(&red_log, "error: test run failed\n").expect("write red fixture log");

    let journal = format!(
        concat!(
            "{{\"ts\":\"2026-07-16T00:00:00Z\",\"event\":\"submitted\",\"branch\":\"a\"}}\n",
            "{{\"ts\":\"2026-07-16T00:00:10Z\",\"event\":\"submitted\",\"branch\":\"b\"}}\n",
            "{{\"ts\":\"2026-07-16T00:01:40Z\",\"event\":\"merged\",\"branch\":\"a\",\"elapsed_s\":\"302\",\"attempt_elapsed_s\":\"302\",\"lock_wait_s\":\"181\",\"prepare_elapsed_s\":\"5\",\"preflight_elapsed_s\":\"0\",\"gate_elapsed_s\":\"111\",\"landing_elapsed_s\":\"5\",\"batch\":\"2\",\"log\":{green:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:01:40Z\",\"event\":\"merged\",\"branch\":\"b\",\"elapsed_s\":\"302\",\"attempt_elapsed_s\":\"302\",\"lock_wait_s\":\"181\",\"prepare_elapsed_s\":\"5\",\"preflight_elapsed_s\":\"0\",\"gate_elapsed_s\":\"111\",\"landing_elapsed_s\":\"5\",\"batch\":\"2\",\"log\":{green:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:03:20Z\",\"event\":\"submitted\",\"branch\":\"c\"}}\n",
            "{{\"ts\":\"2026-07-16T00:04:20Z\",\"event\":\"timeout\",\"branch\":\"c\",\"elapsed_s\":\"60\",\"log\":{timeout:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:05:00Z\",\"event\":\"submitted\",\"branch\":\"d\"}}\n",
            "{{\"ts\":\"2026-07-16T00:05:40Z\",\"event\":\"red\",\"branch\":\"d\",\"elapsed_s\":\"40\",\"log\":{red:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:06:00Z\",\"event\":\"requeued\",\"branch\":\"e\"}}\n",
            "{{\"ts\":\"2026-07-16T00:06:10Z\",\"event\":\"conflict\",\"branch\":\"f\"}}\n",
            "{{\"ts\":\"2026-07-16T00:06:20Z\",\"event\":\"blocked\",\"branch\":\"g\"}}\n"
        ),
        green = green_log.to_string_lossy(),
        timeout = timeout_log.to_string_lossy(),
        red = red_log.to_string_lossy(),
    );
    fs::write(state.join("journal.jsonl"), journal).expect("write report fixture journal");

    let before = fs::read_to_string(state.join("journal.jsonl")).expect("read journal before");
    let report_path = temp.path().join("report.json");
    let error_path = temp.path().join("report.stderr");
    let report_file = fs::File::create(&report_path).expect("create report output");
    let error_file = fs::File::create(&error_path).expect("create report stderr");
    let status = Command::new("bash")
        .arg(root.join("scripts/gate-report.sh"))
        .args(["--state-dir", state.to_str().unwrap(), "--since", "all", "--json"])
        .stdout(Stdio::from(report_file))
        .stderr(Stdio::from(error_file))
        .status()
        .expect("run gate report");
    assert!(
        status.success(),
        "gate report failed: {}",
        fs::read_to_string(error_path).expect("read report stderr")
    );
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(report_path).expect("read report output"),
    )
    .expect("gate report emits JSON");

    assert_eq!(report["throughput"]["merged_branches"], 2);
    assert_eq!(report["throughput"]["green_gates"], 1);
    assert_eq!(report["throughput"]["failed_attempts"], 2);
    assert_eq!(report["throughput"]["branches_per_green_gate"], 2.0);
    assert_eq!(report["throughput"]["batched_gates"], 1);
    let failed_gate_minutes = report["throughput"]["failed_gate_minutes"]
        .as_f64()
        .expect("failed gate minutes is numeric");
    assert!((failed_gate_minutes - (100.0 / 60.0)).abs() < f64::EPSILON);
    assert_eq!(report["schema"], 2);
    assert_eq!(report["attempt_s"]["p50"], 60);
    assert_eq!(report["attempt_s"]["p90"], 302);
    assert_eq!(report["gate_s"]["p50"], 60);
    assert_eq!(report["gate_s"]["p90"], 111);
    assert_eq!(report["attempt_phases_s"]["lock_wait"]["count"], 1);
    assert_eq!(report["attempt_phases_s"]["lock_wait"]["p50"], 181);
    assert_eq!(report["attempt_phases_s"]["prepare"]["p50"], 5);
    assert_eq!(report["attempt_phases_s"]["preflight"]["p50"], 0);
    assert_eq!(report["attempt_phases_s"]["gate"]["count"], 3);
    assert_eq!(report["attempt_phases_s"]["landing"]["p50"], 5);
    assert_eq!(report["outcomes"]["requeued"], 1);
    assert_eq!(report["outcomes"]["conflict"], 1);
    assert_eq!(report["outcomes"]["blocked"], 1);
    assert_eq!(report["outcomes"]["automatic_retries"], 1);
    assert_eq!(report["phases_s"]["compile"]["p50"], 20);
    assert_eq!(report["phases_s"]["discovery_estimate"]["p50"], 21);
    assert_eq!(report["phases_s"]["execution"]["p50"], 30.0);
    assert_eq!(report["phases_s"]["test_stage"]["p50"], 71);
    assert_eq!(report["phases_s"]["auxiliary"]["p50"], 76);
    assert_eq!(report["structured_phases_s"]["tests"]["p50"], 71);
    assert_eq!(report["structured_phases_s"]["fmt"]["p50"], 3);
    assert_eq!(report["structured_phases_s"]["clippy"]["p50"], 50);

    let after = fs::read_to_string(state.join("journal.jsonl")).expect("read journal after");
    assert_eq!(before, after, "reporting must not mutate queue state");
}

#[test]
fn fast_gate_emits_structured_foreground_and_background_timings() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).expect("create fake tool directory");

    let tool = bin.join("cargo");
    fs::write(
        &tool,
        "#!/bin/sh\n\
         if [ -n \"${FAKE_CARGO_ARGS_FILE:-}\" ]; then printf '%s\\n' \"$*\" >>\"$FAKE_CARGO_ARGS_FILE\"; fi\n\
         if [ -n \"${FAKE_CARGO_ENV_FILE:-}\" ]; then printf '%s\\n' \"${CARGO_PROFILE_TEST_STRIP-unset}\" >>\"$FAKE_CARGO_ENV_FILE\"; fi\n\
         if [ \"$1\" = clippy ]; then\n\
           if [ -n \"${FAKE_CLIPPY_PID_FILE:-}\" ]; then printf '%s\\n' \"$$\" >\"$FAKE_CLIPPY_PID_FILE\"; sleep 30; exit 0; fi\n\
           sleep 1; exit 0\n\
         fi\n\
         if [ \"$1\" = nextest ] && [ \"$2\" = run ] && [ \"${FAKE_CARGO_FAIL_NEXTEST:-}\" != 1 ]; then sleep 3; exit 0; fi\n\
         if [ \"${FAKE_CARGO_FAIL_NEXTEST:-}\" = 1 ] && [ \"$1\" = nextest ] && [ \"$2\" = run ]; then\n\
           sleep 1; exit 7\n\
         fi\n\
         exit 0\n",
    )
    .expect("write fake cargo");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod fake cargo");
    let git = bin.join("git");
    fs::write(
        &git,
        "#!/bin/sh\n\
         if [ \"$1\" = rev-parse ] && [ \"$2\" = HEAD ]; then printf '%s\\n' \"${FAKE_GIT_HEAD:-}\"; fi\n\
         exit 0\n",
    )
    .expect("write fake git");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("chmod fake git");
    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        "#!/bin/sh\nif [ \"$1\" = which ]; then echo /usr/bin/true; fi\nexit 0\n",
    )
    .expect("write fake rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake rustup");

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let cargo_args_file = temp.path().join("cargo-args");
    let cargo_env_file = temp.path().join("cargo-env");
    let proof_sha = "0123456789abcdef0123456789abcdef01234567";
    let output = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--fast")
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target"))
        .env_remove("WITCHY_GATE_QUEUE_INFRA")
        .env_remove("CARGO_PROFILE_TEST_STRIP")
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_CENSUS_PROOF_SHA", proof_sha)
        .env("FAKE_GIT_HEAD", proof_sha)
        .env_remove("WITCHY_GATE_TEST_JOBS")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("FAKE_CARGO_ENV_FILE", &cargo_env_file)
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run fast gate with fake tools");
    assert!(
        output.status.success(),
        "fake fast gate failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let cargo_args = fs::read_to_string(&cargo_args_file).expect("read fake cargo arguments");
    assert!(
        cargo_args.lines().any(|line| {
            line.starts_with("nextest run --max-fail=1:immediate -j 8 --workspace")
        }),
        "serialized gate did not use fail-fast execution with the proven eight-job default: {cargo_args}",
    );
    assert!(
        cargo_args.lines().any(|line| {
            line.contains(
                "not test(/^rfc0087_migration_census::repository_census_matches_the_checked_in_type_resolved_snapshot$/)",
            )
        }),
        "serialized gate did not reuse its exact census proof: {cargo_args}",
    );
    let cargo_env = fs::read_to_string(&cargo_env_file).expect("read fake cargo environment");
    assert!(
        !cargo_env.trim().is_empty() && cargo_env.lines().all(|value| value == "symbols"),
        "serialized gate did not strip test symbols: {cargo_env}",
    );

    fs::write(&cargo_env_file, "").expect("clear fake cargo environment");
    let local = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--wasm")
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-local"))
        .env_remove("WITCHY_GATE_SCOPE")
        .env_remove("CARGO_PROFILE_TEST_STRIP")
        .env("FAKE_CARGO_ENV_FILE", &cargo_env_file)
        .output()
        .expect("run local shard with fake tools");
    assert!(local.status.success(), "fake local shard failed");
    let local_env =
        fs::read_to_string(&cargo_env_file).expect("read local cargo environment");
    assert!(
        !local_env.trim().is_empty() && local_env.lines().all(|value| value == "unset"),
        "local builds must retain their normal test-symbol policy: {local_env}",
    );

    fs::write(&cargo_env_file, "").expect("clear fake cargo environment");
    let overridden = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--wasm")
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-overridden"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env("CARGO_PROFILE_TEST_STRIP", "none")
        .env("FAKE_CARGO_ENV_FILE", &cargo_env_file)
        .output()
        .expect("run overridden gate shard with fake tools");
    assert!(overridden.status.success(), "fake overridden shard failed");
    let overridden_env =
        fs::read_to_string(&cargo_env_file).expect("read overridden cargo environment");
    assert!(
        !overridden_env.trim().is_empty()
            && overridden_env.lines().all(|value| value == "none"),
        "an explicit Cargo strip policy must remain authoritative: {overridden_env}",
    );

    let stdout = String::from_utf8(output.stdout).expect("check output is utf8");
    let timings: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("WITCHY_TIMING "))
        .map(|json| serde_json::from_str(json).expect("timing record is JSON"))
        .collect();
    assert_eq!(timings.len(), 2, "expected test and clippy timings: {stdout}");
    assert_eq!(timings[0]["kind"], "foreground");
    assert_eq!(timings[0]["name"], "tests (workspace, minus e2e)");
    assert_eq!(timings[0]["status"], "green");
    assert!(timings[0]["elapsed_s"].as_u64().unwrap() >= 3);
    assert_eq!(timings[1]["kind"], "background");
    assert_eq!(timings[1]["name"], "clippy (bug lints)");
    assert_eq!(timings[1]["status"], "green");
    assert!(
        timings[1]["elapsed_s"].as_u64().unwrap() <= 2,
        "background timing included time spent waiting for foreground collection: {stdout}",
    );

    let isolated = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--fast")
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
        .env("CARGO_TARGET_DIR", temp.path().join("target-isolated"))
        .env("WITCHY_GATE_QUEUE_INFRA", "1")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run fast gate with isolated queue fixtures");
    assert!(
        isolated.status.success(),
        "isolated fake fast gate failed: {}",
        String::from_utf8_lossy(&isolated.stderr)
    );
    let isolated_stdout = String::from_utf8(isolated.stdout).expect("check output is utf8");
    let isolated_names: Vec<_> = isolated_stdout
        .lines()
        .filter_map(|line| line.strip_prefix("WITCHY_TIMING "))
        .map(|json| serde_json::from_str::<serde_json::Value>(json).expect("timing is JSON"))
        .map(|timing| timing["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        isolated_names,
        [
            "tests (workspace, minus e2e)",
            "clippy (bug lints)",
            "queue infrastructure (isolated)",
        ],
        "queue fixtures did not run alone after product work: {isolated_stdout}",
    );
    let isolated_args =
        fs::read_to_string(&cargo_args_file).expect("read isolated fake cargo arguments");
    assert!(
        isolated_args
            .lines()
            .any(|line| line == "nextest run --test merge_queue -j 2"),
        "queue fixtures did not use the bounded hermetic width: {isolated_args}",
    );
    let invalid_width = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--queue-infra")
        .env("PATH", &path)
        .env("WITCHY_QUEUE_INFRA_JOBS", "0")
        .output()
        .expect("reject invalid queue fixture width");
    assert!(!invalid_width.status.success());
    assert!(
        String::from_utf8_lossy(&invalid_width.stderr)
            .contains("WITCHY_QUEUE_INFRA_JOBS must be a positive integer")
    );

    let clippy_pid_file = temp.path().join("red-clippy.pid");
    let failed = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--fast")
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
        .env("CARGO_TARGET_DIR", temp.path().join("target-red"))
        .env_remove("WITCHY_GATE_QUEUE_INFRA")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .env("FAKE_CARGO_FAIL_NEXTEST", "1")
        .env("FAKE_CLIPPY_PID_FILE", &clippy_pid_file)
        .output()
        .expect("run red fast gate with fake tools");
    assert_eq!(failed.status.code(), Some(7));
    let clippy_pid = fs::read_to_string(&clippy_pid_file)
        .expect("red clippy leg recorded its pid")
        .trim()
        .parse::<i32>()
        .expect("parse red clippy pid");
    assert!(
        !process_is_alive(clippy_pid),
        "foreground failure orphaned background clippy pid {clippy_pid}",
    );
    let stdout = String::from_utf8(failed.stdout).expect("red check output is utf8");
    let timing = stdout
        .lines()
        .find_map(|line| line.strip_prefix("WITCHY_TIMING "))
        .map(|json| serde_json::from_str::<serde_json::Value>(json).expect("red timing is JSON"))
        .expect("red foreground timing was emitted");
    assert_eq!(timing["kind"], "foreground");
    assert_eq!(timing["name"], "tests (workspace, minus e2e)");
    assert_eq!(timing["status"], "red");
}

/// Change 1 (gate fail-fast): the full merge-gate profile overlaps a compile
/// check, clippy, and the wasm build behind the foreground tests, and aborts
/// the tests the moment a background leg records a failure. Green path first:
/// stage order and timing records are pinned so observability consumers
/// (gate-report.sh, the journal stage summaries) keep parsing.
#[test]
fn full_gate_fail_fast_aborts_tests_when_a_background_leg_goes_red() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).expect("create fake tool directory");

    let tool = bin.join("cargo");
    fs::write(
        &tool,
        "#!/bin/sh\n\
         if [ \"$1\" = check ]; then sleep \"${FAKE_CARGO_CHECK_SECS:-0}\"; exit 0; fi\n\
         if [ \"$1\" = clippy ]; then\n\
           sleep 1\n\
           if [ \"${FAKE_CARGO_FAIL_CLIPPY:-0}\" = 1 ]; then echo 'error: fake lint failure'; exit 5; fi\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = nextest ] && [ \"$2\" = run ]; then\n\
           if [ -n \"${FAKE_NEXTEST_PID_FILE:-}\" ]; then printf '%s\\n' \"$$\" >\"$FAKE_NEXTEST_PID_FILE\"; fi\n\
           exec sleep \"${FAKE_CARGO_NEXTEST_SECS:-3}\"\n\
         fi\n\
         exit 0\n",
    )
    .expect("write fake cargo");
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).expect("chmod fake cargo");
    let git = bin.join("git");
    fs::write(
        &git,
        "#!/bin/sh\n\
         case \"$*\" in\n\
           *\"rev-parse --verify HEAD\"*) echo 1111111111111111111111111111111111111111 ;;\n\
         esac\n\
         exit 0\n",
    )
    .expect("write fake git");
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).expect("chmod fake git");
    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        "#!/bin/sh\nif [ \"$1\" = which ]; then echo /usr/bin/true; fi\nexit 0\n",
    )
    .expect("write fake rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake rustup");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    // The fmt stage runs the nextest-built binary from the target dir; give
    // the fake toolchain one.
    let target = temp.path().join("target-full");
    fs::create_dir_all(target.join("debug")).expect("create fake target dir");
    let witchy = target.join("debug/witchy");
    fs::write(
        &witchy,
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then\n\
           echo 'witchy 0.1.0 (commit 1111111111111111111111111111111111111111)'\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = fmt ]; then exit 0; fi\n\
         case \"$2\" in\n\
           */scalar_int.witchy) result=24000006 ;;\n\
           */scalar_float.witchy) result=1 ;;\n\
           */packed_records.witchy) result=9599879 ;;\n\
           */list_pipeline.witchy) result=12000160 ;;\n\
           */closed_sum.witchy) result=16833142 ;;\n\
           */generic_helpers.witchy) result=9599942 ;;\n\
           */destination_record.witchy) result=49999959 ;;\n\
           */recursive_values.witchy) result=9999190 ;;\n\
           *) exit 1 ;;\n\
         esac\n\
         printf 'result=%s\\nbench_ns=1\\n' \"$result\"\n",
    )
    .expect("write fake witchy");
    fs::set_permissions(&witchy, fs::Permissions::from_mode(0o755))
        .expect("chmod fake witchy");

    let green = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("WITCHY_GATE_SCOPE")
        .env_remove("WITCHY_GATE_QUEUE_INFRA")
        .env_remove("WITCHY_FAILFAST_POLL")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run green full gate with fake tools");
    assert!(
        green.status.success(),
        "fake full gate failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&green.stdout),
        String::from_utf8_lossy(&green.stderr),
    );
    let stdout = String::from_utf8(green.stdout).expect("green output is utf8");
    let names: Vec<_> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("WITCHY_TIMING "))
        .map(|json| serde_json::from_str::<serde_json::Value>(json).expect("timing is JSON"))
        .map(|timing| timing["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "tests (workspace)",
            "witchy fmt (std+examples)",
            "prose style (no em dashes)",
            "compile check (cargo check)",
            "clippy (bug lints)",
            "wasm playground build",
            "runnable book (browser)",
            "Rust-class paired correctness",
        ],
        "full-gate stage/timing order changed: {stdout}",
    );

    // Red leg: clippy fails at ~1s while the tests would run 60s. Fail-fast
    // must abort the foreground stage, surface clippy's output, and exit red
    // in seconds, not after the test stage.
    let target_red = temp.path().join("target-full-red");
    fs::create_dir_all(target_red.join("debug")).expect("create red fake target dir");
    let witchy_red = target_red.join("debug/witchy");
    fs::write(&witchy_red, "#!/bin/sh\nexit 0\n").expect("write red fake witchy");
    fs::set_permissions(&witchy_red, fs::Permissions::from_mode(0o755))
        .expect("chmod red fake witchy");
    let nextest_pid_file = temp.path().join("nextest.pid");
    let started = Instant::now();
    let red = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", &target_red)
        .env_remove("WITCHY_GATE_SCOPE")
        .env_remove("WITCHY_GATE_QUEUE_INFRA")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .env("WITCHY_FAILFAST_POLL", "1")
        .env("FAKE_CARGO_FAIL_CLIPPY", "1")
        .env("FAKE_CARGO_NEXTEST_SECS", "60")
        .env("FAKE_NEXTEST_PID_FILE", &nextest_pid_file)
        .output()
        .expect("run red full gate with fake tools");
    let elapsed = started.elapsed();
    assert!(!red.status.success(), "red clippy leg did not fail the gate");
    assert!(
        elapsed < Duration::from_secs(45),
        "fail-fast did not abort the 60s test stage promptly: {elapsed:?}",
    );
    let stdout = String::from_utf8(red.stdout).expect("red output is utf8");
    assert!(
        stdout.contains("aborting the foreground stage (fail-fast)"),
        "missing fail-fast abort marker: {stdout}",
    );
    assert!(
        stdout.contains("fake lint failure"),
        "red leg output was not surfaced: {stdout}",
    );
    let timings: Vec<serde_json::Value> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("WITCHY_TIMING "))
        .map(|json| serde_json::from_str(json).expect("timing record is JSON"))
        .collect();
    let tests = timings
        .iter()
        .find(|timing| timing["name"] == "tests (workspace)")
        .expect("aborted foreground timing was emitted");
    assert_eq!(tests["status"], "aborted");
    let clippy = timings
        .iter()
        .find(|timing| timing["name"] == "clippy (bug lints)")
        .expect("red clippy timing was emitted");
    assert_eq!(clippy["kind"], "background");
    assert_eq!(clippy["status"], "red");
    let nextest_pid = fs::read_to_string(&nextest_pid_file)
        .expect("fake nextest recorded its pid")
        .trim()
        .parse::<i32>()
        .expect("parse fake nextest pid");
    assert!(
        !process_is_alive(nextest_pid),
        "fail-fast abort orphaned the foreground test process {nextest_pid}",
    );
}

/// Change 3 (culprit eviction): an unrelated red batch whose failure names a
/// file only one member touches evicts THAT member to a solo gate and lands
/// the remaining members as one green batch — two follow-up gates instead of
/// one per member.
#[test]
fn unrelated_red_batch_evicts_the_culprit_and_rebatches_the_rest() {
    let fixture = QueueFixture::stack(&[]);
    let root = fixture.root.clone();
    for (branch, file) in [
        ("notes-a", "notes-a.txt"),
        ("widget-fix", "tests/foo_widget.rs"),
        ("notes-b", "notes-b.txt"),
    ] {
        run_git(&root, &["switch", "-c", branch, "master"]);
        if let Some(parent) = root.join(file).parent() {
            fs::create_dir_all(parent).expect("create branch file parent");
        }
        fs::write(root.join(file), format!("{branch}\n")).expect("write branch file");
        run_git(&root, &["add", file]);
        run_git(&root, &["commit", "-m", &format!("add {file}")]);
        run_git(&root, &["switch", "master"]);
    }
    fixture.mq_ok(&["submit", "notes-a"], "true");
    fixture.mq_ok(&["submit", "widget-fix"], "true");
    fixture.mq_ok(&["submit", "notes-b"], "true");

    // Red exactly while the culprit's file is in the candidate tree, with a
    // nextest-shaped FAIL line naming the culprit's test binary.
    let gate = "test ! -f tests/foo_widget.rs || { printf 'FAIL [   0.42s] witchy::foo_widget foo_widget::renders_the_widget\\n'; exit 1; }";
    fixture.mq_ok(&["run", "--once"], gate);

    let journal = fixture.journal();
    let batch_reds: Vec<_> = journal
        .iter()
        .filter(|event| event["event"] == "batch_red")
        .collect();
    assert_eq!(batch_reds.len(), 1, "expected exactly one red batch: {journal:?}");
    assert_eq!(batch_reds[0]["strategy"], "culprit_evict");
    let evicted = journal
        .iter()
        .find(|event| event["event"] == "evicted")
        .expect("eviction decision was journaled");
    assert_eq!(evicted["branch"], "widget-fix");
    assert!(
        evicted["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("failing target")),
        "eviction reason is missing: {evicted}",
    );
    let merged: Vec<_> = journal
        .iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    let merged_branches: BTreeSet<_> = merged
        .iter()
        .map(|event| event["branch"].as_str().unwrap())
        .collect();
    assert_eq!(
        merged_branches,
        BTreeSet::from(["notes-a", "notes-b"]),
        "innocent members did not land: {journal:?}",
    );
    assert!(
        merged.iter().all(|event| event["batch"] == "2"),
        "remaining members were not re-gated as ONE batch: {merged:?}",
    );
    let merged_logs: BTreeSet<_> = merged
        .iter()
        .map(|event| event["log"].as_str().unwrap())
        .collect();
    assert_eq!(merged_logs.len(), 1, "remaining members used more than one gate");
    assert!(
        journal
            .iter()
            .any(|event| event["event"] == "red" && event["branch"] == "widget-fix"),
        "evicted culprit was not solo-gated to a terminal red: {journal:?}",
    );
    assert!(run_git(&root, &["show", "master:notes-a.txt"]).status.success());
    assert!(run_git(&root, &["show", "master:notes-b.txt"]).status.success());
    let culprit_on_master = Command::new("git")
        .args(["-C", root.to_str().unwrap(), "show", "master:tests/foo_widget.rs"])
        .output()
        .expect("inspect culprit on master");
    assert!(!culprit_on_master.status.success(), "the red culprit landed");
}

/// Change 2 (snapshot re-baseline): a candidate whose generated snapshot went
/// stale is amended with `chore(gate): re-baseline generated artifacts` during
/// prepare; the amended sha is what gets gated and fast-forwarded. A failing
/// generator never fails the candidate — regen is skipped and the gate
/// adjudicates.
#[test]
fn prepare_rebaselines_stale_generated_snapshots_onto_the_gated_sha() {
    let fixture = QueueFixture::stack(&[]);
    let root = fixture.root.clone();

    // A committed (stale) snapshot on master, and a branch touching a census
    // input (std/ is outside the docs-safe set, so regen triggers).
    fs::create_dir_all(root.join("rfcs")).expect("create rfcs dir");
    fs::write(root.join("rfcs/0087-migration-census.tsv"), "stale\n").expect("write stale tsv");
    run_git(&root, &["add", "rfcs/0087-migration-census.tsv"]);
    run_git(&root, &["commit", "-m", "stale census snapshot"]);
    run_git(&root, &["switch", "-c", "census-branch", "master"]);
    fs::create_dir_all(root.join("std")).expect("create std dir");
    fs::write(root.join("std/foo.witchy"), "## doc\n").expect("write std input");
    run_git(&root, &["add", "std/foo.witchy"]);
    run_git(&root, &["commit", "-m", "touch census input"]);
    run_git(&root, &["switch", "master"]);

    // Pre-create the coordinator's gate worktree and plant fake generator
    // binaries plus a fake `cargo` so the regen build step succeeds.
    let gate = fixture.gate_worktree.clone();
    run_git(
        &root,
        &["worktree", "add", "--detach", gate.to_str().unwrap(), "master"],
    );
    fs::create_dir_all(gate.join("target/debug")).expect("create fake gate target");
    let census = gate.join("target/debug/rfc0087-census");
    fs::write(&census, "#!/bin/sh\nprintf 'fresh\\n'\n").expect("write fake census generator");
    fs::set_permissions(&census, fs::Permissions::from_mode(0o755))
        .expect("chmod fake census generator");
    let bin = fixture._temp.path().join("regen-bin");
    fs::create_dir(&bin).expect("create fake cargo dir");
    let cargo = bin.join("cargo");
    let incremental = fixture._temp.path().join("regen-cargo-incremental");
    let proof_marker = fixture._temp.path().join("census-proof-sha");
    let proof_gate = fixture._temp.path().join("capture-census-proof");
    fs::write(
        &proof_gate,
        format!(
            "#!/bin/sh\nprintf '%s' \"${{WITCHY_GATE_CENSUS_PROOF_SHA:-missing}}\" >{}\n",
            proof_marker.display(),
        ),
    )
    .expect("write census-proof capture gate");
    fs::set_permissions(&proof_gate, fs::Permissions::from_mode(0o755))
        .expect("chmod census-proof capture gate");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nprintf '%s' \"${{CARGO_INCREMENTAL-unset}}\" >{}\nexit 0\n",
            incremental.display(),
        ),
    )
    .expect("write fake cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).expect("chmod fake cargo");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    fixture.mq_ok(&["submit", "census-branch"], "true");
    let output = fixture
        .mq_command(&["run", "--once"], proof_gate.to_str().unwrap())
        .env("PATH", &path)
        .output()
        .expect("run rebaselining coordinator");
    assert!(
        output.status.success(),
        "rebaselining run failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let journal = fixture.journal();
    let rebaselined = journal
        .iter()
        .find(|event| event["event"] == "rebaselined")
        .expect("rebaseline was journaled");
    assert_eq!(rebaselined["branch"], "census-branch");
    assert_eq!(rebaselined["files"], "rfcs/0087-migration-census.tsv");
    assert!(
        journal
            .iter()
            .any(|event| event["event"] == "merged" && event["branch"] == "census-branch"),
        "amended candidate did not land: {journal:?}",
    );
    assert_eq!(
        fs::read_to_string(&incremental).expect("read preparation Cargo profile"),
        "0",
        "preparation did not match the full gate's incremental setting",
    );
    assert_eq!(
        git(&root, &["show", "master:rfcs/0087-migration-census.tsv"]),
        "fresh",
        "master does not carry the regenerated snapshot",
    );
    assert_eq!(
        git(&root, &["log", "-1", "--format=%s", "master"]),
        "chore(gate): re-baseline generated artifacts",
        "the gated+merged tip is not the amended sha",
    );
    assert_eq!(
        fs::read_to_string(&proof_marker).expect("read exact census proof"),
        git(&root, &["rev-parse", "master"]),
        "the gate did not receive preparation's exact amended-tree proof",
    );

    // A broken generator must not fail the candidate: regen is skipped.
    fs::write(&census, "#!/bin/sh\nexit 1\n").expect("break fake census generator");
    fs::set_permissions(&census, fs::Permissions::from_mode(0o755))
        .expect("chmod broken census generator");
    run_git(&root, &["switch", "-c", "census-broken", "master"]);
    fs::write(root.join("std/bar.witchy"), "## doc\n").expect("write second std input");
    run_git(&root, &["add", "std/bar.witchy"]);
    run_git(&root, &["commit", "-m", "touch census input again"]);
    run_git(&root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "census-broken"], "true");
    let output = fixture
        .mq_command(&["run", "--once"], proof_gate.to_str().unwrap())
        .env("PATH", &path)
        .output()
        .expect("run coordinator with broken generator");
    assert!(
        output.status.success(),
        "broken generator failed the queue run: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let journal = fixture.journal();
    assert!(
        journal
            .iter()
            .any(|event| event["event"] == "merged" && event["branch"] == "census-broken"),
        "broken generator blocked an unrelated candidate: {journal:?}",
    );
    assert_eq!(
        journal
            .iter()
            .filter(|event| event["event"] == "rebaselined")
            .count(),
        1,
        "a failing generator still produced a rebaseline commit",
    );
    assert_eq!(
        git(&root, &["show", "master:rfcs/0087-migration-census.tsv"]),
        "fresh",
        "a failing generator modified the snapshot",
    );
    assert_eq!(
        fs::read_to_string(&proof_marker).expect("read failed-generator proof marker"),
        "missing",
        "a failing generator incorrectly authorized census-proof reuse",
    );
}
