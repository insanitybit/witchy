use std::collections::BTreeSet;
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

fn process_group_is_alive(pgid: i32) -> bool {
    Command::new("kill")
        .args(["-0", "--", &format!("-{pgid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn process_group(pid: i32) -> i32 {
    let output = Command::new("perl")
        .args(["-e", "print getpgrp($ARGV[0])"])
        .arg(pid.to_string())
        .output()
        .expect("query process group with Perl");
    assert!(
        output.status.success(),
        "could not query process group for {pid}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout)
        .expect("process group output is utf8")
        .parse()
        .expect("process group is numeric")
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
    let partially_new_sha = git(&repo, &["rev-parse", "HEAD"]);
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
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate real git");
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout)
        .expect("git path is utf8")
        .trim()
        .to_owned();
    let fake_git = fake_bin.join("git");
    fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" checkout --detach --quiet {partially_new_sha} \"*|*\" checkout --detach --quiet refs/heads/partially-new \"*) exit 74 ;;\nesac\nexec {real_git:?} \"$@\"\n",
        ),
    )
    .expect("write old-candidate-denying git wrapper");
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755))
        .expect("chmod old-candidate-denying git wrapper");
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
    let landed_worktree_during_prepare = landed_worktree.clone();
    let lock_wait_marker = state.join("lock-wait-started");
    let lock_wait_marker_for_thread = lock_wait_marker.clone();
    let lock_releaser = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !prepared_worktree.join("new-patch").exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        let prepared_while_lock_held = prepared_worktree.join("new-patch").exists();
        let duplicate_sweep_was_deferred = landed_worktree_during_prepare.exists();
        while !lock_wait_marker_for_thread.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        let coordinator_waited_for_lock = lock_wait_marker_for_thread.exists();
        // The journal uses whole seconds, so hold one complete timing bucket
        // after the explicit lock-wait handshake.
        thread::sleep(Duration::from_millis(1_100));
        fs::remove_dir_all(held_lock).expect("release externally held gate lock");
        (
            prepared_while_lock_held,
            duplicate_sweep_was_deferred,
            coordinator_waited_for_lock,
        )
    });

    let output = Command::new(&queue)
        .args(["run", "--once"])
        .env("PATH", &path)
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", &gate_command)
        .env("MERGE_QUEUE_TEST_LOCK_WAIT_MARKER", &lock_wait_marker)
        .output()
        .expect("run isolated coordinator");
    let (prepared_outside_lock, duplicate_sweep_was_deferred, waited_for_external_lock) =
        lock_releaser.join().expect("join gate lock releaser");
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
        duplicate_sweep_was_deferred,
        "patch-equivalent entry ran an eager worktree sweep before the next merge",
    );
    assert!(
        waited_for_external_lock,
        "coordinator never attempted to acquire the external gate lock",
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

    // Candidate checkout is a correctness boundary, not a best-effort setup
    // step. Reproduce a sandbox-denied gate-worktree index update and prove the
    // coordinator blocks the submission instead of gating the stale master
    // checkout and attributing that result to the branch.
    git(&repo, &["checkout", "-b", "checkout-denied", "master"]);
    fs::write(repo.join("must-not-be-gated-as-master"), "candidate\n")
        .expect("write denied-checkout candidate");
    git(&repo, &["add", "must-not-be-gated-as-master"]);
    git(&repo, &["commit", "-m", "candidate requiring checkout"]);
    git(&repo, &["checkout", "master"]);
    let submitted = Command::new(&queue)
        .args(["submit", "checkout-denied"])
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .output()
        .expect("submit checkout-denied fixture");
    assert!(submitted.status.success());
    fs::remove_file(&gate_marker).expect("clear prior gate marker");

    fs::write(
        &fake_git,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" checkout --detach --quiet \"*) exit 73 ;;\nesac\nexec {real_git:?} \"$@\"\n",
        ),
    )
    .expect("write checkout-denying git wrapper");
    fs::set_permissions(&fake_git, fs::Permissions::from_mode(0o755))
        .expect("chmod checkout-denying git wrapper");

    let denied = Command::new(&queue)
        .args(["run", "--once"])
        .env("PATH", &path)
        .env("MERGE_QUEUE_STATE_DIR", &state)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", &gate_command)
        .output()
        .expect("run coordinator with denied candidate checkout");
    assert!(
        denied.status.success(),
        "coordinator did not fail closed cleanly: {}",
        String::from_utf8_lossy(&denied.stderr),
    );
    assert!(!gate_marker.exists(), "stale gate worktree was gated after checkout failure");
    let journal = fs::read_to_string(state.join("journal.jsonl"))
        .expect("read checkout-failure journal");
    assert!(
        journal.lines().any(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            event["event"] == "blocked"
                && event["branch"] == "checkout-denied"
                && event["reason"] == "candidate checkout failed"
        }),
        "checkout failure was not journaled as blocked: {journal}",
    );
    assert!(!repo.join("must-not-be-gated-as-master").exists());
}

struct QueueFixture {
    _temp: TempDir,
    root: PathBuf,
    state: PathBuf,
    gate_worktree: PathBuf,
}

impl QueueFixture {
    fn stack(files: &[&str]) -> Self {
        let temp = TempDir::new();
        let root = temp.path().join("repo");
        let state = temp.path().join("state");
        let gate_worktree = temp.path().join("gate");
        fs::create_dir(&root).expect("create queue fixture repository");
        run_git(&root, &["init", "-b", "master"]);
        run_git(&root, &["config", "user.email", "queue-test@witchy.invalid"]);
        run_git(&root, &["config", "user.name", "Witchy Queue Test"]);
        fs::write(root.join("base.txt"), "base\n").expect("write base commit");
        run_git(&root, &["add", "base.txt"]);
        run_git(&root, &["commit", "-m", "base"]);

        let mut parent = "master".to_owned();
        for (index, file) in files.iter().enumerate() {
            let branch = ((b'a' + index as u8) as char).to_string();
            run_git(&root, &["switch", "-c", &branch, &parent]);
            if let Some(parent_dir) = root.join(file).parent() {
                fs::create_dir_all(parent_dir).expect("create stack file parent");
            }
            fs::write(root.join(file), format!("{branch}\n")).expect("write stack file");
            run_git(&root, &["add", file]);
            run_git(&root, &["commit", "-m", &format!("add {file}")]);
            parent = branch;
        }
        run_git(&root, &["switch", "master"]);
        Self {
            _temp: temp,
            root,
            state,
            gate_worktree,
        }
    }

    fn mq_command(&self, args: &[&str], gate: &str) -> Command {
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut command = Command::new("bash");
        command
            .arg(repo.join("scripts/merge-queue.sh"))
            .args(args)
            .env("MERGE_QUEUE_TEST_ROOT", &self.root)
            .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
            .env("MERGE_QUEUE_STATE_DIR", &self.state)
            .env("MERGE_QUEUE_GATE_WT", &self.gate_worktree)
            .env("MERGE_QUEUE_GATE_CMD", gate)
            .env("MERGE_QUEUE_ALLOW_MERGE", "1")
            .env("MERGE_QUEUE_MONITOR_INTERVAL", "0")
            .env("MERGE_QUEUE_RETRY_INTERVAL", "0")
            .env("MERGE_QUEUE_POLL_INTERVAL", "0");
        command
    }

