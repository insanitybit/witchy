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


#[path = "merge_queue/coordinator.rs"]
mod coordinator;
#[path = "merge_queue/gate_behavior.rs"]
mod gate_behavior;
