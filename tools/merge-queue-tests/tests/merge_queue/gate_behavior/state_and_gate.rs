use super::*;

fn resolve_merge_queue_state(root: &Path, envs: &[(&str, &Path)]) -> PathBuf {
    let repo = workspace_root();
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
    let repo = workspace_root();
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
    let repo = workspace_root();
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
    let root = workspace_root();
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
    let batch_red_log = logs.join("batch-red.log");
    fs::write(&batch_red_log, "error: batch test run failed\n")
        .expect("write batch-red fixture log");
    let old_red_log = logs.join("old-red.log");
    fs::write(&old_red_log, "error: old unrelated run failed\n")
        .expect("write old red fixture log");

    let journal = format!(
        concat!(
            "{{\"ts\":\"2026-07-15T23:59:00Z\",\"event\":\"red\",\"branch\":\"a\",\"elapsed_s\":\"10\",\"attempt_elapsed_s\":\"10\",\"gate_elapsed_s\":\"10\",\"log\":{old_red:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:00:00Z\",\"event\":\"submitted\",\"branch\":\"a\",\"change_id\":\"change-a\",\"attempt_id\":\"attempt-a0\",\"submitted_sha\":\"sha-a0\",\"presubmit_sha_match\":\"true\",\"presubmit_edit_elapsed_s\":\"12\",\"touches\":\"codegen\"}}\n",
            "{{\"ts\":\"2026-07-16T00:00:10Z\",\"event\":\"submitted\",\"branch\":\"b\",\"change_id\":\"change-b\",\"attempt_id\":\"attempt-b0\",\"submitted_sha\":\"sha-b0\",\"touches\":\"codegen\"}}\n",
            "{{\"ts\":\"2026-07-16T00:00:20Z\",\"event\":\"batch_red\",\"branch\":\"a\",\"members\":\"a b\",\"elapsed_s\":\"10\",\"attempt_elapsed_s\":\"10\",\"lock_wait_s\":\"4\",\"gate_elapsed_s\":\"10\",\"log\":{batch_red:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:00:30Z\",\"event\":\"resubmitted\",\"branch\":\"a\",\"change_id\":\"change-a\",\"attempt_id\":\"attempt-a1\",\"submitted_sha\":\"sha-a1\",\"touches\":\"codegen\"}}\n",
            "{{\"ts\":\"2026-07-16T00:00:35Z\",\"event\":\"resubmitted\",\"branch\":\"b\",\"change_id\":\"change-b\",\"attempt_id\":\"attempt-b1\",\"submitted_sha\":\"sha-b1\",\"presubmit_edit_elapsed_s\":\"30\",\"touches\":\"runtime\"}}\n",
            "{{\"ts\":\"2026-07-16T00:00:40Z\",\"event\":\"coordinator_started\",\"branch\":\"a\",\"attempt_id\":\"attempt-a1\"}}\n",
            "{{\"ts\":\"2026-07-16T00:01:00Z\",\"event\":\"resubmitted\",\"branch\":\"a\",\"change_id\":\"change-a\",\"attempt_id\":\"attempt-a2\",\"submitted_sha\":\"sha-a2\",\"touches\":\"codegen\"}}\n",
            "{{\"ts\":\"2026-07-16T00:01:40Z\",\"event\":\"merged\",\"branch\":\"a\",\"change_id\":\"change-a\",\"attempt_id\":\"attempt-a1\",\"submitted_sha\":\"sha-a1\",\"elapsed_s\":\"302\",\"gate_load_start\":\"2.00\",\"gate_load_end\":\"2.10\",\"host_ncpu\":\"10\",\"attempt_elapsed_s\":\"302\",\"lock_wait_s\":\"181\",\"prepare_elapsed_s\":\"5\",\"preflight_elapsed_s\":\"0\",\"gate_elapsed_s\":\"111\",\"landing_elapsed_s\":\"5\",\"batch\":\"2\",\"log\":{green:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:01:40Z\",\"event\":\"merged\",\"branch\":\"b\",\"change_id\":\"change-b\",\"attempt_id\":\"attempt-b1\",\"submitted_sha\":\"sha-b1\",\"elapsed_s\":\"302\",\"gate_load_start\":\"2.00\",\"gate_load_end\":\"2.10\",\"host_ncpu\":\"10\",\"attempt_elapsed_s\":\"302\",\"lock_wait_s\":\"181\",\"prepare_elapsed_s\":\"5\",\"preflight_elapsed_s\":\"0\",\"gate_elapsed_s\":\"111\",\"landing_elapsed_s\":\"5\",\"batch\":\"2\",\"log\":{green:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:03:20Z\",\"event\":\"submitted\",\"branch\":\"c\",\"presubmit_edit_elapsed_s\":\"\",\"presubmit_eligible\":\"false\"}}\n",
            "{{\"ts\":\"2026-07-16T00:04:20Z\",\"event\":\"timeout\",\"branch\":\"c\",\"elapsed_s\":\"60\",\"gate_load_start\":\"40.00\",\"gate_load_end\":\"41.00\",\"host_ncpu\":\"10\",\"log\":{timeout:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:05:00Z\",\"event\":\"submitted\",\"branch\":\"d\"}}\n",
            "{{\"ts\":\"2026-07-16T00:05:40Z\",\"event\":\"red\",\"branch\":\"d\",\"elapsed_s\":\"40\",\"log\":{red:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:06:00Z\",\"event\":\"requeued\",\"branch\":\"e\"}}\n",
            "{{\"ts\":\"2026-07-16T00:06:10Z\",\"event\":\"conflict\",\"branch\":\"f\",\"elapsed_s\":\"99\",\"attempt_elapsed_s\":\"99\",\"gate_elapsed_s\":\"99\"}}\n",
            "{{\"ts\":\"2026-07-16T00:06:20Z\",\"event\":\"blocked\",\"branch\":\"g\"}}\n"
        ),
        green = green_log.to_string_lossy(),
        timeout = timeout_log.to_string_lossy(),
        red = red_log.to_string_lossy(),
        batch_red = batch_red_log.to_string_lossy(),
        old_red = old_red_log.to_string_lossy(),
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
    assert_eq!(report["throughput"]["failed_attempts"], 4);
    assert_eq!(report["throughput"]["branches_per_green_gate"], 2.0);
    assert_eq!(report["throughput"]["batched_gates"], 1);
    let failed_gate_minutes = report["throughput"]["failed_gate_minutes"]
        .as_f64()
        .expect("failed gate minutes is numeric");
    assert!((failed_gate_minutes - (120.0 / 60.0)).abs() < f64::EPSILON);
    assert_eq!(report["schema"], 3);
    assert_eq!(report["attempt_s"]["p50"], 40);
    assert_eq!(report["attempt_s"]["p90"], 302);
    assert_eq!(report["gate_s"]["p50"], 40);
    assert_eq!(report["gate_s"]["p90"], 111);
    assert_eq!(report["attempt_phases_s"]["lock_wait"]["count"], 2);
    assert_eq!(report["attempt_phases_s"]["lock_wait"]["p50"], 4);
    assert_eq!(report["attempt_phases_s"]["lock_wait"]["p90"], 181);
    assert_eq!(report["attempt_phases_s"]["prepare"]["p50"], 5);
    assert_eq!(report["attempt_phases_s"]["preflight"]["p50"], 0);
    assert_eq!(report["attempt_phases_s"]["gate"]["count"], 5);
    assert_eq!(report["attempt_phases_s"]["landing"]["p50"], 5);
    assert_eq!(report["development_latency"]["coverage"]["edit_to_submit"]["available"], false);
    assert_eq!(report["development_latency"]["coverage"]["final_submit_to_land"]["samples"], 2);
    assert_eq!(report["development_latency"]["final_submit_to_land_s"]["p50"], 65);
    assert_eq!(report["development_latency"]["final_submit_to_land_s"]["max"], 70);
    assert_eq!(report["development_latency"]["shared_batch_dedup"]["terminal_events"], 6);
    assert_eq!(report["development_latency"]["shared_batch_dedup"]["unique_attempts"], 5);
    assert_eq!(report["development_latency"]["shared_batch_dedup"]["duplicate_member_events"], 1);
    assert_eq!(report["development_latency"]["rework"]["affected_changes"], 2);
    assert_eq!(report["development_latency"]["rework"]["unique_failed_attempts"], 4);
    assert_eq!(report["development_latency"]["rework"]["unique_failed_attempt_s_total"], 120);
    assert_eq!(report["development_latency"]["rework"]["unique_failed_work_s_total"], 116);
    assert_eq!(report["development_latency"]["rework"]["unique_failed_lock_wait_s_total"], 4);
    assert_eq!(report["development_latency"]["rework"]["failed_attempts_per_affected_change"]["p50"], 1);
    assert_eq!(report["development_latency"]["rework"]["failed_attempts_per_affected_change"]["max"], 1);
    assert_eq!(report["development_latency"]["rework"]["failed_attempt_s_per_affected_change"]["max"], 10);
    assert_eq!(report["development_latency"]["rework"]["failed_work_s_per_affected_change"]["max"], 6);
    assert_eq!(report["development_latency"]["rework"]["failed_lock_wait_s_per_affected_change"]["max"], 4);
    assert_eq!(report["outcomes"]["requeued"], 1);
    assert_eq!(report["outcomes"]["conflict"], 1);
    assert_eq!(report["throughput"]["failed_attempts"], 4, "conflict is not a failed gate");

    // Contention covariate. Branch `d` deliberately carries no load fields:
    // journals written before the signal landed must read as "no sample"
    // rather than as a zero-load gate, which would drag every bucket toward
    // idle and make a loaded box look fast.
    assert_eq!(report["contention"]["samples"], 2);
    let buckets = report["contention"]["buckets"]
        .as_array()
        .expect("contention buckets are an array");
    assert_eq!(buckets[0]["n"], 1, "idle bucket: {buckets:?}");
    assert_eq!(buckets[0]["red_rate"], 0.0, "idle gate did not fail: {buckets:?}");
    assert_eq!(buckets[2]["n"], 1, "saturated bucket: {buckets:?}");
    assert_eq!(buckets[2]["red_rate"], 1.0, "saturated gate failed: {buckets:?}");
    assert_eq!(report["outcomes"]["blocked"], 1);
    assert_eq!(report["outcomes"]["batch_red"], 1);
    assert_eq!(report["outcomes"]["automatic_retries"], 2);
    assert_eq!(report["attribution"]["presubmit_edit_s"]["count"], 2);
    assert_eq!(report["attribution"]["presubmit_edit_s"]["p50"], 12);
    assert_eq!(report["attribution"]["presubmit_edit_s"]["p90"], 30);
    assert_eq!(report["attribution"]["presubmit_eligible_submissions"], 6);
    assert_eq!(report["attribution"]["presubmit_ineligible_submissions"], 1);
    assert_eq!(
        report["attribution"]["by_actor_hint"],
        serde_json::json!([{"actor_hint": "unknown", "submissions": 7}])
    );
    assert_eq!(
        report["attribution"]["receipt_rate_by_actor_hint"],
        serde_json::json!([{"actor_hint": "unknown", "submissions": 7, "eligible": 6, "receipts": 1, "receipt_rate": 1.0 / 6.0}])
    );
    let eligible_receipt_rate = report["attribution"]["presubmit_receipt_rate"]
        .as_f64()
        .expect("eligible receipt rate is numeric");
    assert!((eligible_receipt_rate - (1.0 / 6.0)).abs() < 1e-12);
    let all_receipt_rate = report["attribution"]["presubmit_receipt_rate_all"]
        .as_f64()
        .expect("all-submission receipt rate is numeric");
    assert!((all_receipt_rate - (1.0 / 7.0)).abs() < 1e-12);
    assert_eq!(
        report["attribution"]["missing_receipts_by_touches"],
        serde_json::json!([
            {"touches": "codegen", "submissions": 3},
            {"touches": "runtime", "submissions": 1},
            {"touches": "unclassified", "submissions": 1},
        ])
    );
    assert_eq!(
        report["attribution"]["missing_receipts_by_actor_hint_and_touches"],
        serde_json::json!([
            {"actor_hint": "unknown", "touches": "codegen", "submissions": 3},
            {"actor_hint": "unknown", "touches": "runtime", "submissions": 1},
            {"actor_hint": "unknown", "touches": "unclassified", "submissions": 1},
        ])
    );
    assert_eq!(report["phases_s"]["compile"]["p50"], 20);
    assert_eq!(report["phases_s"]["discovery_estimate"]["p50"], 21);
    assert_eq!(report["phases_s"]["execution"]["p50"], 30.0);
    assert_eq!(report["phases_s"]["test_stage"]["p50"], 71);
    assert_eq!(report["phases_s"]["auxiliary"]["p50"], 76);
    assert_eq!(report["structured_phases_s"]["tests"]["p50"], 71);
    assert_eq!(report["structured_phases_s"]["fmt"]["p50"], 3);
    assert_eq!(report["structured_phases_s"]["clippy"]["p50"], 50);

    let absolute_report_path = temp.path().join("report-absolute.json");
    let absolute_error_path = temp.path().join("report-absolute.stderr");
    let absolute_report_file = fs::File::create(&absolute_report_path).expect("create absolute report output");
    let absolute_error_file = fs::File::create(&absolute_error_path).expect("create absolute report stderr");
    let absolute_status = Command::new("bash")
        .arg(root.join("scripts/gate-report.sh"))
        .args([
            "--state-dir",
            state.to_str().unwrap(),
            "--since",
            "2026-07-16T00:00:10Z",
            "--json",
        ])
        .stdout(Stdio::from(absolute_report_file))
        .stderr(Stdio::from(absolute_error_file))
        .status()
        .expect("run gate report with an absolute cutoff");
    assert!(
        absolute_status.success(),
        "absolute gate report failed: {}",
        fs::read_to_string(absolute_error_path).expect("read absolute report stderr")
    );
    let absolute_report: serde_json::Value = serde_json::from_slice(
        &fs::read(absolute_report_path).expect("read absolute report output"),
    )
    .expect("absolute gate report emits JSON");
    assert_eq!(absolute_report["attribution"]["submissions"], 6);
    assert_eq!(absolute_report["throughput"]["merged_branches"], 2);

    let after = fs::read_to_string(state.join("journal.jsonl")).expect("read journal after");
    assert_eq!(before, after, "reporting must not mutate queue state");
}

/// The fixtures below assert on check.sh's exact stage list, so any gate knob
/// the coordinator exports must be scrubbed before check.sh is spawned. This
/// pins the scrub to the prefix rather than to a list somebody has to remember
/// to extend: a knob the coordinator gains later is covered here on the day it
/// is added.
#[test]
fn fixture_gate_invocations_scrub_every_ambient_gate_knob() {
    let temp = TempDir::new();
    // SAFETY: single-threaded test-local environment mutation, undone below.
    unsafe { std::env::set_var("WITCHY_GATE_A_KNOB_ADDED_LATER", "1") };
    let command = check_sh(temp.path(), &["--fast"]);
    let removed: Vec<_> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect();
    unsafe { std::env::remove_var("WITCHY_GATE_A_KNOB_ADDED_LATER") };
    assert!(
        removed.iter().any(|key| key == "WITCHY_GATE_A_KNOB_ADDED_LATER"),
        "a gate knob the fixture never heard of must still be scrubbed: {removed:?}",
    );
}

#[test]
fn fast_gate_emits_structured_foreground_and_background_timings() {
    let root = workspace_root();
    let temp = TempDir::new();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).expect("create fake tool directory");

    let tool = bin.join("cargo");
    fs::write(
        &tool,
        "#!/bin/sh\n\
         if [ -n \"${FAKE_CARGO_ARGS_FILE:-}\" ]; then printf '%s target=%s\\n' \"$*\" \"${CARGO_TARGET_DIR:-}\" >>\"$FAKE_CARGO_ARGS_FILE\"; fi\n\
         if [ -n \"${FAKE_CARGO_ENV_FILE:-}\" ]; then printf '%s\\n' \"${CARGO_PROFILE_TEST_STRIP-unset}\" >>\"$FAKE_CARGO_ENV_FILE\"; fi\n\
         if [ -n \"${FAKE_CARGO_JOBS_FILE:-}\" ]; then printf '%s\\n' \"${CARGO_BUILD_JOBS-unset}\" >>\"$FAKE_CARGO_JOBS_FILE\"; fi\n\
         if [ -n \"${FAKE_FUZZ_ENV_FILE:-}\" ]; then printf '%s\\n' \"${WITCHY_FUZZ_PROGRAMS-unset}\" >>\"$FAKE_FUZZ_ENV_FILE\"; fi\n\
         if [ \"$1\" = nextest ] && [ \"$2\" = run ] && [ \"${FAKE_CARGO_FAIL_NEXTEST:-}\" != 1 ]; then sleep \"${FAKE_CARGO_NEXTEST_SECS:-0}\"; exit 0; fi\n\
         if [ \"${FAKE_CARGO_FAIL_NEXTEST:-}\" = 1 ] && [ \"$1\" = nextest ] && [ \"$2\" = run ]; then\n\
           sleep 0.1; exit 7\n\
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
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755)).expect("chmod fake rustup");
    // This fixture invokes check.sh twelve times without changing repository
    // sources. Return the already-audited ignore-policy census instead of
    // rescanning every Rust file twelve times; test-nextest-list-wrapper.sh
    // retains the real positive and negative policy checks.
    let find = bin.join("find");
    fs::write(
        &find,
        "#!/bin/sh\n\
         if [ \"$1\" = src ]; then exit 0; fi\n\
         PATH=/usr/bin:/bin:/usr/sbin:/sbin exec find \"$@\"\n",
    )
    .expect("write fake ignore-policy finder");
    fs::set_permissions(&find, fs::Permissions::from_mode(0o755))
        .expect("chmod fake ignore-policy finder");

    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    // The command-shape groups have separate target and capture paths. Run
    // them together so their repeated read-only check.sh startup does not
    // serialize the queue shard; each group retains its original assertions.
    std::thread::scope(|scope| {
        let timing_and_strip = scope.spawn(|| {
    let cargo_args_file = temp.path().join("cargo-args-timing");
    let cargo_env_file = temp.path().join("cargo-env-timing");
    let cargo_jobs_file = temp.path().join("cargo-jobs-timing");
    let proof_sha = "0123456789abcdef0123456789abcdef01234567";
    // Only this invocation asserts a nonzero foreground duration. The later
    // command-shape cases use the same fake without paying an unrelated delay.
    let output = check_sh(&root, &["--fast"])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target"))
        .env_remove("CARGO_PROFILE_TEST_STRIP")
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_CENSUS_PROOF_SHA", proof_sha)
        .env("FAKE_GIT_HEAD", proof_sha)
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("FAKE_CARGO_ENV_FILE", &cargo_env_file)
        .env("FAKE_CARGO_JOBS_FILE", &cargo_jobs_file)
        .env_remove("CARGO_BUILD_JOBS")
        .env_remove("WITCHY_GATE_BUILD_JOBS")
        .env("FAKE_CARGO_NEXTEST_SECS", "1")
        .env("WITCHY_FAILFAST_POLL", "0.05")
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
        cargo_args.lines().all(|line| !line.starts_with("clippy ")),
        "fast gate still launched Clippy: {cargo_args}",
    );
    assert!(
        cargo_args.lines().any(|line| line.contains("--features witchy-wir/native")),
        "serialized workspace gate did not enable RFC-0165 native security tests: {cargo_args}",
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
    let cargo_jobs = fs::read_to_string(&cargo_jobs_file).expect("read fake cargo build jobs");
    assert!(
        !cargo_jobs.trim().is_empty() && cargo_jobs.lines().all(|value| value == "8"),
        "serialized fast gate must give the foreground eight jobs: {cargo_jobs}",
    );

    fs::write(&cargo_env_file, "").expect("clear fake cargo environment");
    fs::write(&cargo_jobs_file, "").expect("clear fake cargo build jobs");
    let local = check_sh(&root, &["--examples"])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-local"))
        .env_remove("CARGO_PROFILE_TEST_STRIP")
        .env_remove("CARGO_BUILD_JOBS")
        .env("FAKE_CARGO_ENV_FILE", &cargo_env_file)
        .env("FAKE_CARGO_JOBS_FILE", &cargo_jobs_file)
        .output()
        .expect("run local shard with fake tools");
    assert!(local.status.success(), "fake local shard failed");
    let local_env =
        fs::read_to_string(&cargo_env_file).expect("read local cargo environment");
    assert!(
        !local_env.trim().is_empty() && local_env.lines().all(|value| value == "unset"),
        "local builds must retain their normal test-symbol policy: {local_env}",
    );
    let local_jobs = fs::read_to_string(&cargo_jobs_file).expect("read local cargo build jobs");
    assert!(
        !local_jobs.trim().is_empty() && local_jobs.lines().all(|value| value == "unset"),
        "local builds must keep the machine's build-job policy: {local_jobs}",
    );

    fs::write(&cargo_env_file, "").expect("clear fake cargo environment");
    let overridden = check_sh(&root, &["--examples"])
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
    assert_eq!(timings.len(), 1, "expected only the test timing: {stdout}");
    assert_eq!(timings[0]["kind"], "foreground");
    assert_eq!(timings[0]["name"], "tests (workspace)");
    assert_eq!(timings[0]["status"], "green");
    assert!(timings[0]["elapsed_s"].as_u64().unwrap() >= 1);
    });

        let queue_selection = scope.spawn(|| {
    let default_queue_prefix =
        "nextest run -p merge-queue-tests --test merge_queue --test test_for_paths --test worktree -j 6";
    let override_queue_prefix =
        "nextest run -p merge-queue-tests --test merge_queue --test test_for_paths --test worktree -j 4";
    let worktree_queue_prefix =
        "nextest run -p merge-queue-tests --test worktree -j 4";
    let mixed_queue_prefix =
        "nextest run -p merge-queue-tests --test merge_queue -j 6";
    let cargo_args_file = temp.path().join("cargo-args-queue-selection");
    let isolated = check_sh(&root, &["--fast"])
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
        .env("CARGO_TARGET_DIR", temp.path().join("target-isolated"))
        .env("WITCHY_GATE_QUEUE_INFRA", "1")
        .env_remove("WITCHY_QUEUE_INFRA_JOBS")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
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
            "tests (workspace)",
            "queue infrastructure (isolated)",
        ],
        "queue fixtures did not run alone after product work: {isolated_stdout}",
    );
    let isolated_args =
        fs::read_to_string(&cargo_args_file).expect("read isolated fake cargo arguments");
    assert!(
        isolated_args
            .lines()
            .any(|line| line.starts_with(default_queue_prefix)),
        "queue fixtures did not use the bounded hermetic width: {isolated_args}",
    );
    fs::write(&cargo_args_file, "").expect("clear fake cargo arguments");
    let queue_only = check_sh(&root, &["--fast"])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-queue-only"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_QUEUE_INFRA", "1")
        .env("WITCHY_QUEUE_INFRA_JOBS", "4")
        // This is the exact selector emitted for a queue-infrastructure-only
        // landing. All test binaries live in the standalone package, so root
        // Cargo must not be asked to resolve any of them.
        .env(
            "WITCHY_GATE_NEXTEST",
            "--test merge_queue --test test_for_paths --test worktree",
        )
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run queue-only gate with standalone fixture package");
    assert!(
        queue_only.status.success(),
        "queue-only fake gate failed: {}",
        String::from_utf8_lossy(&queue_only.stderr)
    );
    let queue_only_args =
        fs::read_to_string(&cargo_args_file).expect("read queue-only fake cargo arguments");
    assert!(
        !queue_only_args
            .lines()
            .any(|line| line.starts_with("nextest run --test")),
        "root Cargo was asked to resolve the standalone queue target: {queue_only_args}",
    );
    assert!(
        queue_only_args
            .lines()
            .any(|line| line.starts_with(override_queue_prefix)),
        "queue-only selection did not honor the configured bounded width: {queue_only_args}",
    );
    fs::write(&cargo_args_file, "").expect("clear fake cargo arguments");
    let worktree_only = check_sh(&root, &["--fast"])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-worktree-only"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_QUEUE_INFRA", "1")
        .env("WITCHY_QUEUE_INFRA_JOBS", "4")
        .env("WITCHY_GATE_NEXTEST", "--test worktree")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run worktree-only queue gate");
    assert!(
        worktree_only.status.success(),
        "worktree-only fake gate failed: {}",
        String::from_utf8_lossy(&worktree_only.stderr)
    );
    let worktree_only_args =
        fs::read_to_string(&cargo_args_file).expect("read worktree-only fake cargo arguments");
    assert!(
        worktree_only_args
            .lines()
            .any(|line| line.starts_with(worktree_queue_prefix)),
        "worktree-only selection expanded to unrelated queue binaries: {worktree_only_args}",
    );
    assert!(
        !worktree_only_args.lines().any(|line| {
            line.starts_with("nextest run -p merge-queue-tests")
                && (line.contains("--test merge_queue")
                    || line.contains("--test test_for_paths"))
        }),
        "worktree-only selection ran unrelated queue binaries: {worktree_only_args}",
    );
    fs::write(&cargo_args_file, "").expect("clear fake cargo arguments");
    // A diff touching the queue scripts AND a root integration test maps to a
    // mixed selector. Root Cargo cannot resolve the standalone queue binary,
    // so it must go to the isolated shard (turned on even without the
    // coordinator's flag) while the product target stays in the main run.
    let mixed = check_sh(&root, &["--fast"])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-mixed"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env_remove("WITCHY_GATE_QUEUE_INFRA")
        .env_remove("WITCHY_QUEUE_INFRA_JOBS")
        .env("WITCHY_GATE_NEXTEST", "--test merge_queue --test rfc0092")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run mixed queue/product gate");
    assert!(
        mixed.status.success(),
        "mixed queue/product fake gate failed: {}",
        String::from_utf8_lossy(&mixed.stderr)
    );
    let mixed_args = fs::read_to_string(&cargo_args_file).expect("read mixed fake cargo arguments");
    assert!(
        !mixed_args
            .lines()
            .any(|line| line.starts_with("nextest run") && line.contains("--test merge_queue")
                && !line.contains("-p merge-queue-tests")),
        "root Cargo was asked to resolve the standalone queue target: {mixed_args}",
    );
    assert!(
        mixed_args
            .lines()
            .any(|line| line.starts_with("nextest run") && line.contains("--test rfc0092")),
        "the product target left the main run: {mixed_args}",
    );
    assert!(
        mixed_args.lines().any(|line| line.starts_with(mixed_queue_prefix)),
        "the selected queue binary did not run alone in the isolated shard: {mixed_args}",
    );
    let invalid_width = check_sh(&root, &["--queue-infra"])
        .env("PATH", &path)
        .env("WITCHY_QUEUE_INFRA_JOBS", "0")
        .output()
        .expect("reject invalid queue fixture width");
    assert!(!invalid_width.status.success());
    assert!(
        String::from_utf8_lossy(&invalid_width.stderr)
            .contains("WITCHY_QUEUE_INFRA_JOBS must be a positive integer")
    );
    });

        let focused_selection = scope.spawn(|| {
            let cargo_args_file = temp.path().join("cargo-args-focused-selection");
            let focused = check_sh(&root, &["--fast"])
                .env("PATH", &path)
                .env("CARGO_TARGET_DIR", temp.path().join("target-focused"))
                .env("WITCHY_GATE_SCOPE", "all")
                .env(
                    "WITCHY_GATE_NEXTEST",
                    "-p witchy-types -p witchy --test typeck --bin witchy",
                )
                .env("WITCHY_GATE_NEXTEST_EXTRA", "-p witchy-caps --lib")
                .env(
                    "WITCHY_GATE_NEXTEST_EXPR",
                    "package(witchy-types) or package(witchy-caps) or test(/^example_tests::/)",
                )
                .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
                .env("WITCHY_FAILFAST_POLL", "0.05")
                .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
                .output()
                .expect("run fast gate with a focused nextest selection");
            assert!(
                focused.status.success(),
                "focused fake fast gate failed: {}",
                String::from_utf8_lossy(&focused.stderr),
            );
            let focused_args =
                fs::read_to_string(&cargo_args_file).expect("read focused cargo arguments");
            assert!(
                focused_args.lines().any(|line| {
                    line.contains("nextest run")
                        && line.contains("-p witchy-types")
                        && line.contains("package(witchy-types)")
                        && !line.contains("--workspace")
                }),
                "types-only serialized nextest still used unfiltered --workspace: {focused_args}",
            );
            assert!(
                focused_args.lines().any(|line| {
                    line.contains("nextest run")
                        && line.contains("-p witchy-caps --lib")
                        && line.contains("--profile gate-partition")
                        && line.contains("package(witchy-caps)")
                }),
                "secondary lib-test partition did not run with the shared filter: {focused_args}",
            );

            fs::write(&cargo_args_file, "").expect("clear fake cargo arguments");
            let docs = check_sh(&root, &[])
                .env("PATH", &path)
                .env("CARGO_TARGET_DIR", temp.path().join("target-docs-contracts"))
                .env("WITCHY_GATE_SCOPE", "docs")
                .env("WITCHY_GATE_NEXTEST", "-p repo-tests")
                .env("WITCHY_GATE_NEXTEST_EXPR", "package(repo-tests)")
                .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
                .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
                .output()
                .expect("run docs-only repository contracts");
            assert!(
                docs.status.success(),
                "docs-only repository contracts failed: {}",
                String::from_utf8_lossy(&docs.stderr),
            );
            let docs_args =
                fs::read_to_string(&cargo_args_file).expect("read docs-only cargo arguments");
            assert!(
                docs_args.lines().any(|line| {
                    line.starts_with("nextest run -p repo-tests --test repo_hygiene")
                }),
                "docs-only gate omitted repository contracts: {docs_args}",
            );
            assert!(
                !docs_args.lines().any(|line| line.contains("--workspace")),
                "docs-only repository gate compiled the workspace: {docs_args}",
            );

        });

        let package_selection = scope.spawn(|| {
    let cargo_args_file = temp.path().join("cargo-args-package-selection");
    let witchy_bin = temp.path().join("target-example-tests/debug/witchy");
    fs::create_dir_all(witchy_bin.parent().unwrap()).expect("create fake witchy dir");
    fs::write(&witchy_bin, "#!/bin/sh\nexit 0\n").expect("write fake witchy");
    fs::set_permissions(&witchy_bin, fs::Permissions::from_mode(0o755))
        .expect("chmod fake witchy");
    let example_tests = check_sh(&root, &[])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-example-tests"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_NEXTEST", "-p witchy")
        .env("WITCHY_GATE_NEXTEST_EXPR", "test(/^example_tests::/)")
        .env("WITCHY_GATE_SKIP_RUST_CLASS", "1")
        .env("WITCHY_GATE_SKIP_FMT", "1")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run serialized gate for an example_tests-only batch");
    let example_stdout = String::from_utf8_lossy(&example_tests.stdout);
    let example_stderr = String::from_utf8_lossy(&example_tests.stderr);
    assert!(
        example_tests.status.success(),
        "example_tests-only fake gate failed: {example_stderr}\n{example_stdout}",
    );
    assert!(
        example_stdout.contains("skipping rust-class")
            && example_stdout.contains("skipping witchy fmt"),
        "example_tests-only check.sh did not skip rust-class/fmt: {example_stdout}",
    );
    assert!(
        !example_stdout.contains("Rust-class paired correctness")
            && !example_stdout.contains("bench/rust-class/run.sh"),
        "example_tests-only check.sh still launched rust-class: {example_stdout}",
    );
    let example_args = fs::read_to_string(&cargo_args_file).expect("read example_tests cargo args");
    assert!(
        example_args.lines().any(|line| {
            line.contains("nextest run")
                && line.contains("-p witchy")
                && line.contains("example_tests")
                && !line.contains("--workspace")
        }),
        "example_tests-only serialized nextest still used unfiltered --workspace: {example_args}",
    );

    fs::write(&cargo_args_file, "").expect("clear fake cargo arguments");
    // Crate-only types: no witchy CLI in the target dir. Skip fmt must not
    // try to invoke or build it.
    let types_target = temp.path().join("target-types-only");
    fs::create_dir_all(&types_target).expect("create types-only target");
    let types_only = check_sh(&root, &[])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", &types_target)
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_NEXTEST", "-p witchy-types")
        .env("WITCHY_GATE_NEXTEST_EXPR", "package(witchy-types)")
        .env("WITCHY_GATE_SKIP_RUST_CLASS", "1")
        .env("WITCHY_GATE_SKIP_FMT", "1")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run serialized gate for a types-only batch");
    let types_out = String::from_utf8_lossy(&types_only.stdout);
    assert!(
        types_only.status.success(),
        "types-only fake gate failed: {}\n{types_out}",
        String::from_utf8_lossy(&types_only.stderr),
    );
    assert!(
        types_out.contains("skipping witchy fmt")
            && !types_out.contains("witchy fmt (std+examples)"),
        "types-only check.sh did not skip fmt: {types_out}",
    );

    fs::write(&cargo_args_file, "").expect("clear fake cargo arguments");
    let lower_only = check_sh(&root, &[])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-lower-only"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env(
            "WITCHY_GATE_NEXTEST",
            "-p witchy-lower --test codegen --test safewir_checked_source",
        )
        .env("WITCHY_GATE_NEXTEST_EXPR", "package(witchy-lower)")
        .env("WITCHY_GATE_CHECK_PACKAGES", "-p witchy-lower")
        .env("WITCHY_GATE_SKIP_RUST_CLASS", "1")
        .env("WITCHY_GATE_SKIP_COMPILE", "0")
        .env("WITCHY_GATE_SKIP_FMT", "1")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run serialized gate for a lower-only batch");
    assert!(
        lower_only.status.success(),
        "lower-only fake gate failed: {}\n{}",
        String::from_utf8_lossy(&lower_only.stderr),
        String::from_utf8_lossy(&lower_only.stdout),
    );
    let lower_args =
        fs::read_to_string(&cargo_args_file).expect("read lower-only cargo arguments");
    for line in lower_args.lines().filter(|line| {
        line.starts_with("nextest run") || line.starts_with("check ")
    }) {
        assert!(
            !line.contains("witchy-wir/native")
                && !line.contains("witchy-lower/native-test-runtime"),
            "lower-only gate enabled the native runtime graph: {line}",
        );
    }
    assert!(
        lower_args.lines().any(|line| {
            line.starts_with("nextest run")
                && line.contains("-p witchy-lower")
                && line.contains("--test codegen")
                && line.contains("--test safewir_checked_source")
        }),
        "lower-only gate dropped its pure compiler tests: {lower_args}",
    );
    assert!(
        lower_args
            .lines()
            .filter(|line| line.starts_with("nextest run"))
            .all(|line| {
                line.ends_with(&format!(
                    "target={}-pure",
                    temp.path().join("target-lower-only").display()
                ))
            }),
        "lower-only gate shared the native graph's target: {lower_args}",
    );

    fs::write(&cargo_args_file, "").expect("clear fake cargo arguments");
    let pure_wir = check_sh(&root, &[])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target-pure-wir"))
        .env("WITCHY_GATE_SCOPE", "all")
        .env(
            "WITCHY_GATE_NEXTEST",
            "-p witchy-wir --test unit --test structural_contracts --no-default-features",
        )
        .env("WITCHY_GATE_NEXTEST_EXPR", "package(witchy-wir)")
        .env("WITCHY_GATE_SKIP_RUST_CLASS", "1")
        .env("WITCHY_GATE_SKIP_COMPILE", "1")
        .env("WITCHY_GATE_SKIP_FMT", "1")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run serialized pure WIR gate");
    assert!(
        pure_wir.status.success(),
        "pure WIR fake gate failed: {}\n{}",
        String::from_utf8_lossy(&pure_wir.stderr),
        String::from_utf8_lossy(&pure_wir.stdout),
    );
    let pure_wir_args =
        fs::read_to_string(&cargo_args_file).expect("read pure WIR cargo arguments");
    assert!(
        pure_wir_args.lines().any(|line| {
            line.starts_with("nextest run")
                && line.contains("-p witchy-wir")
                && line.contains("--test unit")
                && line.contains("--no-default-features")
                && !line.contains("--features witchy-wir/native")
        }),
        "pure WIR gate re-enabled the native graph: {pure_wir_args}",
    );
    assert!(
        pure_wir_args.lines().filter(|line| line.starts_with("nextest run")).all(|line| {
            line.ends_with(&format!("target={}-pure", temp.path().join("target-pure-wir").display()))
        }),
        "pure WIR gate shared the native graph's target: {pure_wir_args}",
    );

    fs::write(&cargo_args_file, "").expect("clear fake cargo arguments");
    let interp_target = temp.path().join("target-interp-check");
    fs::create_dir_all(interp_target.join("debug")).expect("create interp fake target");
    fs::write(interp_target.join("debug/witchy"), "#!/bin/sh\nexit 0\n").expect("write interp witchy");
    fs::set_permissions(interp_target.join("debug/witchy"), fs::Permissions::from_mode(0o755))
        .expect("chmod interp witchy");
    let interp = check_sh(&root, &[])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", &interp_target)
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_NEXTEST", "-p witchy-interp -p witchy-caps -p witchy")
        .env(
            "WITCHY_GATE_NEXTEST_EXPR",
            "package(witchy-interp) or package(witchy-caps) or test(/^example_tests::/)",
        )
        .env("WITCHY_GATE_SKIP_RUST_CLASS", "1")
        .env("WITCHY_GATE_SKIP_FMT", "1")
        .env("FAKE_CARGO_ARGS_FILE", &cargo_args_file)
        .env("WITCHY_FAILFAST_POLL", "0.05")
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run serialized gate for interp+example_tests");
    let interp_out = String::from_utf8_lossy(&interp.stdout);
    assert!(
        interp.status.success(),
        "interp+example_tests fake gate failed: {}\n{interp_out}",
        String::from_utf8_lossy(&interp.stderr),
    );
    assert!(
        interp_out.contains("skipping rust-class")
            && interp_out.contains("skipping witchy fmt"),
        "interp+example_tests did not skip rust-class/fmt: {interp_out}",
    );
    let interp_args = fs::read_to_string(&cargo_args_file).expect("read interp cargo args");
    assert!(
        interp_args.lines().all(|line| !line.starts_with("check ")),
        "interp+example_tests launched redundant cargo check: {interp_args}",
    );
    });

        let failure_timing = scope.spawn(|| {
            let failed = check_sh(&root, &["--fast"])
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        bin.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .env("CARGO_TARGET_DIR", temp.path().join("target-red"))
                .env("WITCHY_FAILFAST_POLL", "0.05")
                .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
                .env("FAKE_CARGO_FAIL_NEXTEST", "1")
                .output()
                .expect("run red fast gate with fake tools");
            assert_eq!(failed.status.code(), Some(7));
            let stdout = String::from_utf8(failed.stdout).expect("red check output is utf8");
            let timing = stdout
                .lines()
                .find_map(|line| line.strip_prefix("WITCHY_TIMING "))
                .map(|json| {
                    serde_json::from_str::<serde_json::Value>(json).expect("red timing is JSON")
                })
                .expect("red foreground timing was emitted");
            assert_eq!(timing["kind"], "foreground");
            assert_eq!(timing["name"], "tests (workspace)");
            assert_eq!(timing["status"], "red");
        });

        timing_and_strip.join().expect("timing and strip checks");
        queue_selection.join().expect("queue selection checks");
        focused_selection.join().expect("focused selection checks");
        package_selection.join().expect("package selection checks");
        failure_timing.join().expect("failure timing checks");
    });
}