    fn mq(&self, args: &[&str], gate: &str) -> std::process::Output {
        self.mq_command(args, gate)
            .output()
            .expect("run isolated merge queue")
    }

    fn mq_ok(&self, args: &[&str], gate: &str) -> std::process::Output {
        let output = self.mq(args, gate);
        assert!(
            output.status.success(),
            "merge queue {:?} failed:\nstdout:\n{}\nstderr:\n{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        output
    }

    fn change(&self, branch: &str) -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(self.state.join(format!("changes/{branch}.json")))
                .expect("read persistent change record"),
        )
        .expect("change record is JSON")
    }

    fn status(&self) -> serde_json::Value {
        serde_json::from_slice(&self.mq_ok(&["status"], "true").stdout)
            .expect("queue status is JSON")
    }

    fn journal(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.state.join("journal.jsonl"))
            .expect("read queue journal")
            .lines()
            .map(|line| serde_json::from_str(line).expect("journal event is JSON"))
            .collect()
    }
}

fn run_git(root: &Path, args: &[&str]) -> std::process::Output {
    let output = git_output(root, args);
    assert!(
        output.status.success(),
        "git {:?} failed:\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn git_output(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run git for queue fixture")
}

fn submit_stack(fixture: &QueueFixture, branches: &[&str]) {
    for (index, branch) in branches.iter().enumerate() {
        if index == 0 {
            fixture.mq_ok(&["submit", branch], "true");
        } else {
            fixture.mq_ok(&["submit", "--after", branches[index - 1], branch], "true");
        }
    }
}

#[test]
fn zero_whole_gate_timeout_does_not_kill_a_progressing_gate() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let output = fixture
        .mq_command(&["run", "--once"], "printf 'gate started\\n'; sleep 2")
        .env("MERGE_QUEUE_GATE_TIMEOUT", "0")
        .output()
        .expect("run gate with the whole-gate ceiling disabled");

    assert!(
        output.status.success(),
        "disabled whole-gate ceiling rejected a progressing gate: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(fixture.root.join("a.txt").exists(), "green candidate did not land");
    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "merged" && event["branch"] == "a"),
        "green candidate was not journaled as merged",
    );
    assert!(
        fixture
            .journal()
            .iter()
            .all(|event| event["event"] != "timeout"),
        "disabled whole-gate ceiling still produced a timeout",
    );

    let invalid = fixture
        .mq_command(&["status"], "true")
        .env("MERGE_QUEUE_GATE_TIMEOUT", "forever")
        .output()
        .expect("reject invalid whole-gate ceiling");
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stderr)
            .contains("MERGE_QUEUE_GATE_TIMEOUT must be a non-negative integer")
    );
}

#[test]
fn stale_gate_lock_reaps_its_recorded_process_group_before_regating() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let mut orphan = Command::new("sh")
        .args(["-c", "sleep 30"])
        .process_group(0)
        .spawn()
        .expect("start abandoned gate fixture");
    let orphan_pgid = orphan.id() as i32;
    let _guard = ProcessGroupGuard(orphan_pgid);
    let gate_lock = fixture.state.join("gate.lock");
    fs::create_dir(&gate_lock).expect("create stale gate lock");
    fs::write(gate_lock.join("pid"), "999999\n").expect("write dead coordinator pid");
    fs::write(gate_lock.join("gate_pgid"), format!("{orphan_pgid}\n"))
        .expect("write abandoned gate process group");
    fs::write(gate_lock.join("what"), "abandoned full gate\n")
        .expect("write lock description");

    fixture.mq_ok(&["run", "--once"], "true");
    let status = orphan.wait().expect("reap abandoned gate fixture");
    assert!(!status.success(), "abandoned gate process group was not terminated");
    assert!(fixture.root.join("a.txt").exists(), "replacement gate did not land");
}

#[test]
fn coordinator_start_recovers_an_orphaned_gating_claim() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let change_path = fixture.state.join("changes/a.json");
    let mut change: serde_json::Value = serde_json::from_slice(
        &fs::read(&change_path).expect("read queued change record"),
    )
    .expect("change record is JSON");
    change["state"] = serde_json::Value::String("gating".to_owned());
    fs::write(
        &change_path,
        serde_json::to_vec(&change).expect("serialize orphaned change record"),
    )
    .expect("write orphaned change record");

    fixture.mq_ok(&["run", "--once"], "true");

    assert!(fixture.root.join("a.txt").exists(), "recovered candidate did not land");
    assert!(
        fixture
            .journal()
            .iter()
            .any(|event| event["event"] == "recovered" && event["branch"] == "a"),
        "orphaned claim recovery was not journaled",
    );
}

#[test]
fn coordinator_exit_terminates_its_active_gate_process_group() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let gate_started = fixture._temp.path().join("gate-started");
    let gate = format!(
        "printf started >{}; while :; do /bin/sleep 1; done",
        gate_started.display(),
    );

    let mut coordinator = fixture
        .mq_command(&["run", "--once"], &gate)
        .spawn()
        .expect("start coordinator with a blocking gate");
    let coordinator_pid = coordinator.id() as i32;

    let gate_pgid_path = fixture.state.join("gate.lock/gate_pgid");
    let deadline = Instant::now() + Duration::from_secs(20);
    while (!gate_started.exists() || !gate_pgid_path.exists()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(gate_started.exists(), "blocking gate never started");
    let gate_pgid: i32 = fs::read_to_string(&gate_pgid_path)
        .expect("active gate PGID was not recorded in the lock")
        .trim()
        .parse()
        .expect("recorded gate PGID is numeric");
    let _gate_guard = ProcessGroupGuard(gate_pgid);

    let killed = Command::new("kill")
        .args(["-TERM", &coordinator_pid.to_string()])
        .status()
        .expect("terminate coordinator");
    assert!(killed.success(), "could not terminate coordinator");
    let status = coordinator.wait().expect("reap terminated coordinator");
    assert!(!status.success(), "terminated coordinator exited successfully");

    let deadline = Instant::now() + Duration::from_secs(5);
    while process_group_is_alive(gate_pgid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        !process_group_is_alive(gate_pgid),
        "gate process group survived coordinator exit"
    );
    assert!(
        !fixture.state.join("gate.lock").exists(),
        "coordinator exit left the serialized gate lock behind"
    );
}

