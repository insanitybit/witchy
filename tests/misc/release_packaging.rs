//! Release-packaging proofs: deterministic and fail-closed before native smoke tests run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const REPO: &str = env!("CARGO_MANIFEST_DIR");
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const TARGET: &str = "x86_64-unknown-linux-gnu";
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "witchy-release-packaging-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
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

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn fake_binary(scratch: &Scratch, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let binary = scratch.join("witchy");
    std::fs::write(&binary, format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n")).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    binary
}

fn package(_scratch: &Scratch, binary: &Path, output: &Path) -> Output {
    run(
        Command::new(Path::new(REPO).join("scripts/release-package.sh"))
            .arg("--binary")
            .arg(binary)
            .arg("--target")
            .arg(TARGET)
            .arg("--commit")
            .arg(COMMIT)
            .arg("--output-dir")
            .arg(output)
            .arg("--source-date-epoch")
            .arg("1720000000")
            .current_dir(REPO),
    )
}

#[test]
#[cfg(unix)]
fn package_is_deterministic_exact_and_checksummed() {
    let scratch = Scratch::new();
    let binary = fake_binary(&scratch, &format!("witchy 0.1.0 (commit {COMMIT})"));
    let first = scratch.join("first");
    let second = scratch.join("second");
    let one = package(&scratch, &binary, &first);
    assert!(one.status.success(), "{}", stderr(&one));
    let two = package(&scratch, &binary, &second);
    assert!(two.status.success(), "{}", stderr(&two));

    let name = format!("witchy-0.1.0-{TARGET}.tar.gz");
    let first_archive = first.join(&name);
    let second_archive = second.join(&name);
    assert_eq!(std::fs::read(&first_archive).unwrap(), std::fs::read(&second_archive).unwrap());

    let listing = run(Command::new("tar").args(["-tzf"]).arg(&first_archive));
    assert!(listing.status.success(), "{}", stderr(&listing));
    let root = format!("witchy-0.1.0-{TARGET}");
    assert_eq!(
        String::from_utf8(listing.stdout).unwrap(),
        format!(
            "{root}/\n{root}/bin/\n{root}/bin/witchy\n{root}/README.md\n{root}/CHANGELOG.md\n{root}/LICENSE-MIT\n{root}/LICENSE-APACHE\n"
        )
    );

    let sums = first.join("SHA256SUMS");
    let checksummed = run(
        Command::new(Path::new(REPO).join("scripts/release-checksums.sh"))
            .arg("--output")
            .arg(&sums)
            .arg(&first_archive)
            .current_dir(REPO),
    );
    assert!(checksummed.status.success(), "{}", stderr(&checksummed));
    let manifest = std::fs::read_to_string(sums).unwrap();
    assert!(manifest.ends_with(&format!("  {name}\n")), "{manifest}");
}

#[test]
#[cfg(unix)]
fn package_rejects_wrong_name_version_and_symlink() {
    use std::os::unix::fs::symlink;

    let scratch = Scratch::new();
    let good = fake_binary(&scratch, &format!("witchy 0.1.0 (commit {COMMIT})"));
    let wrong_name = scratch.join("not-witchy");
    std::fs::copy(&good, &wrong_name).unwrap();
    let rejected_name = package(&scratch, &wrong_name, &scratch.join("name-out"));
    assert!(!rejected_name.status.success());
    assert!(stderr(&rejected_name).contains("named exactly 'witchy'"));

    let wrong = scratch.join("wrong/witchy");
    std::fs::create_dir_all(wrong.parent().unwrap()).unwrap();
    std::fs::write(&wrong, "#!/bin/sh\nprintf '%s\\n' 'witchy 0.1.0'\n").unwrap();
    let permissions = std::fs::metadata(&good).unwrap().permissions();
    std::fs::set_permissions(&wrong, permissions).unwrap();
    let rejected_version = package(&scratch, &wrong, &scratch.join("version-out"));
    assert!(!rejected_version.status.success());
    assert!(stderr(&rejected_version).contains("provenance mismatch"));

    let link_dir = scratch.join("link");
    std::fs::create_dir_all(&link_dir).unwrap();
    let link = link_dir.join("witchy");
    symlink(&good, &link).unwrap();
    let rejected_link = package(&scratch, &link, &scratch.join("link-out"));
    assert!(!rejected_link.status.success());
    assert!(stderr(&rejected_link).contains("not a symlink"));
}
