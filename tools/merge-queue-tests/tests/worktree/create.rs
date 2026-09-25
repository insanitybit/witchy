use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::thread;
use std::time::{Duration, Instant};

use super::support::{TempRepo, git, workspace_root};

fn temp_repo() -> TempRepo {
    TempRepo::new("worktree-create", |path| {
        git(path, &["init", "--quiet", "-b", "master"]);
        git(path, &["config", "user.email", "worktree-test@witchy.invalid"]);
        git(path, &["config", "user.name", "Witchy Worktree Test"]);
        fs::write(path.join("README.md"), "fixture\n").expect("write fixture");
        git(path, &["add", "README.md"]);
        git(path, &["commit", "--quiet", "-m", "fixture"]);
        // Build caches and queue state are ignored in the real checkout.
        fs::write(path.join(".git/info/exclude"), "target/\nstate/\n.claude/\n")
            .expect("write fixture excludes");
    })
}

fn create(repo: &Path, name: &str) -> Output {
    create_with(repo, name, &[])
}

fn create_command(repo: &Path, name: &str, env: &[(&str, &str)]) -> Command {
    let script = workspace_root().join("scripts/worktree-create.sh");
    let mut command = Command::new("bash");
    command
        .arg(script)
        .arg(name)
        .current_dir(repo)
        .env("CLAUDE_PROJECT_DIR", repo)
        .env("WITCHY_WORKTREE_CREATE_PREBUILD", "0")
        .env_remove("WITCHY_STATE_DIR")
        .env_remove("WITCHY_WORKTREE_POOL_MAX");
    for (key, value) in env {
        command.env(key, value);
    }
    command
}

fn create_with(repo: &Path, name: &str, env: &[(&str, &str)]) -> Output {
    create_command(repo, name, env).output().expect("run worktree-create.sh")
}

/// What the merge-queue sweep does to a landed pool worktree.
fn release(repo: &Path, worktree: &Path) {
    let scripts = workspace_root().join("scripts");
    let output = Command::new("bash")
        .arg("-c")
        .arg(". \"$0/state-paths.sh\" && . \"$0/worktree-pool.sh\" && worktree_pool_release \"$1\" \"$2\"")
        .arg(&scripts)
        .arg(repo)
        .arg(worktree)
        .env_remove("WITCHY_STATE_DIR")
        .output()
        .expect("run worktree_pool_release");
    assert!(
        output.status.success(),
        "pool release failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn pool_slot(repo: &Path, slot: &str) -> PathBuf {
    repo.join(".claude/worktrees").join(format!("pool-{slot}"))
}

/// Stand in for the incremental state an earlier task left in the slot.
fn plant_incremental_state(worktree: &Path) -> PathBuf {
    let state = worktree.join("target/debug/incremental/witchy_types-fixture/s-1");
    fs::create_dir_all(state.parent().expect("state parent")).expect("create target dir");
    fs::write(&state, "incremental\n").expect("write incremental state");
    state
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
    let repo = temp_repo();
    let root = repo.path();
    let journal = root.join("state/merge-queue/journal.jsonl");
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

#[test]
fn worktree_create_does_not_speculatively_prebuild_by_default() {
    let repo = temp_repo();
    let root = repo.path();
    let script = workspace_root().join("scripts/worktree-create.sh");
    let output = Command::new("bash")
        .arg(script)
        .arg("no-prebuild")
        .current_dir(root)
        .env("CLAUDE_PROJECT_DIR", root)
        .env_remove("WITCHY_WORKTREE_CREATE_PREBUILD")
        .output()
        .expect("create worktree without speculative prebuild");
    created_path(&output);
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("background workspace prebuild started"),
        "worktree creation started an unrequested full-workspace build",
    );
}

#[cfg(unix)]
#[test]
fn worktree_create_runs_background_prebuild_at_utility_priority() {
    let repo = temp_repo();
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
        "#!/bin/sh\nprintf 'fallback\\n' >> \"$WITCHY_PREBUILD_TRACE/fallback-cargo\"\n",
    );
    let shim = root.join("scripts/bin/cargo");
    fs::create_dir_all(shim.parent().expect("shim parent")).expect("create shim directory");
    write_executable(
        &shim,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$WITCHY_PREBUILD_TRACE/cargo\"\n",
    );
    git(root, &["add", "scripts/bin/cargo"]);
    git(root, &["commit", "--quiet", "-m", "fixture cargo shim"]);

    let script = workspace_root().join("scripts/worktree-create.sh");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
    let output = Command::new("bash")
        .arg(script)
        .arg("priority")
        .current_dir(root)
        .env("CLAUDE_PROJECT_DIR", root)
        .env("WITCHY_PREBUILD_TRACE", &trace)
        .env("WITCHY_WORKTREE_CREATE_PREBUILD", "1")
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
    assert!(
        priority.contains("/scripts/bin/cargo"),
        "prebuild did not select the new worktree's cargo shim: {priority}",
    );
    assert!(
        !trace.join("fallback-cargo").exists(),
        "prebuild bypassed the repository cargo shim",
    );
    let cargo = fs::read_to_string(cargo_trace).expect("cargo was invoked");
    let calls: Vec<_> = cargo.lines().collect();
    assert_eq!(calls.len(), 2, "expected build and test prebuild calls: {cargo}");
    assert!(calls[0].starts_with("build --workspace --manifest-path "));
    assert!(calls[1].starts_with("test --workspace --no-run --manifest-path "));
}

