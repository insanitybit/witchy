use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) struct TempRepo(pub(crate) PathBuf);

impl TempRepo {
    pub(crate) fn new(tag: &str, setup: impl FnOnce(&Path)) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("witchy-{tag}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).expect("create temp repo");
        setup(&path);
        Self(path)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn git(repo: &Path, args: &[&str]) -> String {
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
    String::from_utf8(output.stdout)
        .expect("git output is utf8")
        .trim()
        .to_string()
}

pub(crate) fn write(repo: &Path, relative: &str, contents: &str) {
    let path = repo.join(relative);
    fs::create_dir_all(path.parent().expect("fixture parent")).expect("create fixture parent");
    fs::write(path, contents).expect("write fixture");
}