#[test]
fn dependency_submission_keeps_stable_ids_reports_readiness_and_rejects_cycles() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    submit_stack(&fixture, &["a", "b"]);
    let first_id = fixture.change("a")["change_id"]
        .as_str()
        .expect("change id is a string")
        .to_owned();
    let first_attempt = fixture.change("a")["current_attempt"].clone();

    let status = fixture.status();
    let child = status["queue"]
        .as_array()
        .expect("queue is an array")
        .iter()
        .find(|entry| entry["branch"] == "b")
        .expect("child is queued");
    assert_eq!(child["readiness"], "waiting");
    assert_eq!(child["waiting_on"][0]["branch"], "a");

    run_git(&fixture.root, &["switch", "a"]);
    fs::write(fixture.root.join("a2.txt"), "a2\n").expect("update queued parent");
    run_git(&fixture.root, &["add", "a2.txt"]);
    run_git(&fixture.root, &["commit", "-m", "update a"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");
    assert_eq!(fixture.change("a")["change_id"], first_id);
    assert_ne!(fixture.change("a")["current_attempt"], first_attempt);
    assert_eq!(
        fs::read_dir(fixture.state.join("queue"))
            .expect("read queue")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count(),
        2,
        "resubmit duplicated the logical change",
    );

    let cycle = fixture.mq(&["submit", "--after", "b", "a"], "true");
    assert!(!cycle.status.success(), "dependency cycle was accepted");
    assert!(
        String::from_utf8_lossy(&cycle.stderr).contains("would create a cycle"),
        "cycle rejection was not explicit: {}",
        String::from_utf8_lossy(&cycle.stderr),
    );
}

#[test]
fn submit_precheck_writes_only_to_an_ephemeral_object_database() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    let bin = fixture._temp.path().join("git-wrapper-bin");
    fs::create_dir(&bin).expect("create fake git bin");
    let real_git = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate real git");
    assert!(real_git.status.success(), "git is required by the queue harness");
    let real_git = String::from_utf8(real_git.stdout)
        .expect("git path is utf8")
        .trim()
        .to_owned();
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\ncase \" $* \" in\n  *\" merge-tree --write-tree \"*)\n    [ -n \"${{GIT_OBJECT_DIRECTORY:-}}\" ] || exit 73\n    [ -n \"${{GIT_ALTERNATE_OBJECT_DIRECTORIES:-}}\" ] || exit 74\n    ;;\nesac\nexec {real_git} \"$@\"\n"
        ),
    )
    .expect("write git wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))
        .expect("chmod git wrapper");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = fixture
        .mq_command(&["submit", "a"], "true")
        .env("PATH", path)
        .output()
        .expect("submit through read-only-object precheck fixture");
    assert!(
        output.status.success(),
        "submit precheck required repository object writes:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn front_resubmission_moves_an_existing_change_to_the_actual_queue_head() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["submit", "b"], "true");

    fixture.mq_ok(&["submit", "--front", "b"], "true");

    let status = fixture.status();
    let queue = status["queue"].as_array().expect("queue is an array");
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[0]["branch"], "b");
    assert_eq!(queue[1]["branch"], "a");
}

#[test]
fn reused_branch_gets_a_new_change_id_without_forgetting_old_dependencies() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let old_id = fixture.change("a")["change_id"]
        .as_str()
        .expect("change id is a string")
        .to_owned();
    fixture.mq_ok(&["run", "--once"], "true");

    fixture.mq_ok(&["submit", "--after", "a", "b"], "true");
    run_git(&fixture.root, &["branch", "a", "master"]);
    run_git(&fixture.root, &["switch", "a"]);
    fs::write(fixture.root.join("a-next.txt"), "next generation\n")
        .expect("write next branch generation");
    run_git(&fixture.root, &["add", "a-next.txt"]);
    run_git(&fixture.root, &["commit", "-m", "reuse branch a"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let new_id = fixture.change("a")["change_id"]
        .as_str()
        .expect("new change id is a string")
        .to_owned();
    assert_ne!(new_id, old_id, "completed branch generation reused its old ID");
    let old_record: serde_json::Value = serde_json::from_slice(
        &fs::read(fixture.state.join(format!("changes/history-{old_id}.json")))
            .expect("old change generation remains addressable"),
    )
    .expect("old change record is JSON");
    assert_eq!(old_record["state"], "merged");

    let status = fixture.status();
    let old_child = status["queue"]
        .as_array()
        .expect("queue is an array")
        .iter()
        .find(|entry| entry["branch"] == "b")
        .expect("old child remains queued");
    assert_eq!(old_child["readiness"], "ready");
    assert_eq!(old_child["dependencies"][0]["change_id"], old_id);
}

#[test]
fn resubmission_during_gate_is_not_deleted_or_falsely_marked_merged() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let change_id = fixture.change("a")["change_id"].clone();
    let started = fixture._temp.path().join("gate-started");
    let proceed = fixture._temp.path().join("gate-proceed");
    let gate = format!(
        "touch '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; true",
        started.display(),
        proceed.display(),
    );
    let mut runner = fixture.mq_command(&["run", "--once"], &gate);
    let runner = runner
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start paused queue gate");
    // Checkout and replay are outside the gate lock and can take several
    // seconds on a loaded shared machine. Wait for the explicit gate marker;
    // this outer bound is deadlock protection, not the synchronization rule.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(started.exists(), "gate did not reach its pause point");

    run_git(&fixture.root, &["switch", "a"]);
    fs::write(fixture.root.join("a2.txt"), "updated while gating\n")
        .expect("write in-flight update");
    run_git(&fixture.root, &["add", "a2.txt"]);
    run_git(&fixture.root, &["commit", "-m", "update a while gating"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");
    assert_eq!(fixture.change("a")["change_id"], change_id);
    fs::write(&proceed, "go\n").expect("release paused gate");

    let output = runner.wait_with_output().expect("wait for queue drain");
    assert!(
        output.status.success(),
        "queue run failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(run_git(&fixture.root, &["show", "master:a2.txt"]).status.success());
    assert_eq!(fixture.change("a")["state"], "merged");
    let merged = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged" && event["branch"] == "a")
        .count();
    assert_eq!(merged, 2, "updated SHA did not receive its own gate attempt");
}

#[test]
fn concurrent_dependency_updates_cannot_commit_opposite_cycle_edges() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["submit", "b"], "true");

    let mut left = fixture.mq_command(&["submit", "--after", "b", "a"], "true");
    let mut right = fixture.mq_command(&["submit", "--after", "a", "b"], "true");
    let left = left
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start first concurrent dependency update");
    let right = right
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start second concurrent dependency update");
    let left = left.wait_with_output().expect("wait for first dependency update");
    let right = right.wait_with_output().expect("wait for second dependency update");

    assert_ne!(
        left.status.success(),
        right.status.success(),
        "exactly one opposite edge must commit:\nleft: {}\nright: {}",
        String::from_utf8_lossy(&left.stderr),
        String::from_utf8_lossy(&right.stderr),
    );
    let edge_count = fixture.change("a")["after"].as_array().unwrap().len()
        + fixture.change("b")["after"].as_array().unwrap().len();
    assert_eq!(edge_count, 1, "both cycle edges were persisted");
    assert!(!fixture.state.join("change.lock").exists(), "metadata lock leaked");
}

#[test]
fn merge_commit_submission_is_rejected_without_gating_or_erasing_resolutions() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    run_git(&fixture.root, &["switch", "-c", "side", "master"]);
    fs::write(fixture.root.join("side.txt"), "side\n").expect("write side branch");
    run_git(&fixture.root, &["add", "side.txt"]);
    run_git(&fixture.root, &["commit", "-m", "side change"]);
    run_git(&fixture.root, &["switch", "a"]);
    run_git(&fixture.root, &["merge", "--no-ff", "side", "-m", "merge with resolution boundary"]);
    run_git(&fixture.root, &["switch", "master"]);

    fixture.mq_ok(&["submit", "a"], "true");
    let marker = fixture.root.join("gate-must-not-run");
    let gate = format!("printf gated >{}", marker.display());
    fixture.mq_ok(&["run", "--once"], &gate);

    assert!(!marker.exists(), "merge-commit candidate reached the gate");
    assert_eq!(fixture.change("a")["state"], "conflict");
    assert!(fixture
        .journal()
        .iter()
        .any(|event| event["event"] == "conflict" && event["branch"] == "a"));
}

