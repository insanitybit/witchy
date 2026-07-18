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

fn run_status(repo: &Path, args: &[&str]) -> Output {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/worktree-status.sh");
    let script = repo.join("scripts/worktree-status.sh");
    fs::create_dir_all(script.parent().expect("script parent")).expect("create scripts dir");
    fs::copy(source, &script).expect("copy worktree-status script");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/state-paths.sh"),
        repo.join("scripts/state-paths.sh"),
    )
    .expect("copy state path resolver");
    Command::new("bash")
        .arg(script)
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run worktree-status.sh")
}

#[test]
fn help_lists_every_dashboard_mode() {
    let repo = TempRepo::new();
    let output = run_status(repo.path(), &["--help"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("worktree-status.sh            # the dashboard"));
    assert!(stdout.contains("worktree-status.sh --disk"));
    assert!(stdout.contains("worktree-status.sh --equivalent"));
    assert!(stdout.contains("worktree-status.sh --prune"));
    assert!(stdout.contains("worktree-status.sh --branches"));
    assert!(!stdout.contains("set -euo pipefail"));
}

#[test]
fn disk_usage_is_opt_in() {
    let repo = TempRepo::new();
    let target = repo.path().join("target");
    fs::create_dir(&target).expect("create target fixture");
    fs::write(target.join("artifact"), vec![0u8; 1024]).expect("write target fixture");

    let regular = run_status(repo.path(), &[]);
    assert!(regular.status.success());
    assert!(!String::from_utf8_lossy(&regular.stdout).contains("[target:"));

    let with_disk = run_status(repo.path(), &["--disk"]);
    assert!(with_disk.status.success());
    assert!(String::from_utf8_lossy(&with_disk.stdout).contains("[target:"));
}

#[test]
fn prunable_worktree_metadata_is_reported_without_aborting_the_dashboard() {
    let repo = TempRepo::new();
    let root = repo.path();
    let stale = root.join(".worktrees/stale");
    git(
        root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "stale-worktree",
            stale.to_str().expect("stale path"),
            "master",
        ],
    );
    fs::remove_dir_all(&stale).expect("remove worktree without pruning metadata");

    let output = run_status(root, &[]);
    assert!(
        output.status.success(),
        "dashboard aborted on prunable metadata: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(stale.to_str().expect("stale path")));
    assert!(stdout.contains("PRUNABLE metadata"));
    assert!(stdout.contains("local branches not checked out anywhere"));
}

#[test]
fn branch_pruning_is_relative_to_master_and_never_deletes_master() {
    let repo = TempRepo::new();
    let root = repo.path();
    let initial = git(root, &["rev-parse", "HEAD"]);
    git(root, &["branch", "feature"]);

    write(root, "master.txt", "on master\n");
    git(root, &["add", "master.txt"]);
    git(root, &["commit", "--quiet", "-m", "master work"]);
    git(root, &["branch", "merged-candidate"]);

    git(root, &["checkout", "--quiet", "-b", "equivalent-candidate", &initial]);
    write(root, "equivalent.txt", "same patch, different history\n");
    git(root, &["add", "equivalent.txt"]);
    git(root, &["commit", "--quiet", "-m", "equivalent patch"]);
    let equivalent_commit = git(root, &["rev-parse", "HEAD"]);

    git(root, &["checkout", "--quiet", "master"]);
    git(root, &["cherry-pick", "--quiet", &equivalent_commit]);

    git(root, &["checkout", "--quiet", "feature"]);
    write(root, "feature.txt", "on feature\n");
    git(root, &["add", "feature.txt"]);
    git(root, &["commit", "--quiet", "-m", "feature work"]);
    git(root, &["branch", "unmerged-candidate"]);

    let regular = run_status(root, &[]);
    assert!(regular.status.success());
    let regular_stdout = String::from_utf8_lossy(&regular.stdout);
    assert!(regular_stdout.contains("equivalent-candidate"));
    assert!(!regular_stdout.contains("patch-equivalent to master"));

    let output = run_status(root, &["--branches", "--equivalent"]);
    assert!(
        output.status.success(),
        "worktree-status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(ref_exists(root, "master"), "master must never be pruned");
    assert!(!ref_exists(root, "merged-candidate"), "branch merged into master should be pruned");
    assert!(
        ref_exists(root, "equivalent-candidate"),
        "patch-equivalent branch must not be auto-deleted without ancestry proof"
    );
    assert!(ref_exists(root, "feature"));
    assert!(ref_exists(root, "unmerged-candidate"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("cleanup: git branch -d master"));
    assert!(stdout.contains("deleted merged-candidate"));
    assert!(stdout.contains("equivalent-candidate"));
    assert!(stdout.contains("patch-equivalent to master"));
}

#[test]
fn worktree_pruning_requires_a_merge_journal_record() {
    let repo = TempRepo::new();
    let root = repo.path();
    let fresh = root.join(".worktrees/fresh");
    let merged = root.join(".worktrees/merged");
    git(
        root,
        &["worktree", "add", "--quiet", "-b", "fresh-worktree", fresh.to_str().expect("fresh path"), "master"],
    );
    git(
        root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "merged-worktree",
            merged.to_str().expect("merged path"),
            "master",
        ],
    );
    let journal = root.join("state/merge-queue/journal.jsonl");
    fs::create_dir_all(journal.parent().expect("journal parent")).expect("create journal parent");
    fs::write(
        journal,
        "{\"event\":\"merged\",\"branch\":\"merged-worktree\",\"sha\":\"fixture\"}\n",
    )
    .expect("write merge journal");

    let output = run_status(root, &["--prune"]);
    assert!(
        output.status.success(),
        "worktree-status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(fresh.is_dir(), "fresh unjournaled worktree must survive pruning");
    assert!(!merged.exists(), "journaled merged worktree should be pruned");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fresh-worktree is not journaled merged"));
    assert!(stdout.contains("REMOVE"));
    assert!(stdout.contains("merged-worktree"));
}

#[test]
fn journaled_patch_equivalent_worktree_can_be_pruned_without_deleting_its_branch() {
    let repo = TempRepo::new();
    let root = repo.path();
    let initial = git(root, &["rev-parse", "HEAD"]);

    git(root, &["checkout", "--quiet", "-b", "equivalent-worktree", &initial]);
    write(root, "equivalent.txt", "landed through a rebase\n");
    git(root, &["add", "equivalent.txt"]);
    git(root, &["commit", "--quiet", "-m", "queued patch"]);
    let queued_commit = git(root, &["rev-parse", "HEAD"]);

    git(root, &["checkout", "--quiet", "master"]);
    write(root, "master.txt", "master moved first\n");
    git(root, &["add", "master.txt"]);
    git(root, &["commit", "--quiet", "-m", "concurrent master work"]);
    git(root, &["cherry-pick", "--quiet", &queued_commit]);

    let worktree = root.join(".worktrees/equivalent");
    git(
        root,
        &["worktree", "add", "--quiet", worktree.to_str().expect("worktree path"), "equivalent-worktree"],
    );
    let journal = root.join("state/merge-queue/journal.jsonl");
    fs::create_dir_all(journal.parent().expect("journal parent")).expect("create journal parent");
    fs::write(
        journal,
        "{\"event\":\"merged\",\"branch\":\"equivalent-worktree\",\"sha\":\"rebased\"}\n",
    )
    .expect("write merge journal");

    let output = run_status(root, &["--prune"]);
    assert!(
        output.status.success(),
        "worktree-status failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!worktree.exists(), "journaled patch-equivalent worktree should be pruned");
    assert!(ref_exists(root, "equivalent-worktree"), "worktree pruning must preserve the branch ref");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("patch-equivalent merge"));
    assert!(stdout.contains("REMOVE"));
    assert!(stdout.contains("equivalent-worktree"));
}
