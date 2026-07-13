use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct TempRepo(PathBuf);

impl TempRepo {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("witchy-worktree-create-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create temp repo");
        git(&path, &["init", "--quiet"]);
        git(&path, &["config", "user.email", "worktree-test@witchy.invalid"]);
        git(&path, &["config", "user.name", "Witchy Worktree Test"]);
        fs::write(path.join("README.md"), "fixture\n").expect("write fixture");
        git(&path, &["add", "README.md"]);
        git(&path, &["commit", "--quiet", "-m", "fixture"]);
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
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

fn create(repo: &Path, name: &str) -> Output {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/worktree-create.sh");
    Command::new("bash")
        .arg(script)
        .arg(name)
        .current_dir(repo)
        .env("CLAUDE_PROJECT_DIR", repo)
        .env("WITCHY_WORKTREE_CREATE_PREBUILD", "0")
        .output()
        .expect("run worktree-create.sh")
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, contents).expect("write fake executable");
    let mut permissions = fs::metadata(path).expect("fake executable metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make fake executable runnable");
}

fn created_path(output: &Output) -> PathBuf {
    assert!(
        output.status.success(),
        "worktree creation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.lines().count(), 1, "hook stdout must contain only the worktree path");
    PathBuf::from(stdout.trim())
}

#[test]
fn worktree_create_never_reuses_a_merged_branch_name() {
    let repo = TempRepo::new();
    let root = repo.path();
    let journal = root.join("scratch/merge-queue/journal.jsonl");
    fs::create_dir_all(journal.parent().expect("journal parent")).expect("create journal parent");
    fs::write(
        &journal,
        "{\"event\":\"merged\",\"branch\":\"worktree-reused\",\"sha\":\"fixture\"}\n",
    )
    .expect("write journal");

    let historical = create(root, "reused");
    let historical_path = created_path(&historical);
    let historical_branch = git(&historical_path, &["branch", "--show-current"]);
    assert_ne!(historical_branch, "worktree-reused");
    assert!(historical_branch.starts_with("worktree-reused-"));
    assert!(String::from_utf8_lossy(&historical.stderr).contains("previously merged"));

    let fresh = create(root, "fresh");
    let fresh_path = created_path(&fresh);
    assert_eq!(git(&fresh_path, &["branch", "--show-current"]), "worktree-fresh");

    git(root, &["branch", "worktree-live"]);
    let live = create(root, "live");
    let live_path = created_path(&live);
    let live_branch = git(&live_path, &["branch", "--show-current"]);
    assert_ne!(live_branch, "worktree-live");
    assert!(live_branch.starts_with("worktree-live-"));
}

#[cfg(unix)]
#[test]
fn worktree_create_runs_background_prebuild_at_utility_priority() {
    let repo = TempRepo::new();
    let root = repo.path();
    let bin = root.join("fake-bin");
    let trace = root.join("prebuild-trace");
    fs::create_dir(&bin).expect("create fake bin");
    fs::create_dir(&trace).expect("create trace dir");
    write_executable(
        &bin.join("taskpolicy"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$WITCHY_PREBUILD_TRACE/taskpolicy\"\nshift 2\nexec \"$@\"\n",
    );
    write_executable(
        &bin.join("cargo"),
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$WITCHY_PREBUILD_TRACE/cargo\"\n",
    );

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/worktree-create.sh");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
    let output = Command::new("bash")
        .arg(script)
        .arg("priority")
        .current_dir(root)
        .env("CLAUDE_PROJECT_DIR", root)
        .env("WITCHY_PREBUILD_TRACE", &trace)
        .env("PATH", path)
        .output()
        .expect("run worktree-create.sh");
    created_path(&output);

    let cargo_trace = trace.join("cargo");
    let taskpolicy_trace = trace.join("taskpolicy");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let taskpolicy_ready = taskpolicy_trace.is_file();
        let cargo_ready =
            fs::read_to_string(&cargo_trace).is_ok_and(|calls| calls.lines().count() == 2);
        if taskpolicy_ready && cargo_ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background prebuild did not produce both traces within 10 seconds"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let priority = fs::read_to_string(taskpolicy_trace).expect("taskpolicy was invoked");
    assert!(priority.starts_with("-c utility sh -c "), "unexpected taskpolicy command: {priority}");
    let cargo = fs::read_to_string(cargo_trace).expect("cargo was invoked");
    let calls: Vec<_> = cargo.lines().collect();
    assert_eq!(calls.len(), 2, "expected build and test prebuild calls: {cargo}");
    assert!(calls[0].starts_with("build --workspace --manifest-path "));
    assert!(calls[1].starts_with("test --workspace --no-run --manifest-path "));
}
