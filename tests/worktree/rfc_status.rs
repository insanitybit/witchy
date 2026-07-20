use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempRepo(PathBuf);

impl TempRepo {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("witchy-rfc-status-{}-{nonce}", std::process::id()));
        fs::create_dir_all(path.join("rfcs")).expect("create RFC fixture");
        fs::create_dir_all(path.join("scripts")).expect("create scripts fixture");
        copy_script("scripts/rfc-status.sh", &path);
        copy_script("scripts/state-paths.sh", &path);
        git(&path, &["init", "--quiet"]);
        git(&path, &["symbolic-ref", "HEAD", "refs/heads/master"]);
        git(&path, &["config", "user.email", "rfc-status-test@witchy.invalid"]);
        git(&path, &["config", "user.name", "Witchy RFC Status Test"]);
        write_rfc(&path, "0001", "implemented", "shipped");
        git(&path, &["add", "."]);
        git(&path, &["commit", "--quiet", "-m", "initial"]);
        Self(path)
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn copy_script(relative: &str, repo: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::copy(source, repo.join(relative)).expect("copy script fixture");
}

fn write_rfc(repo: &Path, id: &str, status: &str, tracking: &str) {
    fs::write(
        repo.join(format!("rfcs/{id}-fixture.md")),
        format!("---\nrfc: {id}\ntitle: Fixture\nstatus: {status}\ntracking: {tracking}\n---\n"),
    )
    .expect("write RFC fixture");
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output is utf8").trim().to_string()
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new("bash")
        .arg(repo.join("scripts/rfc-status.sh"))
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run rfc-status")
}

#[test]
fn check_rejects_unowned_proposals_and_vague_statuses() {
    let repo = TempRepo::new();
    write_rfc(&repo.0, "0002", "proposed", "");
    write_rfc(&repo.0, "0003", "in-progress", "agent working");
    let output = run(&repo.0, &["--check"]);
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RFC-0002  proposed     STALE"), "{stdout}");
    assert!(stdout.contains("RFC-0003  in-progress  INVALID"), "{stdout}");
}

#[test]
fn clean_ahead_branch_is_a_pickup_until_queued() {
    let repo = TempRepo::new();
    write_rfc(&repo.0, "0002", "proposed", "foundation");
    git(&repo.0, &["add", "rfcs/0002-fixture.md"]);
    git(&repo.0, &["commit", "--quiet", "-m", "add proposal"]);
    git(&repo.0, &["checkout", "--quiet", "-b", "impl/rfc0002-slice"]);
    fs::write(repo.0.join("slice.txt"), "work\n").expect("write slice");
    git(&repo.0, &["add", "slice.txt"]);
    git(&repo.0, &["commit", "--quiet", "-m", "implement slice"]);

    let pickup = run(&repo.0, &["--check"]);
    assert!(!pickup.status.success());
    assert!(String::from_utf8_lossy(&pickup.stdout).contains("PICKUP"));

    let queue = repo.0.join("state/merge-queue/queue");
    fs::create_dir_all(&queue).expect("create queue fixture");
    fs::write(queue.join("change.json"), r#"{"branch":"impl/rfc0002-slice"}"#)
        .expect("write queued change");
    let queued = run(&repo.0, &["--check"]);
    assert!(queued.status.success(), "{}", String::from_utf8_lossy(&queued.stdout));
    assert!(String::from_utf8_lossy(&queued.stdout).contains("QUEUED"));
}

#[test]
fn accepted_policy_with_tracking_and_terminal_rfcs_are_valid() {
    let repo = TempRepo::new();
    write_rfc(&repo.0, "0002", "accepted", "ongoing ordering contract");
    let output = run(&repo.0, &["--check", "--all"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("RFC-0001  implemented  TERMINAL"), "{stdout}");
    assert!(stdout.contains("RFC-0002  accepted     TRACKED"), "{stdout}");
}