#[test]
fn red_parent_blocks_child_until_the_same_change_is_resubmitted_green() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt", "c.txt"]);
    submit_stack(&fixture, &["a", "b", "c"]);
    let parent_id = fixture.change("a")["change_id"].clone();

    fixture.mq_ok(&["run", "--once"], "false");
    let status = fixture.status();
    let child = status["queue"]
        .as_array()
        .expect("queue is an array")
        .iter()
        .find(|entry| entry["branch"] == "b")
        .expect("red parent's child remains queued");
    assert_eq!(child["readiness"], "blocked");
    assert_eq!(child["blocked_by"][0]["branch"], "a");
    assert_eq!(child["blocked_by"][0]["state"], "red");
    let grandchild = status["queue"]
        .as_array()
        .expect("queue is an array")
        .iter()
        .find(|entry| entry["branch"] == "c")
        .expect("red parent's grandchild remains queued");
    assert_eq!(grandchild["readiness"], "blocked");
    assert!(grandchild["blocked_by"]
        .as_array()
        .expect("blocked_by is an array")
        .iter()
        .any(|dependency| dependency["branch"] == "a" && dependency["state"] == "red"));
    assert!(
        !git_output(&fixture.root, &["show", "master:b.txt"]).status.success(),
        "child landed after a red parent",
    );

    fixture.mq_ok(&["submit", "a"], "true");
    assert_eq!(fixture.change("a")["change_id"], parent_id);
    fixture.mq_ok(&["run", "--once"], "true");
    assert!(run_git(&fixture.root, &["show", "master:c.txt"]).status.success());
    assert_eq!(fixture.change("a")["state"], "merged");
    assert_eq!(fixture.change("b")["state"], "merged");
    assert_eq!(fixture.change("c")["state"], "merged");
}

#[test]
fn represented_child_does_not_wait_for_an_obsolete_red_parent() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt", "c.txt"]);
    submit_stack(&fixture, &["a", "b", "c"]);

    fixture.mq_ok(&["run", "--once"], "false");
    assert_eq!(fixture.change("a")["state"], "red");
    assert_eq!(fixture.change("b")["state"], "queued");

    // Model a canonical integration tip landing the parent and child patches
    // without rewriting this obsolete queue graph.
    run_git(&fixture.root, &["cherry-pick", "a"]);
    run_git(&fixture.root, &["cherry-pick", "b"]);

    fixture.mq_ok(&["run", "--once"], "true");
    assert_eq!(fixture.change("a")["state"], "red");
    assert_eq!(fixture.change("b")["state"], "merged");
    assert_eq!(fixture.change("c")["state"], "queued");
    assert!(fixture.journal().iter().any(|event| {
        event["event"] == "already_merged" && event["branch"] == "b"
    }));
    assert!(
        !git_output(&fixture.root, &["show", "master:c.txt"])
            .status
            .success(),
        "unrepresented grandchild bypassed its red dependency",
    );
}

#[test]
fn operator_drop_retires_only_the_pending_attempt_and_preserves_its_branch() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    submit_stack(&fixture, &["a", "b"]);
    let branch_sha = git(&fixture.root, &["rev-parse", "a"]);

    fixture.mq_ok(
        &["drop", "a", "superseded by canonical integration tip"],
        "true",
    );

    assert_eq!(git(&fixture.root, &["rev-parse", "a"]), branch_sha);
    assert_eq!(fixture.change("a")["state"], "dropped");
    assert_eq!(
        fixture.change("a")["drop_reason"],
        "superseded by canonical integration tip"
    );
    assert_eq!(fixture.change("b")["state"], "queued");
    let status = fixture.status();
    let queued = status["queue"].as_array().expect("queue is an array");
    assert!(queued.iter().all(|entry| entry["branch"] != "a"));
    assert!(queued.iter().any(|entry| {
        entry["branch"] == "b"
            && entry["readiness"] == "blocked"
            && entry["blocked_by"][0]["state"] == "dropped"
    }));
    assert!(fixture.journal().iter().any(|event| {
        event["event"] == "dropped"
            && event["branch"] == "a"
            && event["reason"] == "superseded by canonical integration tip"
            && event["via"] == "operator drop"
    }));
}

#[test]
fn operator_drop_refuses_an_attempt_that_has_entered_the_gate() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    let started = fixture._temp.path().join("drop-gate-started");
    let proceed = fixture._temp.path().join("drop-gate-proceed");
    let gate = format!(
        "touch '{}'; while [ ! -f '{}' ]; do sleep 0.01; done; true",
        started.display(),
        proceed.display(),
    );
    let mut runner = fixture.mq_command(&["run", "--once"], &gate);
    let runner = runner
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start paused queue gate");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(started.exists(), "gate did not reach its pause point");

    let drop = fixture.mq(&["drop", "a", "too late"], "true");
    assert!(!drop.status.success(), "active gate was dropped");
    assert!(
        String::from_utf8_lossy(&drop.stderr).contains("current attempt is gating"),
        "drop refusal did not name the active state: {}",
        String::from_utf8_lossy(&drop.stderr),
    );
    fs::write(&proceed, "go\n").expect("release paused gate");
    let output = runner.wait_with_output().expect("wait for queue drain");
    assert!(
        output.status.success(),
        "queue run failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(fixture.change("a")["state"], "merged");
}

#[test]
fn green_dependency_stack_gates_the_tip_once_and_lands_the_whole_stack() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt", "c.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(&["submit", "b"], "true");
    fixture.mq_ok(&["submit", "--after", "a", "--after", "b", "c"], "true");
    fixture.mq_ok(&["run", "--once"], "true");

    assert!(run_git(&fixture.root, &["show", "master:c.txt"]).status.success());
    let merged: Vec<_> = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    assert_eq!(merged.len(), 3);
    assert!(merged.iter().all(|event| event["batch"] == "3"));
    let logs: BTreeSet<_> = merged
        .iter()
        .map(|event| event["log"].as_str().expect("merged event has log"))
        .collect();
    assert_eq!(logs.len(), 1, "stack used more than one green gate");
}

