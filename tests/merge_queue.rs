use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ProcessGroupGuard(i32);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "witchy-merge-queue-daemon-{}-{nonce}",
            std::process::id(),
        ));
        fs::create_dir(&path).expect("create temporary state directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{}", self.0)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn process_group(pid: i32) -> i32 {
    let output = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    assert!(output.status.success(), "ps failed for pid {pid}");
    String::from_utf8(output.stdout)
        .expect("ps output is utf8")
        .trim()
        .parse()
        .expect("parse process group")
}

fn process_is_alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed:\nstdout: {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout)
        .expect("git output is utf8")
        .trim()
        .to_owned()
}

#[test]
fn coordinator_skips_only_fully_patch_equivalent_submissions() {
    let source_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let repo = temp.path().join("repo");
    let state = temp.path().join("state");
    let gate_worktree = temp.path().join("gate-worktree");
    let landed_worktree = temp.path().join("landed-worktree");
    let gate_marker = temp.path().join("gate-ran");
    let fake_bin = temp.path().join("bin");
    fs::create_dir_all(repo.join("scripts")).expect("create temporary repository");
    fs::create_dir(&fake_bin).expect("create fake bin directory");
    fs::copy(
        source_root.join("scripts/merge-queue.sh"),
        repo.join("scripts/merge-queue.sh"),
    )
    .expect("copy merge queue script");
    fs::set_permissions(
        repo.join("scripts/merge-queue.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .expect("chmod merge queue script");

    git(&repo, &["init", "-b", "master"]);
    git(&repo, &["config", "user.email", "merge-queue-test@witchy.invalid"]);
    git(&repo, &["config", "user.name", "Merge Queue Test"]);
    fs::write(repo.join("base"), "base\n").expect("write base file");
    git(&repo, &["add", "base"]);
    git(&repo, &["commit", "-m", "base"]);

    git(&repo, &["checkout", "-b", "landed-original"]);
    fs::write(repo.join("represented"), "represented\n").expect("write represented patch");
    git(&repo, &["add", "represented"]);
    git(&repo, &["commit", "-m", "represented patch"]);
    let original_sha = git(&repo, &["rev-parse", "HEAD"]);

    git(&repo, &["checkout", "master"]);
    fs::write(repo.join("master-only"), "unrelated\n").expect("write unrelated master patch");
    git(&repo, &["add", "master-only"]);
    git(&repo, &["commit", "-m", "unrelated master patch"]);
    git(&repo, &["cherry-pick", &original_sha]);
    assert_ne!(
        git(&repo, &["rev-parse", "master"]),
        original_sha,
        "fixture must model a rebased/cherry-picked landing",
    );
    let ancestor = Command::new("git")
        .current_dir(&repo)
        .args(["merge-base", "--is-ancestor", &original_sha, "master"])
        .status()
        .expect("check original ancestry");
    assert!(!ancestor.success(), "original SHA unexpectedly became an ancestor");
    assert!(
        git(&repo, &["cherry", "master", "landed-original"])
            .lines()
            .all(|line| line.starts_with('-')),
        "the original branch must be fully patch-equivalent to master",
    );
    git(
        &repo,
        &[
            "worktree",
            "add",
            landed_worktree.to_str().unwrap(),
            "landed-original",
        ],
    );

    git(&repo, &["checkout", "-b", "partially-new", "landed-original"]);
    fs::write(repo.join("new-patch"), "new\n").expect("write new patch");
    git(&repo, &["add", "new-patch"]);
    git(&repo, &["commit", "-m", "new patch"]);
    assert!(
        git(&repo, &["cherry", "master", "partially-new"])
            .lines()
            .any(|line| line.starts_with('+')),
        "multi-commit fixture must retain one unrepresented patch",
    );
    git(&repo, &["checkout", "master"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            gate_worktree.to_str().unwrap(),
            "master",
        ],
    );

    let fake_sleep = fake_bin.join("sleep");
    fs::write(&fake_sleep, "#!/bin/sh\nexec /bin/sleep 0.05\n").expect("write yielding sleep");
    fs::set_permissions(&fake_sleep, fs::Permissions::from_mode(0o755))
        .expect("chmod yielding sleep");
    let gate_command = temp.path().join("gate-command");
    fs::write(
        &gate_command,
        format!("#!/bin/sh\nprintf ran >{}\n", gate_marker.display()),
    )
    .expect("write gate command");
    fs::set_permissions(&gate_command, fs::Permissions::from_mode(0o755))
        .expect("chmod gate command");
    let path = format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap_or_default());
    let queue = repo.join("scripts/merge-queue.sh");
    for branch in ["landed-original", "partially-new"] {
        let output = Command::new(&queue)
            .args(["submit", branch])
            .env("MERGE_QUEUE_STATE_DIR", &state)
            .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
            .output()
            .expect("submit fixture branch");
        assert!(
            output.status.success(),
            "submit {branch} failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
    let landed_queue_file = fs::read_dir(state.join("queue"))
        .expect("read submitted queue")
        .map(|entry| entry.expect("read queue entry").path())
        .find(|path| {
            path.extension().is_some_and(|extension| extension == "json")
                && fs::read_to_string(path)
                    .expect("read queue file")
                    .contains("landed-original")
        })
        .expect("find landed queue file");
    fs::write(format!("{}.nobatch", landed_queue_file.display()), "")
        .expect("write no-batch sidecar");
    fs::write(format!("{}.batch-limit", landed_queue_file.display()), "")
        .expect("write batch-limit sidecar");

    let output = Command::new(&queue)
        .args(["run", "--once"])
        .env("PATH", path)
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", &gate_command)
        .output()
        .expect("run isolated coordinator");
    assert!(
        output.status.success(),
        "coordinator failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(gate_marker.exists(), "the partially new branch was incorrectly skipped");
    assert!(
        !landed_worktree.exists(),
        "already-merged clean worktree was not swept",
    );

    let events: Vec<serde_json::Value> = fs::read_to_string(state.join("journal.jsonl"))
        .expect("read isolated journal")
        .lines()
        .map(|line| serde_json::from_str(line).expect("journal line is JSON"))
        .collect();
    assert!(events.iter().any(|event| {
        event["event"] == "already_merged" && event["branch"] == "landed-original"
    }));
    assert!(events.iter().any(|event| {
        event["event"] == "validated" && event["branch"] == "partially-new"
    }));
    assert_eq!(
        fs::read_dir(state.join("queue"))
            .expect("read drained queue")
            .count(),
        0,
        "coordinator did not consume both terminal submissions",
    );
}

#[test]
fn daemon_enters_an_independent_process_group() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let gate_worktree = temp.path().join("unused-gate-worktree");

    let output = Command::new(root.join("scripts/merge-queue.sh"))
        .arg("daemon")
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", "true")
        .env(
            "MERGE_QUEUE_COORDINATOR_SCRIPT",
            root.join("scripts/merge-queue.sh"),
        )
        .output()
        .expect("start isolated coordinator daemon");
    assert!(
        output.status.success(),
        "daemon failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pid_path = state.join("coordinator.pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Ok(text) = fs::read_to_string(&pid_path) {
            break text.trim().parse::<i32>().expect("parse coordinator pid");
        }
        assert!(Instant::now() < deadline, "coordinator pid was not written");
        thread::sleep(Duration::from_millis(25));
    };
    let guard = ProcessGroupGuard(pid);

    assert!(
        process_is_alive(pid),
        "coordinator {pid} did not survive daemon return"
    );
    assert_eq!(
        process_group(pid),
        pid,
        "the coordinator must lead a new process group so launcher cleanup cannot kill it"
    );
    assert_ne!(
        process_group(std::process::id() as i32),
        pid,
        "the coordinator remained in the test runner's process group"
    );

    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_is_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !process_is_alive(pid),
        "coordinator {pid} ignored process-group termination"
    );
}

#[test]
fn concurrent_daemon_starts_create_exactly_one_coordinator() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let gate_worktree = temp.path().join("unused-gate-worktree");

    let mut starters = Vec::new();
    for _ in 0..8 {
        starters.push(
            Command::new(root.join("scripts/merge-queue.sh"))
                .arg("daemon")
                .env("MERGE_QUEUE_STATE_DIR", &state)
                .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
                .env("MERGE_QUEUE_GATE_CMD", "true")
                .env(
                    "MERGE_QUEUE_COORDINATOR_SCRIPT",
                    root.join("scripts/merge-queue.sh"),
                )
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("race coordinator daemon start"),
        );
    }
    for mut starter in starters {
        assert!(starter.wait().expect("wait for daemon starter").success());
    }

    let pid: i32 = fs::read_to_string(state.join("coordinator.pid"))
        .expect("read winning coordinator pid")
        .trim()
        .parse()
        .expect("parse winning coordinator pid");
    let guard = ProcessGroupGuard(pid);
    assert!(process_is_alive(pid), "winning coordinator is not alive");
    assert_eq!(
        fs::read_to_string(state.join("coordinator.lock/pid"))
            .expect("read singleton lock owner")
            .trim(),
        pid.to_string(),
    );
    let log = fs::read_to_string(state.join("coordinator.log"))
        .expect("read concurrent-start coordinator log");
    assert_eq!(
        log.matches("coordinator up (pid ").count(),
        1,
        "more than one persistent loop started:\n{log}",
    );

    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(5);
    while process_is_alive(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(!process_is_alive(pid), "winning coordinator ignored termination");
}

#[test]
fn doctor_treats_denied_process_inspection_as_advisory() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&state).expect("create isolated coordinator state");
    fs::create_dir(&bin).expect("create fake tool directory");
    fs::write(
        state.join("coordinator.pid"),
        format!("{}\n", std::process::id()),
    )
    .expect("write live coordinator pid fixture");
    let ps = bin.join("ps");
    fs::write(&ps, "#!/bin/sh\nexit 126\n").expect("write denied ps fixture");
    fs::set_permissions(&ps, fs::Permissions::from_mode(0o755)).expect("chmod fake ps");

    let output = Command::new(root.join("scripts/merge-queue.sh"))
        .arg("doctor")
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()),
        )
        .output()
        .expect("run doctor with denied process inspection");
    assert!(
        output.status.success(),
        "doctor made advisory ps failure fatal: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("coordinator : RUNNING"),
        "doctor lost coordinator health output",
    );
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
        "    Finished `test` profile [unoptimized] target(s) in 20s\n\
         Starting 100 tests across 10 binaries\n\
         Summary [ 30.000s] 100 tests run: 100 passed\n\
         [1] tests (workspace) took 70s\n\
         [2] witchy fmt (std+examples) took 2s\n\
         WITCHY_TIMING {\"schema\":1,\"kind\":\"foreground\",\"step\":1,\"name\":\"tests (workspace)\",\"status\":\"green\",\"started_epoch\":10,\"finished_epoch\":81,\"elapsed_s\":71,\"gate_elapsed_s\":71}\n\
         WITCHY_TIMING {\"schema\":1,\"kind\":\"foreground\",\"step\":2,\"name\":\"witchy fmt (std+examples)\",\"status\":\"green\",\"started_epoch\":81,\"finished_epoch\":84,\"elapsed_s\":3,\"gate_elapsed_s\":74}\n\
         WITCHY_TIMING {\"schema\":1,\"kind\":\"background\",\"step\":3,\"name\":\"clippy (deny warnings)\",\"status\":\"green\",\"started_epoch\":10,\"finished_epoch\":60,\"elapsed_s\":50,\"gate_elapsed_s\":50}\n",
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
            "{{\"ts\":\"2026-07-16T00:01:40Z\",\"event\":\"merged\",\"branch\":\"a\",\"elapsed_s\":\"90\",\"batch\":\"2\",\"log\":{green:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:01:40Z\",\"event\":\"merged\",\"branch\":\"b\",\"elapsed_s\":\"90\",\"batch\":\"2\",\"log\":{green:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:03:20Z\",\"event\":\"submitted\",\"branch\":\"c\"}}\n",
            "{{\"ts\":\"2026-07-16T00:04:20Z\",\"event\":\"timeout\",\"branch\":\"c\",\"elapsed_s\":\"60\",\"log\":{timeout:?}}}\n",
            "{{\"ts\":\"2026-07-16T00:05:00Z\",\"event\":\"submitted\",\"branch\":\"d\"}}\n",
            "{{\"ts\":\"2026-07-16T00:05:40Z\",\"event\":\"red\",\"branch\":\"d\",\"elapsed_s\":\"40\",\"log\":{red:?}}}\n"
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
    assert_eq!(report["gate_s"]["p50"], 60);
    assert_eq!(report["gate_s"]["p90"], 90);
    assert_eq!(report["phases_s"]["compile"]["p50"], 20);
    assert_eq!(report["phases_s"]["discovery_estimate"]["p50"], 21);
    assert_eq!(report["phases_s"]["execution"]["p50"], 30.0);
    assert_eq!(report["phases_s"]["test_stage"]["p50"], 71);
    assert_eq!(report["phases_s"]["auxiliary"]["p50"], 2);
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
    fs::write(&git, "#!/bin/sh\nexit 0\n").expect("write fake git");
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
    let output = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--fast")
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", temp.path().join("target"))
        .env("WITCHY_STAGE_HEARTBEAT_INTERVAL", "0")
        .output()
        .expect("run fast gate with fake tools");
    assert!(
        output.status.success(),
        "fake fast gate failed: {}",
        String::from_utf8_lossy(&output.stderr)
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
    assert_eq!(timings[1]["name"], "clippy (deny warnings)");
    assert_eq!(timings[1]["status"], "green");
    assert!(
        timings[1]["elapsed_s"].as_u64().unwrap() <= 2,
        "background timing included time spent waiting for foreground collection: {stdout}",
    );

    let clippy_pid_file = temp.path().join("red-clippy.pid");
    let failed = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--fast")
        .env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default()))
        .env("CARGO_TARGET_DIR", temp.path().join("target-red"))
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