/// Nextest owns compilation in the full merge gate. Stage order and timing
/// records are pinned so observability consumers keep parsing, and the removed
/// cargo-check graph cannot silently return.
#[test]
fn full_gate_compiles_once_through_nextest() {
    let root = workspace_root();
    let temp = TempDir::new();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).expect("create fake tool directory");

    let tool = bin.join("cargo");
    fs::write(
        &tool,
        "#!/bin/sh\n\
         if [ \"$1\" = check ]; then\n\
           echo 'redundant cargo check invoked' >&2\n\
           exit 91\n\
         fi\n\
         if [ \"$1\" = nextest ] && [ \"$2\" = run ]; then\n\
           if [ -n \"${FAKE_NEXTEST_PID_FILE:-}\" ]; then printf '%s\\n' \"$$\" >\"$FAKE_NEXTEST_PID_FILE\"; fi\n\
           exec sleep \"${FAKE_CARGO_NEXTEST_SECS:-0.25}\"\n\
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

    let green = check_sh(&root, &[])
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", &target)
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
            "Rust-class paired correctness",
        ],
        "full-gate stage/timing order changed: {stdout}",
    );
}

/// Change 3 (culprit eviction): an unrelated red batch whose failure names a
/// file only one member touches evicts THAT member to a solo gate and lands
/// the remaining members as one green batch — two follow-up gates instead of
/// one per member.
#[test]
fn a_red_costs_the_front_entry_its_queue_head() {
    // `--front` expedites ONE urgent change. A batch red deliberately RETAINS
    // every member's queue file, so without demotion a front-stamped member
    // re-leads the next cycle and every cycle after it, starving every ready
    // candidate behind it (BUG-633: 3h41m, zero merges, 14 ready).
    let fixture = QueueFixture::stack(&[]);
    let root = fixture.root.clone();
    for (branch, file) in [("urgent", "urgent.txt"), ("notes", "notes.txt")] {
        run_git(&root, &["switch", "-c", branch, "master"]);
        fs::write(root.join(file), format!("{branch}\n")).expect("write branch file");
        run_git(&root, &["add", file]);
        run_git(&root, &["commit", "-m", &format!("add {file}")]);
        run_git(&root, &["switch", "master"]);
    }
    fixture.mq_ok(&["submit", "--front", "urgent"], "true");
    fixture.mq_ok(&["submit", "notes"], "true");

    let queued_head = |fixture: &QueueFixture| -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(fixture.state.join("queue"))
            .expect("read queue dir")
            .map(|entry| entry.expect("queue entry").file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".json"))
            .collect();
        names.sort();
        names
    };
    assert!(
        queued_head(&fixture)[0].starts_with("00front-"),
        "--front must place the change at the queue head: {:?}",
        queued_head(&fixture),
    );

    // A red that indicts no single member: the batch is retained wholesale.
    fixture.mq_ok(&["run", "--once"], "exit 1");

    // The harness gate reds unconditionally and `run --once` keeps draining, so
    // every member is re-gated individually and consumed by the end of the run.
    // The durable evidence of demotion is therefore the journal, not the final
    // directory listing: the batch red must be followed by a `demoted` event
    // whose new queue entry no longer carries the front prefix.
    let journal = fixture.journal();
    let trace: Vec<&str> = journal
        .iter()
        .map(|event| event["event"].as_str().unwrap_or("?"))
        .collect();
    let batch_red_at = trace
        .iter()
        .position(|event| *event == "batch_red")
        .unwrap_or_else(|| panic!("expected a red batch: {trace:?}"));
    let demoted_at = trace
        .iter()
        .position(|event| *event == "demoted")
        .unwrap_or_else(|| panic!("a red must cost the front slot, but nothing was demoted: {trace:?}"));
    assert!(
        demoted_at > batch_red_at,
        "demotion must be a consequence of the red, not precede it: {trace:?}",
    );
    let demoted = &journal[demoted_at];
    assert_eq!(demoted["branch"], "urgent", "the front-stamped member is the one demoted");
    let entry = demoted["entry"].as_str().expect("demotion records its new queue entry");
    assert!(
        !entry.starts_with("00front-") && entry.contains("urgent"),
        "demotion must move the change to an ordinary FIFO stamp, not evict it: {entry}",
    );
}

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
    let gate = "test ! -f tests/foo_widget.rs || { printf 'FAIL [   0.42s] ( 1/3) witchy::foo_widget foo_widget::renders_the_widget\\n'; exit 1; }";
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
    let changes: Vec<serde_json::Value> = fs::read_dir(fixture.state.join("changes"))
        .expect("read change records")
        .map(|entry| entry.expect("read change entry"))
        .map(|entry| {
            serde_json::from_slice(&fs::read(entry.path()).expect("read change record"))
                .expect("change record is JSON")
        })
        .collect();
    let culprit = changes
        .iter()
        .find(|change| change["branch"] == "widget-fix")
        .expect("culprit change record exists");
    assert!(
        culprit["preflight"]
            .as_str()
            .is_some_and(|expr| expr.contains("foo_widget::renders_the_widget")),
        "culprit did not retain its focused failure replay: {culprit}",
    );
    for innocent in ["notes-a", "notes-b"] {
        let change = changes
            .iter()
            .find(|change| change["branch"] == innocent)
            .unwrap_or_else(|| panic!("innocent change record exists: {innocent}"));
        assert!(
            change.get("preflight").is_none(),
            "batch failure was attached to innocent {innocent}: {change}",
        );
    }
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
    let cargo_args = fixture._temp.path().join("regen-cargo-args");
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
            "#!/bin/sh\nprintf '%s' \"${{CARGO_INCREMENTAL-unset}}\" >{}\nprintf '%s\\n' \"$*\" >{}\nexit 0\n",
            incremental.display(),
            cargo_args.display(),
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
        "unset",
        "preparation did not match the serialized gate incremental setting",
    );
    assert_eq!(
        fs::read_to_string(&cargo_args).expect("read preparation Cargo arguments"),
        "build -p witchy-snapshot-tools --bin rfc0087-census\n",
        "snapshot preparation did not select the isolated generator package",
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

#[test]
fn profile_only_manifest_change_skips_snapshot_generator_build() {
    let fixture = QueueFixture::stack(&[]);
    let root = fixture.root.clone();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[profile.dev]\nopt-level = 1\n",
    )
    .expect("write base manifest");
    run_git(&root, &["add", "Cargo.toml"]);
    run_git(&root, &["commit", "-m", "add manifest"]);
    run_git(&root, &["switch", "-c", "profile-only", "master"]);
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[profile.dev]\nopt-level = 0\n",
    )
    .expect("change only the profile");
    run_git(&root, &["add", "Cargo.toml"]);
    run_git(&root, &["commit", "-m", "change profile"]);
    run_git(&root, &["switch", "master"]);

    let marker = fixture._temp.path().join("snapshot-build-ran");
    let bin = fixture._temp.path().join("profile-only-bin");
    fs::create_dir(&bin).expect("create fake cargo dir");
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    )
    .expect("write fake cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).expect("chmod fake cargo");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    fixture.mq_ok(&["submit", "profile-only"], "true");
    let output = fixture
        .mq_command(&["run", "--once"], "true")
        .env("PATH", &path)
        .output()
        .expect("run profile-only candidate");
    assert!(
        output.status.success(),
        "profile-only gate failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(!marker.exists(), "profile-only change built the snapshot generator");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(
            "Cargo profile-only change cannot stale snapshots"
        ),
        "profile-only skip was not reported: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    // A semantic manifest edit must retain the conservative generator path.
    run_git(&root, &["switch", "-c", "manifest-semantic", "master"]);
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"0.2.0\"\n\n[profile.dev]\nopt-level = 0\n",
    )
    .expect("change package semantics");
    run_git(&root, &["add", "Cargo.toml"]);
    run_git(&root, &["commit", "-m", "change manifest semantics"]);
    run_git(&root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "manifest-semantic"], "true");
    fixture
        .mq_command(&["run", "--once"], "true")
        .env("PATH", &path)
        .output()
        .expect("run semantic manifest candidate");
    assert!(marker.exists(), "semantic manifest change skipped the snapshot generator");
}

