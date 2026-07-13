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
        let path = std::env::temp_dir().join(format!("witchy-worktree-status-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create temp repo");
        git(&path, &["init", "--quiet"]);
        git(&path, &["symbolic-ref", "HEAD", "refs/heads/master"]);
        git(&path, &["config", "user.email", "worktree-test@witchy.invalid"]);
        git(&path, &["config", "user.name", "Witchy Worktree Test"]);
        write(&path, "README.md", "fixture\n");
        git(&path, &["add", "README.md"]);
        git(&path, &["commit", "--quiet", "-m", "initial"]);
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

fn write(repo: &Path, relative: &str, contents: &str) {
    let path = repo.join(relative);
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
    fs::write(path, contents).expect("write fixture");
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

fn ref_exists(repo: &Path, name: &str) -> bool {
    Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{name}")])
        .current_dir(repo)
        .status()
        .expect("inspect ref")
        .success()
}

fn run_status(repo: &Path) -> Output {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/worktree-status.sh");
    let script = repo.join("scripts/worktree-status.sh");
    fs::create_dir_all(script.parent().expect("script parent")).expect("create scripts dir");
    fs::copy(source, &script).expect("copy worktree-status script");
    Command::new("bash")
        .arg(script)
        .arg("--branches")
        .current_dir(repo)
        .output()
        .expect("run worktree-status.sh")
}

#[test]
fn branch_pruning_is_relative_to_master_and_never_deletes_master() {
    let repo = TempRepo::new();
    let root = repo.path();
    git(root, &["branch", "feature"]);

    write(root, "master.txt", "on master\n");
    git(root, &["add", "master.txt"]);
    git(root, &["commit", "--quiet", "-m", "master work"]);
    git(root, &["branch", "merged-candidate"]);

    git(root, &["checkout", "--quiet", "feature"]);
    write(root, "feature.txt", "on feature\n");
    git(root, &["add", "feature.txt"]);
    git(root, &["commit", "--quiet", "-m", "feature work"]);
    git(root, &["branch", "unmerged-candidate"]);

    let output = run_status(root);
    assert!(
        output.status.success(),
        "worktree-status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(ref_exists(root, "master"), "master must never be pruned");
    assert!(!ref_exists(root, "merged-candidate"), "branch merged into master should be pruned");
    assert!(ref_exists(root, "feature"));
    assert!(ref_exists(root, "unmerged-candidate"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("cleanup: git branch -d master"));
    assert!(stdout.contains("deleted merged-candidate"));
}