#[test]
fn markdown_only_stack_can_exceed_the_semantic_batch_limit() {
    let fixture = QueueFixture::stack(&[
        "a.md", "b.md", "c.md", "d.md", "e.md", "f.md",
    ]);
    submit_stack(&fixture, &["a", "b", "c", "d", "e", "f"]);
    let output = fixture
        .mq_command(&["run", "--once"], "true")
        .env("MERGE_QUEUE_BATCH_MAX", "5")
        .env("MERGE_QUEUE_DOCS_BATCH_MAX", "20")
        .output()
        .expect("run documentation batch");
    assert!(
        output.status.success(),
        "documentation batch failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(run_git(&fixture.root, &["show", "master:f.md"]).status.success());
    let merged: Vec<_> = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    assert_eq!(merged.len(), 6);
    assert!(merged.iter().all(|event| event["batch"] == "6"));
}

#[test]
fn documentation_batch_rejects_a_non_markdown_candidate() {
    let fixture = QueueFixture::stack(&["a.md", "b.txt", "c.md"]);
    submit_stack(&fixture, &["a", "b", "c"]);
    fixture.mq_ok(&["run", "--once"], "true");

    assert!(run_git(&fixture.root, &["show", "master:a.md"]).status.success());
    let merged: Vec<_> = fixture
        .journal()
        .into_iter()
        .filter(|event| event["event"] == "merged")
        .collect();
    assert_eq!(merged.len(), 3);
    assert_eq!(merged[0]["batch"], "1");
    assert_ne!(merged[0]["log"], merged[1]["log"]);
    assert_eq!(merged[1]["batch"], "2");
    assert_eq!(merged[1]["log"], merged[2]["log"]);
}

#[test]
fn queue_substrate_change_requests_the_isolated_fixture_shard() {
    let fixture = QueueFixture::stack(&["scripts/merge-queue.sh"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fixture.mq_ok(
        &["run", "--once"],
        "test \"$WITCHY_GATE_QUEUE_INFRA\" = 1",
    );
    assert!(
        run_git(&fixture.root, &["show", "master:scripts/merge-queue.sh"])
            .status
            .success()
    );
}

#[test]
fn queue_core_only_change_runs_the_hermetic_shard_instead_of_the_product_gate() {
    let fixture = QueueFixture::stack(&["scripts/merge-queue.sh"]);
    let check = fixture.root.join("scripts/check.sh");
    fs::create_dir_all(check.parent().expect("check script has a parent"))
        .expect("create fixture scripts directory");
    fs::write(
        &check,
        "#!/bin/sh\nset -eu\ntest \"$1\" = --queue-infra\ntest \"$WITCHY_GATE_QUEUE_INFRA\" = 1\n: >\"$QUEUE_SHARD_MARKER\"\n",
    )
    .expect("write fixture queue shard");
    fs::set_permissions(&check, fs::Permissions::from_mode(0o755))
        .expect("make fixture queue shard executable");
    run_git(&fixture.root, &["add", "scripts/check.sh"]);
    run_git(&fixture.root, &["commit", "-m", "add fixture product gate"]);
    run_git(&fixture.root, &["switch", "a"]);
    fs::write(
        fixture.root.join("scripts/MERGE-QUEUE.md"),
        "queue operator documentation\n",
    )
    .expect("write queue operator documentation");
    run_git(&fixture.root, &["add", "scripts/MERGE-QUEUE.md"]);
    run_git(&fixture.root, &["commit", "-m", "document queue substrate"]);
    run_git(&fixture.root, &["switch", "master"]);
    fixture.mq_ok(&["submit", "a"], "true");

    let marker = fixture._temp.path().join("queue-shard-ran");
    let mut command = fixture.mq_command(&["run", "--once"], "false");
    let output = command
        .env_remove("MERGE_QUEUE_GATE_CMD")
        .env("QUEUE_SHARD_MARKER", &marker)
        .output()
        .expect("run queue-core-only gate");
    assert!(
        output.status.success(),
        "queue-only gate failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(marker.exists(), "full product command replaced the queue shard");
    assert!(run_git(&fixture.root, &["show", "master:scripts/merge-queue.sh"])
        .status
        .success());
}

#[test]
fn dirty_main_master_defers_the_queue_before_running_the_gate() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    fixture.mq_ok(&["submit", "a"], "true");
    fs::write(fixture.root.join("base.txt"), "locally edited\n")
        .expect("dirty the main master checkout");

    let marker = fixture.root.join("gate-ran");
    let gate = format!("printf ran > {}", marker.display());
    fixture.mq_ok(&["run", "--once"], &gate);

    assert!(
        !marker.exists(),
        "the full gate ran despite tracked changes in the main master checkout"
    );
    assert_eq!(fixture.change("a")["state"], "queued");
    assert_eq!(
        fs::read_dir(fixture.state.join("queue"))
            .expect("read deferred queue")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json"))
            .count(),
        1,
        "the deferred submission was consumed"
    );
    assert!(fixture.journal().iter().any(|event| {
        event["event"] == "requeued"
            && event["branch"] == "a"
            && event["reason"] == "main master checkout has tracked changes before gate"
    }));
}

#[test]
fn status_reports_multiple_queue_entries_from_one_registry_snapshot() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt"]);
    submit_stack(&fixture, &["a", "b"]);

    let status = fixture.status();
    let queue = status["queue"].as_array().expect("queue is an array");
    assert_eq!(queue.len(), 2);
    let a = queue.iter().find(|entry| entry["branch"] == "a").expect("a is queued");
    let b = queue.iter().find(|entry| entry["branch"] == "b").expect("b is queued");
    assert_eq!(a["readiness"], "ready");
    assert_eq!(b["readiness"], "waiting");
    assert_eq!(b["waiting_on"][0]["branch"], "a");
}

#[test]
fn red_dependency_stack_bisects_and_lands_only_the_green_prefix() {
    let fixture = QueueFixture::stack(&["a.txt", "b.txt", "c.txt"]);
    submit_stack(&fixture, &["a", "b", "c"]);
    fixture.mq_ok(&["run", "--once"], "test ! -f c.txt");

    assert!(run_git(&fixture.root, &["show", "master:a.txt"]).status.success());
    assert!(run_git(&fixture.root, &["show", "master:b.txt"]).status.success());
    let missing_c = Command::new("git")
        .args(["-C", fixture.root.to_str().unwrap(), "show", "master:c.txt"])
        .output()
        .expect("inspect red suffix on master");
    assert!(!missing_c.status.success(), "red stack suffix landed");

    let journal = fixture.journal();
    let split = journal
        .iter()
        .find(|event| event["event"] == "batch_red")
        .expect("initial stack red was journaled");
    assert_eq!(split["strategy"], "prefix_split");
    assert_eq!(split["members"], "a b c");
    assert!(journal.iter().any(|event| event["event"] == "merged" && event["branch"] == "a"));
    assert!(journal.iter().any(|event| event["event"] == "merged" && event["branch"] == "b"));
    assert!(journal.iter().any(|event| event["event"] == "red" && event["branch"] == "c"));
    let gate_logs: BTreeSet<_> = journal
        .iter()
        .filter(|event| {
            event["event"] == "batch_red" || event["event"] == "merged" || event["event"] == "red"
        })
        .filter_map(|event| event["log"].as_str())
        .collect();
    assert_eq!(gate_logs.len(), 3, "expected full stack, prefix, and suffix gates");
}

#[test]
fn daemon_enters_an_independent_process_group() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let isolated_root = temp.path().join("isolated-root");
    let isolated_scripts = isolated_root.join("scripts");
    fs::create_dir_all(&isolated_scripts).expect("create isolated script directory");
    let queue = isolated_scripts.join("merge-queue.sh");
    fs::copy(root.join("scripts/merge-queue.sh"), &queue).expect("copy queue script");
    fs::copy(
        root.join("scripts/state-paths.sh"),
        isolated_scripts.join("state-paths.sh"),
    )
    .expect("copy state path script");
    let state_root = temp.path().join("state-root");
    let state = state_root.join("merge-queue");
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
        .arg(&queue)
        .env("MERGE_QUEUE_TEST_ROOT", &root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("WITCHY_STATE_DIR", &state_root)
        .env("MERGE_QUEUE_GATE_WT", &gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", "true")
        .env("MERGE_QUEUE_COORDINATOR_SCRIPT", &queue)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start isolated coordinator daemon launcher");
    let launcher_group = launcher.id() as i32;

    let pid_path = state.join("coordinator.pid");
    // The daemon command has its own readiness handshake. This outer bound is
    // only deadlock protection and remains below nextest's hard timeout.
    let deadline = Instant::now() + Duration::from_secs(90);
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
    assert_ne!(
        process_group(pid),
        launcher_group,
        "coordinator {pid} remained in the launcher's process group"
    );
    let killed = Command::new("kill")
        .args(["-TERM", "--", &format!("-{launcher_group}")])
        .status()
        .expect("terminate daemon launcher process group");
    assert!(killed.success(), "could not terminate daemon launcher group");
    let _ = launcher.wait();

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
fn queued_work_preempts_an_idle_prewarm_process_group() {
    // Keep rustup blocked before Cargo: the production stall occurred when
    // this setup ran synchronously before the cancellable prewarm PGID existed.
    let fixture = QueueFixture::stack(&["a.txt"]);
    run_git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--detach",
            fixture.gate_worktree.to_str().unwrap(),
            "master",
        ],
    );
    let bin = fixture._temp.path().join("prewarm-bin");
    fs::create_dir(&bin).expect("create fake prewarm bin");
    let started = fixture._temp.path().join("prewarm-started");
    let cancelled = fixture._temp.path().join("prewarm-cancelled");
    let rustup_pid_file = fixture._temp.path().join("prewarm-rustup-pid");
    let cargo_env = fixture._temp.path().join("prewarm-cargo-env");
    let gate_ran = fixture._temp.path().join("gate-ran");
    let gate_proceed = fixture._temp.path().join("gate-proceed");
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\nif [ -e \"{}\" ]; then\n  printf '%s|%s|%s' \"${{CARGO_INCREMENTAL-unset}}\" \"${{RUSTC_WRAPPER-unset}}\" \"${{CARGO_BUILD_RUSTC_WRAPPER-unset}}\" >\"{}\"\nfi\nexit 0\n",
            gate_proceed.display(),
            cargo_env.display(),
        ),
    )
    .expect("write fake prewarm cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("chmod fake cargo");
    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        format!(
            "#!/bin/sh\nif [ \"$1\" = target ] && [ ! -e \"{}\" ]; then\n  trap 'printf cancelled >\"{}\"; exit 143' TERM INT\n  printf '%s' \"$$\" >\"{}\"\n  printf started >\"{}\"\n  while :; do /bin/sleep 1; done\nfi\ncase \"$1\" in\n  target) exit 0 ;;\n  which) printf '/usr/bin/rustc\\n'; exit 0 ;;\nesac\nexit 1\n",
            started.display(),
            cancelled.display(),
            rustup_pid_file.display(),
            started.display(),
        ),
    )
    .expect("write blocking rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake rustup");

    let queue_script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-queue.sh");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let gate = format!(
        "printf ran >{}; while [ ! -e {} ]; do /bin/sleep 0.05; done",
        gate_ran.display(),
        gate_proceed.display(),
    );
    let mut coordinator = Command::new("bash")
        .arg(&queue_script)
        .arg("run")
        .env("PATH", path)
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_GATE_WT", &fixture.gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", gate)
        .env("MERGE_QUEUE_ALLOW_MERGE", "1")
        .env("MERGE_QUEUE_POLL_INTERVAL", "1")
        .env("MERGE_QUEUE_RETRY_INTERVAL", "1")
        .env("CARGO_INCREMENTAL", "1")
        .env("RUSTC_WRAPPER", "forbidden-wrapper")
        .env("CARGO_BUILD_RUSTC_WRAPPER", "forbidden-wrapper")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start persistent coordinator");
    let coordinator_pid = coordinator.id() as i32;
    let guard = ProcessGroupGuard(coordinator_pid);

    let deadline = Instant::now() + Duration::from_secs(20);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(started.exists(), "coordinator never entered prewarm");
    let rustup_pid = fs::read_to_string(&rustup_pid_file)
        .expect("read blocking rustup pid")
        .parse::<i32>()
        .expect("parse blocking rustup pid");
    let prewarm_guard = ProcessGroupGuard(process_group(rustup_pid));
    assert!(
        fixture.state.join("gate.lock").exists(),
        "prewarm did not retain the serialized gate lock"
    );

    fixture.mq_ok(&["submit", "a"], "true");
    let deadline = Instant::now() + Duration::from_secs(20);
    while (!cancelled.exists() || !gate_ran.exists()) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(cancelled.exists(), "queued work did not cancel idle prewarm");
    assert!(gate_ran.exists(), "coordinator did not advance queued work after prewarm");
    drop(prewarm_guard);
    assert!(
        !fixture.state.join("prewarmed").exists(),
        "cancelled prewarm was recorded as complete"
    );
    fs::write(&gate_proceed, "go\n").expect("release fake gate");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !git_output(&fixture.root, &["show", "master:a.txt"])
        .status
        .success()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        git_output(&fixture.root, &["show", "master:a.txt"])
            .status
            .success(),
        "queued branch did not land"
    );
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cargo_env.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        fs::read_to_string(&cargo_env).expect("read prewarm Cargo environment"),
        "0||",
        "idle prewarm did not match the full gate Cargo profile",
    );

    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(5);
    while coordinator
        .try_wait()
        .expect("poll terminated coordinator")
        .is_none()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        coordinator
            .try_wait()
            .expect("reap terminated coordinator")
            .is_some(),
        "coordinator ignored process-group termination"
    );
}

