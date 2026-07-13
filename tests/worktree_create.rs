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
