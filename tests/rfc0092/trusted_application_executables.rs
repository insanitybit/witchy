//! RFC-0092 trusted application distribution: real native artifact, checked
//! bindings, normal command behavior, and portable-WASM trust separation.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "witchy-rfc0092-{}-{nonce}-{sequence}",
            std::process::id()
        ));
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
fn trusted_executable_end_to_end() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.join("src")).unwrap();
    std::fs::write(scratch.join("inside.txt"), "cwd-data").unwrap();
    std::fs::write(
        scratch.join("witchy.toml"),
        "[rune]\nname = \"trusted_fixture\"\nversion = \"0.1.0\"\n\n\
         [targets.trusted-exe.dirs]\n\
         cwd = { from = \"cwd\" }\n\
         root = { from = \"path\", path = \"/\" }\n\
         [targets.trusted-exe.env]\n\
         env = { from = \"system\", names = [\"RFC0092_TEST_LABEL\"] }\n",
    )
    .unwrap();
    std::fs::write(
        scratch.join("src/trusted_fixture.witchy"),
        r#"import list

fn main(console: Console, cwd: Dir[Read], root: Dir[Read], env: Env, args: List(String)) -> Int:
    let requested = list.at(args, 0)
    if requested.starts_with("/"):
        console.print("absolute:${root.read(requested.strip_prefix("/"))}")
    else:
        console.print("relative:${cwd.read(requested)}")
    match env.get_env("RFC0092_TEST_LABEL"):
        Some(label) -> console.print("env:${label}")
        None -> console.print("env:missing")
    console.print("argv:${list.join(args, "|")}")
    23
"#,
    )
    .unwrap();

    // The documented project command chooses target/release/<rune-name> and
    // produces one executable artifact.
    let built = run(
        Command::new(BIN)
            .current_dir(&scratch.0)
            .args(["--release", "build", "--target", "trusted-exe"]),
    );
    assert!(
        built.status.success(),
        "trusted build failed\nstdout: {}\nstderr: {}",
        stdout(&built),
        stderr(&built)
    );
    let executable = scratch.join("target/release/trusted_fixture");
    assert!(executable.is_file(), "missing native executable: {}", executable.display());

    // Simulate ordinary installation: the artifact is copied away from its
    // project and runs with no Witchy executable discoverable through PATH.
    std::fs::create_dir_all(scratch.join("installed/bin")).unwrap();
    let installed = scratch.join("installed/bin/trusted_fixture");
    std::fs::copy(&executable, &installed).unwrap();
    let relative = run(
        Command::new(&installed)
            .current_dir(&scratch.0)
            .env("PATH", "")
            .env("RFC0092_TEST_LABEL", "inherited")
            .args(["inside.txt", "--dir", "belongs-to-app"]),
    );
    assert_eq!(relative.status.code(), Some(23), "stderr: {}", stderr(&relative));
    assert_eq!(
        stdout(&relative),
        "relative:cwd-data\nenv:inherited\nargv:inside.txt|--dir|belongs-to-app\n"
    );

    // The app—not the launcher—routes an absolute OS argument through the
    // separately root-bound Dir after converting it to Dir-relative syntax.
    let absolute_file = scratch.join("absolute.txt");
    std::fs::write(&absolute_file, "root-data").unwrap();
    let absolute = run(
        Command::new(&installed)
            .current_dir(&scratch.0)
            .arg(absolute_file.to_str().unwrap()),
    );
    assert_eq!(absolute.status.code(), Some(23), "stderr: {}", stderr(&absolute));
    assert!(stdout(&absolute).starts_with("absolute:root-data\n"), "{}", stdout(&absolute));

    let absolute_guest = format!("/{}", absolute_file.display());
    let rejected_absolute = run(
        Command::new(&installed)
            .current_dir(&scratch.0)
            .arg(absolute_guest),
    );
    assert!(!rejected_absolute.status.success());
    assert!(
        stderr(&rejected_absolute).contains("absolute paths are not allowed"),
        "{}",
        stderr(&rejected_absolute)
    );

    let escaped = run(Command::new(&installed).current_dir(&scratch.0).arg("../outside.txt"));
    assert!(!escaped.status.success());
    assert!(stderr(&escaped).contains("`..` escapes the Dir capability"), "{}", stderr(&escaped));

    // Corruption is diagnosed before main can print its marker.
    let corrupt = scratch.join("installed/bin/corrupt");
    let mut bytes = std::fs::read(&installed).unwrap();
    let payload_byte = bytes.len() - 185;
    bytes[payload_byte] ^= 1;
    std::fs::write(&corrupt, bytes).unwrap();
    let permissions = std::fs::metadata(&installed).unwrap().permissions();
    std::fs::set_permissions(&corrupt, permissions).unwrap();
    let rejected = run(Command::new(&corrupt).current_dir(&scratch.0).arg("inside.txt"));
    assert!(!rejected.status.success());
    assert!(stdout(&rejected).is_empty(), "main ran before validation: {}", stdout(&rejected));
    assert!(stderr(&rejected).contains("digest mismatch"), "{}", stderr(&rejected));

    // The identical source remains a portable consumer-hosted module: without
    // explicit roots it fails closed; with the two consumer grants it runs.
    let portable = scratch.join("portable.wasm");
    let compiled = run(Command::new(BIN).args([
        "compile",
        scratch.join("src/trusted_fixture.witchy").to_str().unwrap(),
        "--out",
        portable.to_str().unwrap(),
    ]));
    assert!(compiled.status.success(), "{}", stderr(&compiled));
    let denied = run(Command::new(BIN).args(["sandbox", portable.to_str().unwrap(), "inside.txt"]));
    assert!(!denied.status.success());
    assert!(stderr(&denied).contains("no subtree was granted"), "{}", stderr(&denied));
    let granted = run(Command::new(BIN).current_dir(&scratch.0).args([
        "sandbox",
        "--dir",
        ".",
        "--dir",
        "/",
        portable.to_str().unwrap(),
        "inside.txt",
    ]));
    assert_eq!(granted.status.code(), Some(23), "stderr: {}", stderr(&granted));
}

