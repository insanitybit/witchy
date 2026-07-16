//! Dogfood RFC-0092 with a committed application: build `examples/minigrep`,
//! copy the one-file artifact away from its source tree, and run it without a
//! Witchy installation or launch-time grant flags.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const REPO: &str = env!("CARGO_MANIFEST_DIR");
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "witchy-rfc0092-minigrep-{}-{sequence}",
            std::process::id()
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).unwrap();
        }
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.0.join(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(command: &mut Command) -> Output {
    command.output().expect("spawn command")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
#[cfg(unix)]
fn committed_minigrep_builds_and_runs_as_an_installed_trusted_app() {
    let scratch = Scratch::new();
    let project = scratch.join("project");
    std::fs::create_dir_all(project.join("src")).unwrap();
    for file in ["witchy.toml", "src/minigrep.witchy", "src/minigrep_test.witchy"] {
        std::fs::copy(Path::new(REPO).join("examples/minigrep").join(file), project.join(file)).unwrap();
    }

    let built = run(
        Command::new(BIN)
            .current_dir(&scratch.0)
            .args(["--release", "build", "--target", "trusted-exe", "project"]),
    );
    assert!(
        built.status.success(),
        "trusted minigrep build failed\nstdout: {}\nstderr: {}",
        stdout(&built),
        stderr(&built)
    );

    let packaged = project.join("target/release/minigrep");
    assert!(packaged.is_file(), "missing packaged minigrep at {}", packaged.display());
    let installed = scratch.join("bin/minigrep");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::copy(&packaged, &installed).unwrap();
    std::fs::set_permissions(&installed, std::fs::metadata(&packaged).unwrap().permissions()).unwrap();

    // Prove the copied artifact contains everything it needs: remove the
    // compiler input and run with an empty PATH from a separate search root.
    std::fs::remove_dir_all(&project).unwrap();
    let search_root = scratch.join("search-root");
    std::fs::create_dir_all(&search_root).unwrap();
    std::fs::write(
        search_root.join("poem.txt"),
        "Nobody trusts a wrapper.\nTrust the installed artifact.\nnobody needs grant flags.\n",
    )
    .unwrap();

    let sensitive = run(
        Command::new(&installed)
            .current_dir(&search_root)
            .env("PATH", "")
            .env_remove("IGNORE_CASE")
            .args(["nobody", "poem.txt"]),
    );
    assert_eq!(sensitive.status.code(), Some(0), "stderr: {}", stderr(&sensitive));
    assert_eq!(stdout(&sensitive), "nobody needs grant flags.\n");

    let insensitive = run(
        Command::new(&installed)
            .current_dir(&search_root)
            .env("PATH", "")
            .env("IGNORE_CASE", "1")
            .args(["TRUST", "poem.txt"]),
    );
    assert_eq!(insensitive.status.code(), Some(0), "stderr: {}", stderr(&insensitive));
    assert_eq!(
        stdout(&insensitive),
        "Nobody trusts a wrapper.\nTrust the installed artifact.\n"
    );

    // The compiled binding follows launch cwd, not the build directory or the
    // executable's install directory, and remains fail-closed when the file is
    // outside that one read-only root.
    let denied = run(
        Command::new(&installed)
            .current_dir(scratch.join("bin"))
            .env("PATH", "")
            .args(["nobody", "../search-root/poem.txt"]),
    );
    assert!(!denied.status.success());
    assert!(stderr(&denied).contains("`..` escapes the Dir capability"), "{}", stderr(&denied));
}
