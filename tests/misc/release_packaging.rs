//! Release-packaging proofs: deterministic and fail-closed before native smoke tests run.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

const REPO: &str = env!("CARGO_MANIFEST_DIR");
const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const TARGET: &str = "x86_64-unknown-linux-gnu";
const VERSION: &str = "0.2.7";
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
            .arg("--version")
            .arg(VERSION)
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
    let binary = fake_binary(&scratch, &format!("witchy {VERSION} (commit {COMMIT})"));
    let first = scratch.join("first");
    let second = scratch.join("second");
    let one = package(&scratch, &binary, &first);
    assert!(one.status.success(), "{}", stderr(&one));
    let two = package(&scratch, &binary, &second);
    assert!(two.status.success(), "{}", stderr(&two));

    let name = format!("witchy-{VERSION}-{TARGET}.tar.gz");
    let first_archive = first.join(&name);
    let second_archive = second.join(&name);
    assert_eq!(std::fs::read(&first_archive).unwrap(), std::fs::read(&second_archive).unwrap());

    let listing = run(Command::new("tar").args(["-tzf"]).arg(&first_archive));
    assert!(listing.status.success(), "{}", stderr(&listing));
    let root = format!("witchy-{VERSION}-{TARGET}");
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
    let good = fake_binary(&scratch, &format!("witchy {VERSION} (commit {COMMIT})"));
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

#[test]
#[cfg(unix)]
fn pinned_binaryen_installer_exposes_the_reviewed_target_identity_and_rejects_substitution() {
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = Scratch::new();
    let shown = run(
        Command::new(Path::new(REPO).join("scripts/pinned-binaryen.py"))
            .args(["show", "--target", TARGET])
            .current_dir(REPO),
    );
    assert!(shown.status.success(), "{}", stderr(&shown));
    let identity = String::from_utf8(shown.stdout).unwrap();
    assert!(identity.contains("\"release\": \"version_131\""), "{identity}");
    assert!(
        identity.contains(
            "\"wasm_opt_sha256\": \"bad4b6524b2c8e4b27b9aa69bde1a4b9a05ec8887c77ef0d34300f5825acd97c\""
        ),
        "{identity}"
    );

    let bundle = scratch.join("binaryen");
    let executable = bundle.join("bin/wasm-opt");
    std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
    std::fs::write(
        &executable,
        "#!/bin/sh\nprintf 'wasm-opt version 131 (version_131)\\n'\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let rejected = run(
        Command::new(Path::new(REPO).join("scripts/pinned-binaryen.py"))
            .args(["verify", "--target", TARGET, "--bundle-root"])
            .arg(&bundle)
            .current_dir(REPO),
    );
    assert!(!rejected.status.success());
    assert!(stderr(&rejected).contains("wasm-opt digest mismatch"));
}

#[test]
#[cfg(unix)]
fn package_contains_the_exact_reviewed_binaryen_bundle() {
    #[cfg(all(target_arch = "aarch64", target_os = "macos"))]
    let target = "aarch64-apple-darwin";
    #[cfg(all(target_arch = "x86_64", target_os = "macos"))]
    let target = "x86_64-apple-darwin";
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    let target = "aarch64-unknown-linux-gnu";
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let target = "x86_64-unknown-linux-gnu";

    let scratch = Scratch::new();
    let binary = fake_binary(&scratch, &format!("witchy {VERSION} (commit {COMMIT})"));
    let Some(bundle) = std::env::var_os("WITCHY_TEST_REVIEWED_BINARYEN_ROOT").map(PathBuf::from)
    else {
        eprintln!(
            "skipping official-bundle packaging: WITCHY_TEST_REVIEWED_BINARYEN_ROOT is unset"
        );
        return;
    };
    let output = scratch.join("dist");
    let packaged = run(
        Command::new(Path::new(REPO).join("scripts/release-package.sh"))
            .args(["--version", VERSION, "--binary"])
            .arg(&binary)
            .args(["--target", target, "--commit", COMMIT, "--output-dir"])
            .arg(&output)
            .args(["--source-date-epoch", "1720000000", "--binaryen-root"])
            .arg(&bundle)
            .current_dir(REPO),
    );
    assert!(packaged.status.success(), "{}", stderr(&packaged));
    let archive = output.join(format!("witchy-{VERSION}-{target}.tar.gz"));
    let listing = run(Command::new("tar").args(["-tzf"]).arg(&archive));
    assert!(listing.status.success(), "{}", stderr(&listing));
    let listing = String::from_utf8(listing.stdout).unwrap();
    assert!(listing.contains("/libexec/witchy-binaryen/bin/wasm-opt\n"));
    assert!(listing.contains("/share/witchy/binaryen-version_131.lock\n"));
    if target.ends_with("apple-darwin") {
        assert!(listing.contains("/libexec/witchy-binaryen/lib/libbinaryen.dylib\n"));
    }
}