#[test]
fn dependency_footprint_does_not_widen_the_trusted_root() {
    let scratch = Scratch::new();
    let manifest = scratch.join("witchy.toml");
    std::fs::write(&manifest, "[rune]\nname = \"root\"\nversion = \"0.1.0\"\n[targets.trusted-exe]\n").unwrap();
    let dependency = scratch.join("dependency.witchy");
    std::fs::write(
        &dependency,
        "pub fn answer() -> Int:\n    42\n\npub fn effectful(root: Dir[Read]) -> String:\n    root.read(\"secret.txt\")\n",
    )
    .unwrap();
    let entry = scratch.join("root.witchy");
    std::fs::write(
        &entry,
        "import dependency\nfn main(console: Console):\n    console.print(\"${dependency.answer()}\")\n",
    )
    .unwrap();
    let executable = scratch.join("root");
    let built = run(Command::new(BIN).args([
        "compile",
        entry.to_str().unwrap(),
        "--dep",
        &format!("dependency={}", dependency.display()),
        "--target",
        "trusted-exe",
        "--manifest",
        manifest.to_str().unwrap(),
        "--out",
        executable.to_str().unwrap(),
    ]));
    assert!(built.status.success(), "{}", stderr(&built));
    let output = run(&mut Command::new(&executable));
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), "42\n");
}

#[test]
fn fixed_file_and_named_secret_providers_resolve_only_at_startup() {
    let scratch = Scratch::new();
    std::fs::write(scratch.join("config.txt"), "fixed-config").unwrap();
    let entry = scratch.join("providers.witchy");
    std::fs::write(
        &entry,
        "import crypto\nimport secretstore\n\nfn main(console: Console, config: File[Read], store: SecretStore):\n    console.print(config.read())\n    console.print(crypto.reveal(secretstore.require(store, \"token\")))\n",
    )
    .unwrap();
    let manifest = scratch.join("witchy.toml");
    std::fs::write(
        &manifest,
        "[rune]\nname = \"providers\"\nversion = \"0.1.0\"\n\n\
         [targets.trusted-exe.files]\n\
         config = { from = \"path\", path = \"config.txt\" }\n\
         [targets.trusted-exe.secrets]\n\
         token = { from = \"env:RFC0092_SECRET\" }\n",
    )
    .unwrap();
    let executable = scratch.join("providers");
    let built = run(Command::new(BIN).args([
        "compile",
        entry.to_str().unwrap(),
        "--target",
        "trusted-exe",
        "--manifest",
        manifest.to_str().unwrap(),
        "--out",
        executable.to_str().unwrap(),
    ]));
    assert!(built.status.success(), "{}", stderr(&built));
    let resolved = run(
        Command::new(&executable)
            .current_dir(&scratch.0)
            .env("RFC0092_SECRET", "runtime-only"),
    );
    assert!(resolved.status.success(), "{}", stderr(&resolved));
    assert_eq!(stdout(&resolved), "fixed-config\nruntime-only\n");

    let missing = run(
        Command::new(&executable)
            .current_dir(&scratch.0)
            .env_remove("RFC0092_SECRET"),
    );
    assert!(!missing.status.success());
    assert!(stdout(&missing).is_empty(), "main ran with a missing provider");
    assert!(stderr(&missing).contains("$RFC0092_SECRET is not set"), "{}", stderr(&missing));

    let incomplete = scratch.join("incomplete.toml");
    std::fs::write(&incomplete, "[rune]\nname = \"providers\"\nversion = \"0.1.0\"\n").unwrap();
    let rejected = run(Command::new(BIN).args([
        "compile",
        entry.to_str().unwrap(),
        "--target",
        "trusted-exe",
        "--manifest",
        incomplete.to_str().unwrap(),
        "--out",
        scratch.join("must-not-exist").to_str().unwrap(),
    ]));
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("missing `[targets.trusted-exe.files].config`"),
        "{}",
        stderr(&rejected)
    );
}

#[test]
#[cfg(unix)]
fn explicit_exec_policy_uses_a_separately_bound_readable_dir() {
    let scratch = Scratch::new();
    let entry = scratch.join("exec_app.witchy");
    std::fs::write(
        &entry,
        "import exec\n\nfn main(console: Console, tools: Dir[Read], runner: Exec):\n    let result = exec.run_args(runner, tools, \"echo\", [\"trusted-child\"])\n    console.print(\"${result.0}:${result.1}\")\n",
    )
    .unwrap();
    let manifest = scratch.join("witchy.toml");
    std::fs::write(
        &manifest,
        "[rune]\nname = \"exec_app\"\nversion = \"0.1.0\"\n\n\
         [targets.trusted-exe.dirs]\n\
         tools = { from = \"path\", path = \"/bin\" }\n\
         [targets.trusted-exe.exec]\n\
         runner = { from = \"allow\", programs = [\"echo\"] }\n",
    )
    .unwrap();
    let executable = scratch.join("exec_app");
    let built = run(Command::new(BIN).args([
        "compile",
        entry.to_str().unwrap(),
        "--target",
        "trusted-exe",
        "--manifest",
        manifest.to_str().unwrap(),
        "--out",
        executable.to_str().unwrap(),
    ]));
    assert!(built.status.success(), "{}", stderr(&built));
    let output = run(&mut Command::new(&executable));
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(stdout(&output).contains("0:trusted-child"), "{}", stdout(&output));
}