#[test]
fn worktree_create_recycles_a_released_pool_slot_with_its_build_state() {
    let repo = temp_repo();
    let root = repo.path();
    let first = created_path(&create(root, "first"));
    assert_eq!(first, pool_slot(root, "00"), "new worktrees are created at pool paths");
    let state = plant_incremental_state(&first);
    release(root, &first);
    assert_eq!(git(&first, &["rev-parse", "--abbrev-ref", "HEAD"]), "HEAD");

    let output = create(root, "second");
    let second = created_path(&output);
    assert_eq!(second, first, "a free slot was not handed out");
    assert_eq!(git(&second, &["branch", "--show-current"]), "worktree-second");
    assert!(state.is_file(), "recycling discarded the slot's incremental state");
    assert!(String::from_utf8_lossy(&output.stderr).contains("recycled pool worktree"));

    let third = created_path(&create(root, "third"));
    assert_eq!(third, pool_slot(root, "01"), "a claimed slot was handed out twice");
}

/// `--branch` gives agents that name their own branches (Codex tasks) a pool
/// slot and its build state under exactly that branch, and refuses a name that
/// is taken instead of renaming it.
#[test]
fn worktree_create_with_an_explicit_branch_claims_a_pool_slot() {
    let repo = temp_repo();
    let root = repo.path();
    let first = created_path(&create(root, "first"));
    let state = plant_incremental_state(&first);
    release(root, &first);

    let script = workspace_root().join("scripts/worktree-create.sh");
    let run = |args: &[&str]| {
        Command::new("bash")
            .arg(&script)
            .args(args)
            .current_dir(root)
            .env("CLAUDE_PROJECT_DIR", root)
            .env("WITCHY_WORKTREE_CREATE_PREBUILD", "0")
            .env_remove("WITCHY_STATE_DIR")
            .env_remove("WITCHY_WORKTREE_POOL_MAX")
            .output()
            .expect("run worktree-create.sh --branch")
    };
    let output = run(&["--branch", "rfc0167/typed-thing"]);
    let claimed = created_path(&output);
    assert_eq!(claimed, first, "an explicit branch did not get the free pool slot");
    assert_eq!(git(&claimed, &["branch", "--show-current"]), "rfc0167/typed-thing");
    assert!(state.is_file(), "recycling discarded the slot's incremental state");

    let taken = run(&["--branch", "rfc0167/typed-thing"]);
    assert!(!taken.status.success(), "a taken branch name must be refused");
    assert!(String::from_utf8_lossy(&taken.stderr).contains("already exists"));
}

#[test]
fn worktree_create_past_the_pool_cap_makes_a_named_worktree() {
    let repo = temp_repo();
    let root = repo.path();
    let cap = [("WITCHY_WORKTREE_POOL_MAX", "1")];
    assert_eq!(created_path(&create_with(root, "first", &cap)), pool_slot(root, "00"));
    assert_eq!(
        created_path(&create_with(root, "overflow", &cap)),
        root.join(".claude/worktrees/overflow"),
    );
    let off = [("WITCHY_WORKTREE_POOL_MAX", "0")];
    assert_eq!(
        created_path(&create_with(root, "unpooled", &off)),
        root.join(".claude/worktrees/unpooled"),
    );
    let reserved = create(root, "pool-07");
    assert!(!reserved.status.success(), "a caller claimed a pool path by name");
}

#[test]
fn worktree_create_evicts_a_dirty_pool_slot_instead_of_handing_it_out() {
    let repo = temp_repo();
    let root = repo.path();
    let first = created_path(&create(root, "first"));
    let state = plant_incremental_state(&first);
    release(root, &first);
    fs::write(first.join("stray.txt"), "left behind\n").expect("dirty the free slot");

    let output = create(root, "second");
    let second = created_path(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("evicting slot 00"));
    assert!(!second.join("stray.txt").exists(), "a dirty slot was handed out");
    assert!(!state.exists(), "the evicted slot's cache survived");
    assert_eq!(git(&second, &["branch", "--show-current"]), "worktree-second");
}

#[test]
fn concurrent_worktree_creates_never_share_a_pool_slot() {
    let repo = temp_repo();
    let root = repo.path();
    for name in ["seed-a", "seed-b"] {
        let path = created_path(&create(root, name));
        plant_incremental_state(&path);
        release(root, &path);
    }
    let children: Vec<_> = (0..5)
        .map(|index| {
            create_command(root, &format!("racer-{index}"), &[])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn concurrent worktree-create.sh")
        })
        .collect();
    let mut paths: Vec<PathBuf> = children
        .into_iter()
        .map(|child| created_path(&child.wait_with_output().expect("wait for create")))
        .collect();
    paths.sort();
    let before = paths.len();
    paths.dedup();
    assert_eq!(paths.len(), before, "two creates were handed the same worktree: {paths:?}");
}