#[test]
fn queued_work_cancels_inactive_prewarm_and_preserves_active_generation() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    run_git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--detach",
            fixture.gate_worktree.to_str().unwrap(),
            "master",
        ],
    );
    fs::create_dir_all(&fixture.state).expect("create generation state");
    fs::create_dir_all(fixture.gate_worktree.join("target"))
        .expect("create active gate target");
    fs::write(
        fixture.gate_worktree.join("target/generation-sentinel"),
        "active\n",
    )
    .expect("write active generation sentinel");
    fs::write(fixture.state.join("gate-target"), "target\n")
        .expect("select active gate target");

    let bin = fixture._temp.path().join("inactive-prewarm-bin");
    fs::create_dir(&bin).expect("create fake prewarm bin");
    let started = fixture._temp.path().join("inactive-prewarm-started");
    let cancelled = fixture._temp.path().join("inactive-prewarm-cancelled");
    let cargo_pid_file = fixture._temp.path().join("inactive-prewarm-cargo-pid");
    let prewarm_target = fixture._temp.path().join("inactive-prewarm-target");
    let gate_ran = fixture._temp.path().join("gate-ran");
    let gate_release = fixture._temp.path().join("gate-release");
    let gate_target = fixture._temp.path().join("gate-target-seen");
    let gate_sentinel = fixture._temp.path().join("gate-sentinel-seen");
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\ntarget_dir=\"${{CARGO_TARGET_DIR:-target}}\"\nprintf '%s' \"$target_dir\" >\"{}\"\ntrap 'printf cancelled >\"{}\"; exit 143' TERM INT\nmkdir -p \"$target_dir/debug/.fingerprint/inactive-prewarm\"\n: >\"$target_dir/debug/.fingerprint/inactive-prewarm/invoked.timestamp\"\nprintf '%s' \"$$\" >\"{}\"\nprintf started >\"{}\"\nwhile :; do /bin/sleep 1; done\n",
            prewarm_target.display(),
            cancelled.display(),
            cargo_pid_file.display(),
            started.display(),
        ),
    )
    .expect("write blocking inactive prewarm Cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("chmod blocking inactive prewarm Cargo");
    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  target) exit 0 ;;\n  which) printf '{}/rustc\\n'; exit 0 ;;\nesac\nexit 1\n",
            bin.display(),
        ),
    )
    .expect("write fake prewarm rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake prewarm rustup");

    let queue_script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-queue.sh");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let gate = format!(
        "printf '%s' \"${{CARGO_TARGET_DIR:-unset}}\" >{}; cat \"${{CARGO_TARGET_DIR:-target}}/generation-sentinel\" >{}; printf ran >{}; while [ ! -e {} ]; do /bin/sleep 0.05; done",
        gate_target.display(),
        gate_sentinel.display(),
        gate_ran.display(),
        gate_release.display(),
    );
    let mut coordinator = Command::new("bash")
        .arg(&queue_script)
        .arg("run")
        .env("PATH", path)
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_GATE_WT", &fixture.gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", gate)
        .env("MERGE_QUEUE_ALLOW_MERGE", "1")
        .env("MERGE_QUEUE_MONITOR_INTERVAL", "0.05")
        .env("MERGE_QUEUE_POLL_INTERVAL", "0.05")
        .env("MERGE_QUEUE_RETRY_INTERVAL", "0.05")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start persistent coordinator");
    let coordinator_pid = coordinator.id() as i32;
    let coordinator_guard = ProcessGroupGuard(coordinator_pid);

    let deadline = Instant::now() + Duration::from_secs(20);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(started.exists(), "coordinator never entered inactive Cargo prewarm");
    assert_eq!(
        fs::read_to_string(&prewarm_target).expect("read prewarm target"),
        "target-prewarm",
        "idle prewarm did not use the inactive generation",
    );
    let cargo_pid = fs::read_to_string(&cargo_pid_file)
        .expect("read blocking Cargo pid")
        .parse::<i32>()
        .expect("parse blocking Cargo pid");
    let cargo_pgid = process_group(cargo_pid);
    let coordinator_pgid = process_group(coordinator_pid);
    let cargo_guard = (cargo_pgid != coordinator_pgid).then(|| ProcessGroupGuard(cargo_pgid));

    let submitted_at = Instant::now();
    fixture.mq_ok(&["submit", "a"], "true");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gate_ran.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(gate_ran.exists(), "queued gate waited behind inactive prewarm Cargo");
    assert!(
        submitted_at.elapsed() < Duration::from_secs(10),
        "queued gate did not start promptly after cancelling inactive prewarm"
    );
    assert!(cancelled.exists(), "queued work did not cancel inactive prewarm Cargo");
    assert_eq!(
        fs::read_to_string(&gate_target).expect("read gate target"),
        "target",
        "gate did not retain the active target generation",
    );
    assert_eq!(
        fs::read_to_string(&gate_sentinel).expect("read gate sentinel"),
        "active\n",
        "gate did not see the active target sentinel",
    );
    assert_eq!(
        fs::read_to_string(fixture.state.join("gate-target")).expect("read target pointer"),
        "target\n",
        "cancelled prewarm changed the active target pointer",
    );
    assert_eq!(
        fs::read_to_string(fixture.gate_worktree.join("target/generation-sentinel"))
            .expect("read unchanged active sentinel"),
        "active\n",
        "inactive prewarm mutated the active target",
    );
    assert!(
        !fixture
            .gate_worktree
            .join("target/debug/.fingerprint/inactive-prewarm/invoked.timestamp")
            .exists(),
        "inactive prewarm wrote a fingerprint into the active target",
    );
    assert!(
        fixture
            .gate_worktree
            .join("target-prewarm/debug/.fingerprint/inactive-prewarm/invoked.timestamp")
            .exists(),
        "inactive prewarm never touched its target generation",
    );
    assert!(
        !fixture.state.join("prewarmed").exists(),
        "cancelled prewarm was recorded as complete",
    );
    assert!(
        fixture.state.join("prewarm-incomplete").exists(),
        "cancelled generation lost its incomplete marker",
    );

    fs::write(&gate_release, "go\n").expect("release fake gate");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !git_output(&fixture.root, &["show", "master:a.txt"])
        .status
        .success()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        git_output(&fixture.root, &["show", "master:a.txt"])
            .status
            .success(),
        "queued branch did not land after inactive prewarm cancellation",
    );
    if cargo_pgid != coordinator_pgid {
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_group_is_alive(cargo_pgid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(25));
        }
        assert!(!process_group_is_alive(cargo_pgid), "cancelled Cargo group survived");
        std::mem::forget(cargo_guard);
    }
    drop(coordinator_guard);
    let deadline = Instant::now() + Duration::from_secs(5);
    while coordinator
        .try_wait()
        .expect("poll terminated coordinator")
        .is_none()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        coordinator
            .try_wait()
            .expect("reap terminated coordinator")
            .is_some(),
        "coordinator ignored process-group termination",
    );
}