/// A gate starved by another cargo's package-cache lock produced a bare
/// `timeout` and was re-diagnosed as a bad candidate: measured over 2,390
/// gate logs, 93 runs show the signature and 49 of them were recorded
/// `timeout` or `red`. The coordinator now names it in the attempt's reason.
#[test]
fn a_gate_starved_by_the_cargo_lock_is_named_in_the_timeout_reason() {
    fn note(log_body: &str) -> String {
        let temp = TempDir::new();
        let log = temp.path().join("gate.log");
        fs::write(&log, log_body).expect("write gate log fixture");
        let output = Command::new("bash")
            .args([
                "-c",
                r#"eval "$(sed -n '/^cargo_lock_starvation_note()/,/^}/p' "$1")"; cargo_lock_starvation_note "$2""#,
                "starvation-test",
            ])
            .arg(workspace_root().join("scripts/merge-queue.sh"))
            .arg(&log)
            .output()
            .expect("run the starvation probe");
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).expect("note is utf8")
    }

    let starved = format!(
        "{}\n    Blocking waiting for file lock on build directory\nTerminated: 15\n",
        "    Blocking waiting for file lock on package cache\n".repeat(4),
    );
    let named = note(&starved);
    assert!(named.contains("cargo lock starvation"), "{named}");
    assert!(named.contains("5 lock waits"), "{named}");

    // A stage that waited once and then ran is ordinary: hundreds of real
    // output lines follow, so the run is not starved and gets no note.
    let ran = format!(
        "    Blocking waiting for file lock on package cache\n{}",
        (0..200).map(|i| format!("        PASS [   0.0{i:02}s] witchy::misc case_{i}\n")).collect::<String>(),
    );
    assert_eq!(note(&ran), "", "a stage that ran must not be called starved");

    // No lock contention at all: nothing to say.
    assert_eq!(note("     Summary [ 12.0s] 10 tests run: 10 passed\n"), "");
}
