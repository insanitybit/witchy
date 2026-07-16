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
        let path = std::env::temp_dir().join(format!("witchy-spec-freshness-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create temporary repository");
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

fn git(root: &Path, args: &[&str]) -> Output {
    let output = Command::new("git").current_dir(root).args(args).output().expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn check(root: &Path, args: &[&str]) -> Output {
    Command::new("bash")
        .current_dir(root)
        .arg("scripts/check-spec-freshness.sh")
        .args(args)
        .output()
        .expect("run spec freshness check")
}

#[test]
fn spec_freshness_distinguishes_advisory_strict_and_invalid_stamps() {
    let temp = TempRepo::new();
    let root = temp.path();
    fs::create_dir(root.join("scripts")).expect("scripts directory");
    fs::create_dir(root.join("spec")).expect("spec directory");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-spec-freshness.sh"),
        root.join("scripts/check-spec-freshness.sh"),
    )
    .expect("copy freshness script");

    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Witchy Test"]);
    git(root, &["config", "user.email", "witchy-test@example.invalid"]);
    fs::write(root.join("spec/unstamped.md"), "# Unstamped\n\nverified: prose is not frontmatter\n")
        .expect("write unstamped spec");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);

    let base = git(root, &["rev-parse", "--short=8", "HEAD"]);
    let base = String::from_utf8(base.stdout).expect("commit is UTF-8");
    fs::write(
        root.join("spec/stamped.md"),
        format!("---\nverified: {}\n---\n\n# Stamped\n", base.trim()),
    )
    .expect("write stamped spec");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "add stamp"]);

    let advisory = check(root, &["--max-commits", "0"]);
    assert!(
        advisory.status.success(),
        "advisory check: {}",
        String::from_utf8_lossy(&advisory.stderr)
    );
    assert!(String::from_utf8_lossy(&advisory.stdout).contains("1 stale"));

    let strict = check(root, &["--strict", "--max-commits", "0"]);
    assert_eq!(strict.status.code(), Some(1), "strict check: {strict:?}");
    assert!(String::from_utf8_lossy(&strict.stderr).contains("strict age limit exceeded"));

    fs::write(root.join("spec/stamped.md"), "---\nverified: deadbeef\n---\n").expect("write invalid stamp");
    let invalid = check(root, &[]);
    assert_eq!(invalid.status.code(), Some(1), "invalid check: {invalid:?}");
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("does not exist"));
}