#[test]
fn successful_prewarm_promotes_inactive_generation_for_next_gate() {
    let fixture = QueueFixture::stack(&["a.txt"]);
    run_git(
        &fixture.root,
        &[
            "worktree",
            "add",
            "--detach",
            fixture.gate_worktree.to_str().unwrap(),
            "master",
        ],
    );
    fs::create_dir_all(&fixture.state).expect("create generation state");
    fs::create_dir_all(fixture.gate_worktree.join("target"))
        .expect("create active gate target");
    fs::create_dir_all(fixture.gate_worktree.join("target-clippy"))
        .expect("create active clippy target");
    fs::create_dir_all(fixture.gate_worktree.join("target-check"))
        .expect("create active check target");
    fs::create_dir_all(fixture.gate_worktree.join("target-prewarm-clippy"))
        .expect("create inactive clippy target");
    fs::create_dir_all(fixture.gate_worktree.join("target-prewarm-check"))
        .expect("create inactive check target");
    fs::write(
        fixture.gate_worktree.join("target/generation-sentinel"),
        "old-active\n",
    )
    .expect("write old active generation sentinel");
    fs::write(fixture.state.join("gate-target"), "target\n")
        .expect("select initial gate target");

    let bin = fixture._temp.path().join("promoting-prewarm-bin");
    fs::create_dir(&bin).expect("create fake promoting prewarm bin");
    let started = fixture._temp.path().join("promoting-prewarm-started");
    let prewarm_release = fixture._temp.path().join("promoting-prewarm-release");
    let cargo_targets = fixture._temp.path().join("promoting-prewarm-targets");
    let gate_ran = fixture._temp.path().join("promoted-gate-ran");
    let gate_release = fixture._temp.path().join("promoted-gate-release");
    let gate_target = fixture._temp.path().join("promoted-gate-target");
    let gate_sentinel = fixture._temp.path().join("promoted-gate-sentinel");
    let cargo = bin.join("cargo");
    fs::write(
        &cargo,
        format!(
            "#!/bin/sh\ntarget_dir=\"${{CARGO_TARGET_DIR:-target}}\"\nprintf '%s\\n' \"$target_dir\" >>\"{}\"\nif [ ! -e \"{}\" ]; then\n  mkdir -p \"$target_dir\"\n  printf promoted >\"$target_dir/generation-sentinel\"\n  printf started >\"{}\"\n  while [ ! -e \"{}\" ]; do /bin/sleep 0.05; done\nfi\nexit 0\n",
            cargo_targets.display(),
            started.display(),
            started.display(),
            prewarm_release.display(),
        ),
    )
    .expect("write successful fake prewarm Cargo");
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755))
        .expect("chmod successful fake prewarm Cargo");
    let rustup = bin.join("rustup");
    fs::write(
        &rustup,
        format!(
            "#!/bin/sh\ncase \"$1\" in\n  target) exit 0 ;;\n  which) printf '{}/rustc\\n'; exit 0 ;;\nesac\nexit 1\n",
            bin.display(),
        ),
    )
    .expect("write fake promoting rustup");
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755))
        .expect("chmod fake promoting rustup");

    let queue_script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/merge-queue.sh");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let gate = format!(
        "printf '%s' \"${{CARGO_TARGET_DIR:-unset}}\" >{}; cat \"${{CARGO_TARGET_DIR:-target}}/generation-sentinel\" >{}; printf ran >{}; while [ ! -e {} ]; do /bin/sleep 0.05; done",
        gate_target.display(),
        gate_sentinel.display(),
        gate_ran.display(),
        gate_release.display(),
    );
    let mut coordinator = Command::new("bash")
        .arg(&queue_script)
        .arg("run")
        .env("PATH", path)
        .env("MERGE_QUEUE_TEST_ROOT", &fixture.root)
        .env("MERGE_QUEUE_ALLOW_TEST_ROOT", "1")
        .env("MERGE_QUEUE_STATE_DIR", &fixture.state)
        .env("MERGE_QUEUE_GATE_WT", &fixture.gate_worktree)
        .env("MERGE_QUEUE_GATE_CMD", gate)
        .env("MERGE_QUEUE_ALLOW_MERGE", "1")
        .env("MERGE_QUEUE_MONITOR_INTERVAL", "0.05")
        .env("MERGE_QUEUE_POLL_INTERVAL", "0.05")
        .env("MERGE_QUEUE_RETRY_INTERVAL", "0.05")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start persistent coordinator");
    let coordinator_pid = coordinator.id() as i32;
    let coordinator_guard = ProcessGroupGuard(coordinator_pid);

    let deadline = Instant::now() + Duration::from_secs(20);
    while !started.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(started.exists(), "coordinator never started inactive prewarm");
    assert_eq!(
        fs::read_to_string(fixture.state.join("gate-target")).expect("read initial pointer"),
        "target\n",
        "target generation was promoted before prewarm completed",
    );
    assert!(
        fixture.state.join("prewarm-incomplete").exists(),
        "in-progress generation was not marked incomplete",
    );
    assert!(!fixture.state.join("prewarmed").exists());
    assert_eq!(
        fs::read_to_string(fixture.gate_worktree.join("target/generation-sentinel"))
            .expect("read old active sentinel"),
        "old-active\n",
        "inactive prewarm mutated the old active generation",
    );

    fs::write(&prewarm_release, "go\n").expect("release successful prewarm");
    let deadline = Instant::now() + Duration::from_secs(20);
    while fs::read_to_string(fixture.state.join("gate-target"))
        .is_ok_and(|target| target.trim() != "target-prewarm")
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        fs::read_to_string(fixture.state.join("gate-target")).expect("read promoted pointer"),
        "target-prewarm\n",
        "successful prewarm did not atomically promote the inactive generation",
    );
    assert!(fixture.state.join("prewarmed").exists(), "prewarm completion was not recorded");
    assert!(
        !fixture.state.join("prewarm-incomplete").exists(),
        "successful generation retained the incomplete marker",
    );
    let targets = fs::read_to_string(&cargo_targets).expect("read prewarm Cargo targets");
    assert!(
        targets.lines().any(|target| target == "target-prewarm"),
        "main prewarm did not use target-prewarm: {targets}",
    );
    assert!(
        targets.lines().any(|target| target == "target-prewarm-clippy"),
        "clippy prewarm did not use the inactive suffix: {targets}",
    );
    assert!(
        targets.lines().any(|target| target == "target-prewarm-check"),
        "check prewarm did not use the inactive suffix: {targets}",
    );

    fixture.mq_ok(&["submit", "a"], "true");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !gate_ran.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(gate_ran.exists(), "gate did not run after successful promotion");
    assert_eq!(
        fs::read_to_string(&gate_target).expect("read promoted gate target"),
        "target-prewarm",
        "next gate did not receive the promoted CARGO_TARGET_DIR",
    );
    assert_eq!(
        fs::read_to_string(&gate_sentinel).expect("read promoted gate sentinel"),
        "promoted",
        "next gate did not consume the promoted generation sentinel",
    );
    assert_eq!(
        fs::read_to_string(fixture.state.join("gate-target")).expect("read stable pointer"),
        "target-prewarm\n",
        "gate changed the promoted target pointer",
    );

    fs::write(&gate_release, "go\n").expect("release promoted gate");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !git_output(&fixture.root, &["show", "master:a.txt"])
        .status
        .success()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        git_output(&fixture.root, &["show", "master:a.txt"])
            .status
            .success(),
        "queued branch did not land through the promoted generation",
    );
    drop(coordinator_guard);
    let deadline = Instant::now() + Duration::from_secs(5);
    while coordinator
        .try_wait()
        .expect("poll terminated coordinator")
        .is_none()
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        coordinator
            .try_wait()
            .expect("reap terminated coordinator")
            .is_some(),
        "coordinator ignored process-group termination",
    );
}

