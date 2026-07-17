use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ProcessGroupGuard(i32);

struct TempDir(PathBuf);

static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

impl TempDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "witchy-merge-queue-daemon-{}-{nonce}-{}",
            std::process::id(),
            NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed),
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
        let status = Command::new("kill")
            .args(["-TERM", "--", &format!("-{}", self.0)])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if !status.is_ok_and(|status| status.success()) {
            let _ = Command::new("kill")
                .args(["-TERM", &self.0.to_string()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
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
    let state_paths = source_root.join("scripts/state-paths.sh");
    if state_paths.exists() {
        fs::copy(state_paths, repo.join("scripts/state-paths.sh"))
            .expect("copy merge queue state path helper");
    }
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
    let real_jq = Command::new("sh")
        .args(["-c", "command -v jq"])
        .output()
        .expect("locate real jq");
    assert!(real_jq.status.success(), "jq is required by the queue harness");
    let real_jq = String::from_utf8(real_jq.stdout)
        .expect("jq path is utf8")
        .trim()
        .to_owned();
    let fake_jq = fake_bin.join("jq");
    fs::write(
        &fake_jq,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" --arg event validated \"*)\n    [ -d \"$MERGE_QUEUE_STATE_DIR/gate.lock\" ] || exit 97\n    ;;\nesac\nexec {real_jq:?} \"$@\"\n",
        ),
    )
    .expect("write lock-asserting jq wrapper");
    fs::set_permissions(&fake_jq, fs::Permissions::from_mode(0o755))
        .expect("chmod jq wrapper");
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

    // Hold the gate lock with this live test process long enough to make lock
    // wait distinguishable from the actual (instant) gate in whole seconds.
    let held_lock = state.join("gate.lock");
    fs::create_dir(&held_lock).expect("create externally held gate lock");
    fs::write(held_lock.join("pid"), format!("{}\n", std::process::id()))
        .expect("write live lock owner");
    fs::write(held_lock.join("what"), "focused external check\n")
        .expect("write lock description");
    let prepared_worktree = gate_worktree.clone();
    let lock_releaser = thread::spawn(move || {
        // Git worktree checkout/rebase can take several seconds on a loaded
        // developer machine. Keep the lock held until preparation is visible;
        // the assertion still fails if preparation actually waits on the lock.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !prepared_worktree.join("new-patch").exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        let prepared_while_lock_held = prepared_worktree.join("new-patch").exists();
        thread::sleep(Duration::from_millis(1_100));
        fs::remove_dir_all(held_lock).expect("release externally held gate lock");
        prepared_while_lock_held
    });

    let output = Command::new(&queue)
        .args(["run", "--once"])
        .env("PATH", path)
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", &gate_command)
        .output()
        .expect("run isolated coordinator");
    let prepared_outside_lock = lock_releaser.join().expect("join gate lock releaser");
    assert!(
        output.status.success(),
        "coordinator failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(gate_marker.exists(), "the partially new branch was incorrectly skipped");
    assert!(
        prepared_outside_lock,
        "coordinator did not prepare the gate worktree while an external gate lock was held",
    );
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
    let validated = events
        .iter()
        .find(|event| event["event"] == "validated" && event["branch"] == "partially-new")
        .expect("find validated attempt event");
    assert_eq!(validated["attempt_timing_schema"], "1");
    assert!(
        validated["lock_wait_s"]
            .as_str()
            .expect("lock wait is journaled")
            .parse::<u64>()
            .expect("lock wait is numeric")
            >= 1,
        "external lock wait was not separated from the gate: {validated}",
    );
    assert_eq!(validated["elapsed_s"], validated["gate_elapsed_s"]);
    let phase_total: u64 = [
        "prepare_elapsed_s",
        "lock_wait_s",
        "preflight_elapsed_s",
        "gate_elapsed_s",
        "landing_elapsed_s",
    ]
    .iter()
    .map(|field| {
        validated[*field]
            .as_str()
            .unwrap_or_else(|| panic!("missing {field} in {validated}"))
            .parse::<u64>()
            .unwrap_or_else(|_| panic!("non-numeric {field} in {validated}"))
    })
    .sum();
    assert_eq!(
        phase_total,
        validated["attempt_elapsed_s"]
            .as_str()
            .expect("attempt total is journaled")
            .parse::<u64>()
            .expect("attempt total is numeric"),
        "attempt phases do not account for end-to-end coordinator time",
    );
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

    // Keep the daemon command's launcher alive in a dedicated process group so
    // this test can simulate the tool host reaping its whole group. Querying
    // PGIDs with `ps` is denied in the same sandbox where the coordinator gate
    // runs, while survival after group termination tests the actual invariant.
    let mut launcher = Command::new("bash")
        .args([
            "-c",
            r#""$1" daemon || exit $?; sleep 30"#,
            "merge-queue-daemon-launcher",
        ])
        .arg(root.join("scripts/merge-queue.sh"))
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", "true")
        .env(
            "MERGE_QUEUE_COORDINATOR_SCRIPT",
            root.join("scripts/merge-queue.sh"),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start isolated coordinator daemon launcher");
    let launcher_group = launcher.id() as i32;

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
    let killed = Command::new("kill")
        .args(["-TERM", "--", &format!("-{launcher_group}")])
        .status()
        .expect("terminate daemon launcher process group");
    assert!(killed.success(), "could not terminate daemon launcher group");
    let _ = launcher.wait();
    thread::sleep(Duration::from_millis(100));
    assert!(
        process_is_alive(pid),
        "coordinator {pid} remained in the launcher's process group"
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