#[test]
fn doctor_treats_denied_process_inspection_as_advisory() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let temp = TempDir::new();
    let state = temp.path().join("state");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&state).expect("create isolated coordinator state");
    let queue = state.join("queue");
    fs::create_dir(&queue).expect("create isolated queue");
    fs::write(queue.join("0001.json"), "{}\n").expect("write logical queue entry");
    fs::write(queue.join("0001.json.nobatch"), "").expect("write no-batch sidecar");
    fs::write(queue.join("0001.json.batch-limit"), "2\n")
        .expect("write batch-limit sidecar");
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("queue       : 1 pending — 0001.json"),
        "doctor counted queue sidecars as logical entries: {stdout}",
    );
    assert!(
        !stdout.contains("0001.json.nobatch") && !stdout.contains("0001.json.batch-limit"),
        "doctor exposed internal queue sidecars: {stdout}",
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
    let cargo_args_file = temp.path().join("cargo-args");
    let cargo_env_file = temp.path().join("cargo-env");
    let output = Command::new("bash")
        .arg(root.join("scripts/check.sh"))
        .arg("--fast")
        .env("PATH", &path)
        .env("CARGO_TARGET_DIR", temp.path().join("target"))
        .env_remove("WITCHY_GATE_QUEUE_INFRA")
        .env_remove("CARGO_PROFILE_TEST_STRIP")
        .env("WITCHY_GATE_SCOPE", "all")
        .env("WITCHY_GATE_TEST_JOBS", "4")
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
        cargo_args
            .lines()
            .any(|line| line.starts_with("nextest run -j 4 --workspace")),
        "serialized gate did not bound nextest execution: {cargo_args}",
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
    assert_eq!(timings[1]["name"], "clippy (deny warnings)");
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
            "clippy (deny warnings)",
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

    // The fmt stage runs the nextest-built binary from the target dir; give
    // the fake toolchain one.
    let target = temp.path().join("target-full");
    fs::create_dir_all(target.join("debug")).expect("create fake target dir");
    let witchy = target.join("debug/witchy");
    fs::write(&witchy, "#!/bin/sh\nexit 0\n").expect("write fake witchy");
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
            "compile check (cargo check)",
            "clippy (deny warnings)",
            "wasm playground build",
            "runnable book (browser)",
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
        .find(|timing| timing["name"] == "clippy (deny warnings)")
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
        .mq_command(&["run", "--once"], "true")
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
        .mq_command(&["run", "--once"], "true")
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
}
