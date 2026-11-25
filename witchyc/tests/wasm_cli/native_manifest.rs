//! Manifest-bound native executables (docs/native-trusted-executables.md): the embedded launch
//! plan is the whole command line, and every grant is resolved before Witchy code runs.
use super::{Command, Fixture, fs};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    os::unix::fs::symlink,
    path::Path,
    process::{Child, Output},
    time::{Duration, Instant},
};

/// Each program prints 7 first, so an empty stdout means the launch stopped before guest code.
const MARK: &[u8] = b"7\n";

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Each probe publishes its PID after reaching the branch the test exercises.
fn exec_probe_pids(path: &Path, count: usize) -> Vec<u32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            // A complete record ends in a newline. A partial write is not readiness.
            if text.ends_with('\n') {
                let pids: Vec<u32> = text.split_whitespace().map(|pid| pid.parse().unwrap()).collect();
                assert_eq!(pids.len(), count, "{path:?}: {pids:?}");
                assert!(pids.iter().all(|&pid| pid > 1), "{pids:?}");
                return pids;
            }
        }
        assert!(std::time::Instant::now() < deadline, "exec probe never started: {path:?}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// A reaped process or zombie cannot make progress. Poll the recorded processes so
/// successful tests finish as soon as custody has ended.
fn assert_exec_probes_stopped(pids: &[u32]) {
    let list = pids.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let result = Command::new("/bin/ps").args(["-p", &list, "-o", "state="]).output().unwrap();
        assert!(result.stderr.is_empty(), "{result:?}");
        let states = String::from_utf8(result.stdout).unwrap();
        assert!(result.status.success() || (result.status.code() == Some(1) && states.is_empty()), "{result:?}", result = result.status);
        if states.lines().all(|line| line.trim_start().starts_with('Z')) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "exec probes still live ({list}): {states}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn build(fixture: &Fixture, name: &str, source: &str, manifest: &str) -> Output {
    fs::write(fixture.0.join(format!("{name}.wy")), source).unwrap();
    fs::write(fixture.0.join(format!("{name}.grants.toml")), manifest).unwrap();
    fixture.run(&["build", &format!("{name}.wy"), "-o", name, "--target=macos-arm64", &format!("--manifest={name}.grants.toml")])
}

fn run(fixture: &Fixture, name: &str, args: &[&str]) -> Output {
    Command::new(fixture.0.join(name)).current_dir(&fixture.0).args(args).output().unwrap()
}

pub(super) fn await_loopback_listener(server: &mut Child, log: &Path, timeout: Duration) -> SocketAddr {
    let deadline = Instant::now() + timeout;
    loop {
        let diagnostics = fs::read_to_string(log).unwrap();
        if let Some(endpoint) = diagnostics.lines().find_map(|line| {
            line.strip_prefix("witchy: listening on ")?.parse::<SocketAddr>().ok()
        }) {
            assert!(endpoint.ip().is_loopback() && endpoint.port() != 0, "{endpoint}");
            return endpoint;
        }
        if let Some(status) = server.try_wait().unwrap() {
            panic!("{log:?}: server exited before listening: {status}: {diagnostics}");
        }
        if Instant::now() >= deadline {
            let _ = server.kill();
            let _ = server.wait();
            panic!("{log:?}: server did not report a listener: {diagnostics}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn refused(output: &Output, why: &str) {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "guest code ran: {output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(why), "expected {why:?}: {stderr}");
}

/// The program opens `a.txt` in its directory and prints the status and first byte.
fn reader(ty: &str) -> String {
    let mut source = format!("fn main(project: {ty}, out: Console<Write>):\n    out.print_int(7)\n");
    for byte in b"a.txt" {
        source.push_str(&format!("    project.raw_path_byte(U8.truncate({byte}))\n"));
    }
    source.push_str("    let file = project.raw_open_file()\n    out.print_int(file.raw_status())\n    out.print_int(file.raw_read_byte())\n    let closed = file.raw_close()\n");
    source
}

const PROJECT: &str = "[launch]\nversion = 1\n[flags]\nproject = { type = \"path\", required = true }\n[dirs]\nproject = { root = { from = \"arg\", name = \"project\" }, rights = [\"Read\"]";

#[test]
fn manifest_hello_prints_without_launch_flags() {
    let fixture = Fixture::new();
    let source = "fn main(out: Console<Write>):\n    out.print_int(42)\n";
    let built = build(&fixture, "hello", source, "[launch]\nversion = 1\n");
    assert!(built.status.success(), "{built:?}");
    let hello = run(&fixture, "hello", &[]);
    assert!(hello.status.success(), "{hello:?}");
    assert_eq!(hello.stdout, b"42\n");
    // A development flag is not this executable's: refused, not ignored.
    for flag in ["--output", "--allocator", "--dir=/", "--deadline-ms=5", "positional", "--"] {
        refused(&run(&fixture, "hello", &[flag]), "launch refused");
    }
    // A build without a manifest keeps the development launch.
    let dev = fixture.run(&["build", "hello.wy", "-o", "dev", "--target=macos-arm64"]);
    assert!(dev.status.success(), "{dev:?}");
    assert_eq!(run(&fixture, "dev", &[]).status.code(), Some(2));
    assert_eq!(run(&fixture, "dev", &["--output"]).stdout, b"42\n");
    #[cfg(witchy_native_aws_lc)]
    {
        let libraries = Command::new("/usr/bin/otool")
            .arg("-L").arg(fixture.0.join("hello")).arg(fixture.0.join("dev"))
            .output().unwrap();
        assert!(libraries.status.success(), "{libraries:?}");
        assert!(!String::from_utf8_lossy(&libraries.stdout).contains("libaws_lc_fips"), "{libraries:?}");
    }
    // The review command reports the same checked plan.
    let plan = fixture.run(&["native-plan", "hello.wy", "hello.grants.toml"]);
    assert!(plan.status.success() && String::from_utf8_lossy(&plan.stdout).contains("out: Console<Write>"), "{plan:?}");
}

#[test]
fn manifest_directory_argument_is_read_only_and_link_free() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("notes/sub")).unwrap();
    fs::write(fixture.0.join("notes/a.txt"), b"A").unwrap();
    fs::write(fixture.0.join("notes/sub/a.txt"), b"S").unwrap();
    symlink("notes", fixture.0.join("alias")).unwrap();
    let built = build(&fixture, "notes-tool", &reader("Dir<Read>"), &format!("{PROJECT} }}\n"));
    assert!(built.status.success(), "{built:?}");
    let absolute = fixture.0.canonicalize().unwrap().join("notes");
    for args in [vec!["--project", "./notes"], vec!["--project=notes"], vec!["--project", absolute.to_str().unwrap()], vec!["--project", "notes/sub/.."]] {
        let ok = run(&fixture, "notes-tool", &args);
        assert!(ok.status.success(), "{args:?}: {ok:?}");
        assert_eq!(ok.stdout, b"7\n0\n65\n", "{args:?}");
    }
    assert_eq!(run(&fixture, "notes-tool", &["--project", "notes/sub"]).stdout, b"7\n0\n83\n");
    // Missing, duplicate, unknown, positional, extra authority, a value that is a flag.
    refused(&run(&fixture, "notes-tool", &[]), "--project is required");
    refused(&run(&fixture, "notes-tool", &["--project", "notes", "--project", "notes"]), "given twice");
    refused(&run(&fixture, "notes-tool", &["--projects", "notes"]), "not a declared argument");
    refused(&run(&fixture, "notes-tool", &["--project", "notes", "extra"]), "not a declared");
    refused(&run(&fixture, "notes-tool", &["--project", "notes", "--write-dir=notes"]), "--write-dir is not a declared argument");
    refused(&run(&fixture, "notes-tool", &["--project", "notes", "--", "--dir=/"]), "no words after `--`");
    refused(&run(&fixture, "notes-tool", &["--project", "--output"]), "needs a value");
    refused(&run(&fixture, "notes-tool", &["--project="]), "a path of 1 to 4096 bytes");
    // Links are never followed, at any component; a file is not a directory.
    refused(&run(&fixture, "notes-tool", &["--project", "alias"]), "symbolic link");
    refused(&run(&fixture, "notes-tool", &["--project", "alias/sub"]), "symbolic link");
    refused(&run(&fixture, "notes-tool", &["--project", "notes/a.txt"]), "not a directory");
    refused(&run(&fixture, "notes-tool", &["--project", "missing"]), "does not exist");
    // An unrestricted path argument says so in the reviewed plan.
    let plan = fixture.run(&["native-plan", "notes-tool.wy", "notes-tool.grants.toml"]);
    assert!(String::from_utf8_lossy(&plan.stdout).contains("unrestricted"), "{plan:?}");
}

#[test]
fn manifest_within_keeps_an_argument_under_its_root() {
    let fixture = Fixture::new();
    for dir in ["allowed/notes/deep", "outside"] { fs::create_dir_all(fixture.0.join(dir)).unwrap(); }
    for file in ["allowed/a.txt", "allowed/notes/a.txt", "allowed/notes/deep/a.txt", "outside/a.txt"] { fs::write(fixture.0.join(file), b"A").unwrap(); }
    symlink("../outside", fixture.0.join("allowed/escape")).unwrap();
    let manifest = format!("{PROJECT}, within = {{ from = \"cwd\", path = \"allowed\" }} }}\n");
    let built = build(&fixture, "within", &reader("Dir<Read>"), &manifest);
    assert!(built.status.success(), "{built:?}");
    let root = fixture.0.canonicalize().unwrap();
    for args in [vec!["--project", "allowed/notes"], vec!["--project", "allowed"], vec!["--project", root.join("allowed/notes/deep").to_str().unwrap()]] {
        let ok = run(&fixture, "within", &args);
        assert!(ok.status.success(), "{args:?}: {ok:?}");
    }
    refused(&run(&fixture, "within", &["--project", "outside"]), "outside the directory it is restricted to");
    refused(&run(&fixture, "within", &["--project", "allowed/../outside"]), "`..`");
    refused(&run(&fixture, "within", &["--project", "allowed/escape"]), "symbolic link");
    refused(&run(&fixture, "within", &["--project", "."]), "outside the directory");
    // A path relative to a startup directory already below the root is below it.
    let absolute = format!("{PROJECT}, within = {{ from = \"literal\", value = \"{}\" }} }}\n", root.join("allowed").display());
    let built = build(&fixture, "within-absolute", &reader("Dir<Read>"), &absolute);
    assert!(built.status.success(), "{built:?}");
    let nested = Command::new(fixture.0.join("within-absolute")).current_dir(fixture.0.join("allowed/notes")).args(["--project", "deep"]).output().unwrap();
    assert!(nested.status.success(), "{nested:?}");
    assert_eq!(nested.stdout, b"7\n0\n65\n");

    // A mount below the root is another device: macOS mounts its Preboot volume there.
    let devices = format!("{PROJECT}, within = {{ from = \"literal\", value = \"/System/Volumes\" }} }}\n");
    let built = build(&fixture, "devices", "fn main(project: Dir<Read>, out: Console<Write>):\n    out.print_int(7)\n", &devices);
    assert!(built.status.success(), "{built:?}");
    assert_eq!(run(&fixture, "devices", &["--project", "/System/Volumes"]).stdout, MARK);
    refused(&run(&fixture, "devices", &["--project", "/System/Volumes/Preboot"]), "another device");

    // A restricted file has one link: a hard link inside could name a file outside.
    fs::hard_link(fixture.0.join("outside/a.txt"), fixture.0.join("allowed/linked.txt")).unwrap();
    fs::write(fixture.0.join("allowed/plain.txt"), b"P").unwrap();
    let files = "[launch]\nversion = 1\n[flags]\nconfig = { type = \"path\" }\n[files]\nconfig = { path = { from = \"arg\", name = \"config\" }, within = { from = \"cwd\", path = \"allowed\" } }\n";
    let built = build(&fixture, "config", "fn main(config: File<Read>, out: Console<Write>):\n    out.print_int(7)\n    out.print_int(config.raw_read_byte())\n", files);
    assert!(built.status.success(), "{built:?}");
    assert_eq!(run(&fixture, "config", &["--config", "allowed/plain.txt"]).stdout, b"7\n80\n");
    refused(&run(&fixture, "config", &["--config", "allowed/linked.txt"]), "more than one link");
    refused(&run(&fixture, "config", &["--config", "outside/a.txt"]), "outside the directory");
}

#[test]
fn manifest_sources_resolve_through_one_typed_resolver() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("data/.hidden")).unwrap();
    fs::write(fixture.0.join("data/a.txt"), b"D").unwrap();
    fs::write(fixture.0.join("data/.secret"), b"X").unwrap();
    fs::write(fixture.0.join("input.txt"), b"I").unwrap();
    // cwd with the data-only policy: a dot name is not a data name.
    let mut source = reader("Dir<Read>");
    for byte in b".secret" { source.push_str(&format!("    project.raw_path_byte(U8.truncate({byte}))\n")); }
    source.push_str("    out.print_int(project.raw_probe())\n");
    let cwd = "[launch]\nversion = 1\n[dirs]\nproject = { root = { from = \"cwd\", path = \"data\" }, rights = [\"Read\"], policy = \"data-only\" }\n";
    let built = build(&fixture, "cwd", &source, cwd);
    assert!(built.status.success(), "{built:?}");
    assert_eq!(run(&fixture, "cwd", &[]).stdout, b"7\n0\n68\n0\n");
    let plain = cwd.replace(", policy = \"data-only\"", "");
    let built = build(&fixture, "cwd-plain", &source, &plain);
    assert!(built.status.success(), "{built:?}");
    assert_eq!(run(&fixture, "cwd-plain", &[]).stdout, b"7\n0\n68\n1\n");

    // home comes from the account database, whatever $HOME says.
    let home = "[launch]\nversion = 1\n[dirs]\nproject = { root = { from = \"home\" }, rights = [\"Read\"] }\n";
    let built = build(&fixture, "home", "fn main(project: Dir<Read>, out: Console<Write>):\n    out.print_int(7)\n", home);
    assert!(built.status.success(), "{built:?}");
    let launched = Command::new(fixture.0.join("home")).env("HOME", "/nonexistent").output().unwrap();
    assert_eq!(launched.stdout, MARK, "{launched:?}");
    let missing = home.replace("from = \"home\" }", "from = \"home\", path = \"witchy-absent-for-manifest-test\" }");
    let built = build(&fixture, "home-missing", "fn main(project: Dir<Read>, out: Console<Write>):\n    out.print_int(7)\n", &missing);
    assert!(built.status.success(), "{built:?}");
    refused(&run(&fixture, "home-missing", &[]), "does not exist");

    // An inherited descriptor, checked against the grant's kind, and the launcher's stdin.
    let input = "fn main(input: Console<Read>, out: Console<Write>):\n    out.print_int(7)\n    out.print_int(input.next_byte())\n";
    let built = build(&fixture, "fd", input, "[launch]\nversion = 1\n[input]\ninput = { from = \"fd\", number = 3 }\n");
    assert!(built.status.success(), "{built:?}");
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", script]).output().unwrap();
    assert_eq!(shell("exec ./fd 3<input.txt").stdout, b"7\n73\n");
    refused(&shell("exec ./fd 3<data"), "not a read-only file");
    refused(&shell("exec ./fd 3<>input.txt"), "not a read-only file");
    refused(&shell("exec ./fd 3<&-"), "not an open descriptor");
    let built = build(&fixture, "stdin", input, "[launch]\nversion = 1\n[input]\ninput = { stdin = true }\n");
    assert!(built.status.success(), "{built:?}");
    assert_eq!(shell("exec ./stdin <input.txt").stdout, b"7\n73\n");

    // Env reads only its named variables; Args only the words after `--`.
    let env = "fn main(environment: Env, words: Args, out: Console<Write>):\n    out.print_int(7)\n    out.print_int(words.raw_count())\n";
    let built = build(&fixture, "env", env, "[launch]\nversion = 1\n[env]\nenvironment = [\"LANG\"]\n[args]\nwords = { count = 2, max-bytes = 16 }\n");
    assert!(built.status.success(), "{built:?}");
    assert_eq!(run(&fixture, "env", &["--", "a", "--output"]).stdout, b"7\n2\n");
    refused(&run(&fixture, "env", &["--", "a", "b", "c"]), "words after `--` are at most 2");
    refused(&run(&fixture, "env", &["a"]), "not a declared");
}

#[test]
fn manifest_changes_cannot_replace_the_prior_executable() {
    let fixture = Fixture::new();
    let source = "fn main(out: Console<Write>):\n    out.print_int(42)\n";
    assert!(build(&fixture, "app", source, "[launch]\nversion = 1\n").status.success());
    let published = fs::read(fixture.0.join("app")).unwrap();
    // An extra grant, an unknown section, an unsupported version: refused, output intact.
    for manifest in ["[launch]\nversion = 1\n[clock]\nclock = true\n", "[launch]\nversion = 1\n[magic]\nx = 1\n", "[launch]\nversion = 2\n", ""] {
        let rebuilt = build(&fixture, "app", source, manifest);
        assert!(!rebuilt.status.success(), "{manifest}: {rebuilt:?}");
        assert_eq!(fs::read(fixture.0.join("app")).unwrap(), published, "{manifest}");
    }
    // The manifest is not the output.
    fs::write(fixture.0.join("app.grants.toml"), "[launch]\nversion = 1\n").unwrap();
    let same = fixture.run(&["build", "app.wy", "-o", "app.grants.toml", "--target=macos-arm64", "--manifest=app.grants.toml"]);
    assert!(!same.status.success() && fs::read(fixture.0.join("app.grants.toml")).unwrap() == b"[launch]\nversion = 1\n", "{same:?}");
    // A resident session names the manifest's digest with each request.
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_witchyc")).current_dir(&fixture.0)
        .args(["build-session", "app.wy", "-o", "app", "--target=macos-arm64", "--manifest=app.grants.toml"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut requests = child.stdin.take().unwrap();
    let mut replies = BufReader::new(child.stdout.take().unwrap());
    let manifest = b"[launch]\nversion = 1\n";
    let source = "fn main(out: Console<Write>):\n    out.print_int(43)\n";
    fs::write(fixture.0.join("app.wy"), source).unwrap();
    writeln!(requests, "build one {} {}", digest(source.as_bytes()), digest(manifest)).unwrap();
    let mut reply = String::new();
    replies.read_line(&mut reply).unwrap();
    assert!(reply.contains("witchy.native-build.v1"), "{reply}");
    assert_eq!(run(&fixture, "app", &[]).stdout, b"43\n");
    let published = fs::read(fixture.0.join("app")).unwrap();
    // The manifest changed after the request was made for it: refused, output intact.
    fs::write(fixture.0.join("app.grants.toml"), "[launch]\nversion = 1\n# changed\n").unwrap();
    writeln!(requests, "build two {} {}", digest(source.as_bytes()), digest(manifest)).unwrap();
    drop(requests);
    assert!(!child.wait().unwrap().success());
    assert_eq!(fs::read(fixture.0.join("app")).unwrap(), published);
}

#[cfg(witchy_native_aws_lc)]
#[test]
fn manifest_signer_takes_its_seed_from_a_declared_descriptor() {
    let fixture = Fixture::new();
    let source = "fn main(out: Console<Write>, targets: Signer):\n    out.print_int(7)\n    var short: [U8; 1] = [0]\n    out.print_int(targets.raw_public_key(short.slice_mut()))\n";
    let manifest = "[launch]\nversion = 1\n[flags]\nkey-fd = { type = \"fd\", secret = true }\n[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = { from = \"arg\", name = \"key-fd\" } }\n";
    let built = build(&fixture, "signer", source, manifest);
    assert!(built.status.success(), "{built:?}");
    fs::write(fixture.0.join("seed"), [1; 32]).unwrap();
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", script]).output().unwrap();
    assert_eq!(shell("exec ./signer --key-fd 3 3<seed").stdout, b"7\n2\n");
    refused(&shell("exec ./signer --key-fd 4 3<seed 4<&-"), "not an open descriptor");
    refused(&shell("exec ./signer --key-fd 3 --secret-slot=targets:sign/witchy-2/targets:fd:3 3<seed"), "not a declared argument");
    refused(&shell("exec ./signer 3<seed"), "--key-fd is required");
    // The descriptor number is never echoed as a value; a short seed is refused.
    fs::write(fixture.0.join("short"), [1; 31]).unwrap();
    refused(&shell("exec ./signer --key-fd=3 3<short"), "could not be bound");
}

/// A read file and an input are fixed objects opened before entry: beside a secret, each is
/// compared with the secrets' sources by device and inode, in either launch mode.
#[cfg(witchy_native_aws_lc)]
#[test]
fn manifest_secret_sources_are_no_file_or_input_grant() {
    let fixture = Fixture::new();
    let source = "fn main(out: Console<Write>, token: Secret<Reveal>, config: File<Read>, input: Console<Read>):\n    out.print_int(7)\n    out.print_int(token.raw_byte(0))\n    out.print_int(config.raw_read_byte())\n    out.print_int(input.next_byte())\n";
    let manifest = "[launch]\nversion = 1\n[flags]\nconfig = { type = \"path\" }\ntoken-fd = { type = \"fd\", secret = true }\n[secrets]\ntoken = { use = \"reveal-bytes\", from = { from = \"arg\", name = \"token-fd\" } }\n[files]\nconfig = { from = \"arg\", name = \"config\" }\n[input]\ninput = { stdin = true }\n";
    let built = build(&fixture, "tool", source, manifest);
    assert!(built.status.success(), "{built:?}");
    fs::write(fixture.0.join("key"), b"K").unwrap();
    fs::write(fixture.0.join("config"), b"C").unwrap();
    fs::write(fixture.0.join("input"), b"I").unwrap();
    fs::hard_link(fixture.0.join("key"), fixture.0.join("key-link")).unwrap();
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", script]).output().unwrap();
    assert_eq!(shell("exec ./tool --config config --token-fd 3 3<key <input").stdout, b"7\n75\n67\n73\n");
    refused(&shell("exec ./tool --config key --token-fd 3 3<key <input"), "no secret's source");
    refused(&shell("exec ./tool --config key-link --token-fd 3 3<key <input"), "no secret's source");
    refused(&shell("exec ./tool --config config --token-fd 3 3<key <key-link"), "a secret's source");
    // The development launch applies the same rule.
    let dev = fixture.run(&["build", "tool.wy", "-o", "dev", "--target=macos-arm64"]);
    assert!(dev.status.success(), "{dev:?}");
    let flags = "--output --input-stdin --secret-slot=token:reveal-bytes:fd:3";
    assert_eq!(shell(&format!("exec ./dev {flags} --file=config 3<key <input")).stdout, b"7\n75\n67\n73\n");
    refused(&shell(&format!("exec ./dev {flags} --file=key-link 3<key <input")), "File<Read>");
    refused(&shell(&format!("exec ./dev {flags} --file=config 3<key <key")), "a secret's source");
}

/// The resident compiler builds a manifest-bound executable for a request naming the
/// compiler inputs' key and the manifest's digest; a request made for other inputs or another
/// manifest leaves the prior executable intact.
#[test]
fn manifest_compiler_session_binds_the_request_to_its_manifest() {
    let fixture = Fixture::new();
    let session = super::CompilerSession::start(&fixture.0, |_| {});
    // Left by an earlier compiler whose PID was reused.
    let stale = fixture.0.join(format!(".witchy-native-{}-0", session.id()));
    fs::create_dir(&stale).unwrap();
    fs::write(stale.join("program.o"), b"prior staging").unwrap();
    let request = |args: &[&str]| session.request(args);
    let manifest = "[launch]\nversion = 1\n";
    fs::write(fixture.0.join("main.wy"), "fn main(out: Console<Write>):\n    out.print_int(44)\n").unwrap();
    fs::write(fixture.0.join("main.grants.toml"), manifest).unwrap();
    let (status, key) = request(&["input-key", "main.wy"]);
    assert_eq!(status, 0, "{key:?}");
    let key = key[0].trim().to_string();
    let (status, built) = request(&["build-native", "main.wy", "app", "main.grants.toml", &key, &digest(manifest.as_bytes())]);
    assert_eq!(status, 0, "{built:?}");
    assert_eq!(run(&fixture, "app", &[]).stdout, b"44\n");
    assert_eq!(fs::read(stale.join("program.o")).unwrap(), b"prior staging");
    assert!(!fixture.0.join(format!(".witchy-native-{}-1", session.id())).exists());
    let published = fs::read(fixture.0.join("app")).unwrap();
    // Another manifest than the request's, then other inputs than the key's.
    let (status, _) = request(&["build-native", "main.wy", "app", "main.grants.toml", &key, &digest(b"[launch]\nversion = 1\n# other\n")]);
    assert_eq!(status, 1);
    fs::write(fixture.0.join("main.wy"), "fn main(out: Console<Write>):\n    out.print_int(45)\n").unwrap();
    let (status, refused) = request(&["build-native", "main.wy", "app", "main.grants.toml", &key, &digest(manifest.as_bytes())]);
    assert_eq!(status, 1);
    assert!(refused[1].contains("compiler inputs changed"), "{refused:?}");
    assert_eq!(fs::read(fixture.0.join("app")).unwrap(), published);
    session.finish();
}

/// Review of 5ab1de72: a descriptor argument named a descriptor the launch had itself opened
/// (the read-only root, the startup directory), turning it into a writable grant. Only
/// descriptors open before the launch opened anything are inherited.
#[test]
fn manifest_descriptor_sources_are_only_inherited_ones() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("cfg")).unwrap();
    fs::create_dir_all(fixture.0.join("data")).unwrap();
    let root = fixture.0.canonicalize().unwrap();
    let mut source = String::from("fn main(cfg: Dir<Read>, out: Dir<Write>, console: Console<Write>):\n    console.print_int(7)\n");
    for byte in b"pwned" { source.push_str(&format!("    out.raw_path_byte(U8.truncate({byte}))\n")); }
    source.push_str("    console.print_int(out.raw_make_dir())\n");
    let manifest = format!("[launch]\nversion = 1\n[flags]\nout-fd = {{ type = \"fd\" }}\n[dirs]\ncfg = {{ root = \"{}\", rights = [\"Read\"] }}\nout = {{ root = {{ from = \"arg\", name = \"out-fd\" }}, rights = [\"Write\"] }}\n", root.join("cfg").display());
    let built = build(&fixture, "alias", &source, &manifest);
    assert!(built.status.success(), "{built:?}");
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", script]).output().unwrap();
    for number in ["3", "4", "5"] {
        // Closed by the shell itself: on macOS a pipe another test thread is creating can be
        // inherited before it is marked close-on-exec.
        refused(&shell(&format!("exec 3>&- 4>&- 5>&-; exec ./alias --out-fd {number}")), "not an open descriptor");
    }
    assert!(!fixture.0.join("cfg/pwned").exists());
    // A descriptor the launcher passes still works.
    let ok = shell("exec ./alias --out-fd 3 3<data");
    assert_eq!(ok.stdout, b"7\n0\n", "{ok:?}");
    assert!(fixture.0.join("data/pwned").is_dir() && !fixture.0.join("cfg/pwned").exists());
}

/// Scan of 34f2764c: descriptor numbers were checked against an inherited set and read again
/// later, after the slot binder had closed one and startup could reuse it. Each source's
/// descriptor is now copied once before anything is opened, one number reaches one source, and
/// a number the launcher did not pass is refused before anything is bound.
#[cfg(witchy_native_aws_lc)]
#[test]
fn manifest_descriptor_numbers_are_copied_once_and_never_shared() {
    let fixture = Fixture::new();
    let source = "fn main(out: Console<Write>, targets: Signer, input: Console<Read>):\n    out.print_int(7)\n    var short: [U8; 1] = [0]\n    out.print_int(targets.raw_public_key(short.slice_mut()))\n    out.print_int(input.next_byte())\n";
    let manifest = "[launch]\nversion = 1\n[flags]\nkey-fd = { type = \"fd\", secret = true }\nin-fd = { type = \"fd\" }\n[secrets]\ntargets = { use = \"sign/witchy-2/targets\", from = { from = \"arg\", name = \"key-fd\" } }\n[input]\ninput = { from = \"arg\", name = \"in-fd\" }\n";
    let built = build(&fixture, "tool", source, manifest);
    assert!(built.status.success(), "{built:?}");
    fs::write(fixture.0.join("seed"), [1; 32]).unwrap();
    fs::write(fixture.0.join("input"), b"I").unwrap();
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", script]).output().unwrap();
    assert_eq!(shell("exec ./tool --key-fd 3 --in-fd 4 3<seed 4<input").stdout, b"7\n2\n73\n");
    // One number, two sources.
    refused(&shell("exec ./tool --key-fd 3 --in-fd 3 3<seed"), "another source also names");
    // A number not passed (which the signer's closed copy could otherwise free for reuse).
    for number in ["4", "5", "6"] {
        refused(&shell(&format!("exec ./tool --key-fd 3 --in-fd {number} 3<seed {number}<&-")), "not an open descriptor");
    }
}

/// The granted directory contains a plain file and a hard link to a file outside it.
fn hard_link_fixture() -> Fixture {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("root")).unwrap();
    fs::write(fixture.0.join("outside"), b"Z").unwrap();
    fs::write(fixture.0.join("root/plain"), b"P").unwrap();
    fs::hard_link(fixture.0.join("outside"), fixture.0.join("root/linked")).unwrap();
    fixture
}

#[test]
fn native_directory_operations_refuse_files_with_more_than_one_link() {
    let fixture = hard_link_fixture();
    let path = |source: &mut String, path: &str| for byte in path.bytes() { source.push_str(&format!("    dir.raw_path_byte(U8.truncate({byte}))\n")); };
    let mut source = String::from("fn reads(dir: &Dir<Read>, out: &Console<Write>):\n");
    for (index, name) in ["plain", "linked"].iter().enumerate() {
        path(&mut source, name);
        source.push_str(&format!("    let read{index} = dir.raw_open_file()\n    out.print_int(read{index}.raw_status())\n    let closed{index} = read{index}.raw_close()\n"));
    }
    source.push_str("fn writes(dir: &Dir<Write>, out: &Console<Write>):\n");
    for (index, opener) in ["raw_open_file", "raw_open_append"].iter().enumerate() {
        path(&mut source, "linked");
        source.push_str(&format!("    let write{index} = dir.{opener}()\n    let wrote{index} = write{index}.raw_write_byte(U8.truncate(66))\n    out.print_int(write{index}.raw_close())\n"));
    }
    path(&mut source, "linked");
    source.push_str("    dir.raw_rename_from()\n");
    path(&mut source, "moved");
    source.push_str("    out.print_int(dir.raw_rename())\n");
    path(&mut source, "plain");
    source.push_str("    dir.raw_rename_from()\n");
    path(&mut source, "linked");
    source.push_str("    out.print_int(dir.raw_rename())\n");
    source.push_str("fn main(dir: Dir, out: Console<Write>):\n    out.print_int(7)\n    reads(&dir, &out)\n    writes(&dir, &out)\n");
    fs::write(fixture.0.join("dir.wy"), &source).unwrap();
    let built = fixture.run(&["build", "dir.wy", "-o", "dir", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    // Status 2 is InvalidPath: read plain 0, then linked refused for read, replace, append,
    // as a rename source and as a rename target.
    let expected = b"7\n0\n2\n2\n2\n2\n2\n";
    let dev = Command::new(fixture.0.join("dir")).current_dir(&fixture.0).args(["--output", "--dir=root", "--dir-rights=read,write"]).output().unwrap();
    assert_eq!(dev.stdout, expected, "{dev:?}");
    assert_eq!(fs::read(fixture.0.join("outside")).unwrap(), b"Z");
    let manifest = "[launch]\nversion = 1\n[dirs]\ndir = { root = { from = \"cwd\", path = \"root\" }, rights = [\"Read\", \"Write\"] }\n";
    let built = build(&fixture, "dir-manifest", &source, manifest);
    assert!(built.status.success(), "{built:?}");
    assert_eq!(run(&fixture, "dir-manifest", &[]).stdout, expected);
    assert_eq!(fs::read(fixture.0.join("outside")).unwrap(), b"Z");
    assert!(fixture.0.join("root/linked").exists() && !fixture.0.join("root/moved").exists());
}

#[test]
fn native_file_grants_refuse_files_with_more_than_one_link() {
    let fixture = hard_link_fixture();
    let file = "fn main(config: File<Read>, out: Console<Write>):\n    out.print_int(7)\n    out.print_int(config.raw_read_byte())\n";
    fs::write(fixture.0.join("file.wy"), file).unwrap();
    assert!(fixture.run(&["build", "file.wy", "-o", "file", "--target=macos-arm64"]).status.success());
    assert_eq!(Command::new(fixture.0.join("file")).current_dir(&fixture.0).args(["--output", "--file=root/plain"]).output().unwrap().stdout, b"7\n80\n");
    refused(&Command::new(fixture.0.join("file")).current_dir(&fixture.0).args(["--output", "--file=root/linked"]).output().unwrap(), "one link");
    let built = build(&fixture, "file-manifest", file, "[launch]\nversion = 1\n[files]\nconfig = { from = \"cwd\", path = \"root/linked\" }\n");
    assert!(built.status.success(), "{built:?}");
    refused(&run(&fixture, "file-manifest", &[]), "one link");
}

#[test]
fn native_console_input_refuses_files_with_more_than_one_link() {
    let fixture = hard_link_fixture();
    let input = "fn main(input: Console<Read>, out: Console<Write>):\n    out.print_int(7)\n    out.print_int(input.next_byte())\n";
    fs::write(fixture.0.join("input.wy"), input).unwrap();
    assert!(fixture.run(&["build", "input.wy", "-o", "input", "--target=macos-arm64"]).status.success());
    refused(&Command::new(fixture.0.join("input")).current_dir(&fixture.0).args(["--output", "--input=root/linked"]).output().unwrap(), "--input");
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", script]).output().unwrap();
    refused(&shell("exec ./input --output --input-stdin <root/linked"), "more than one link");
    assert_eq!(shell("exec ./input --output --input-stdin <root/plain").stdout, b"7\n80\n");
}

/// A program over the raw Exec operations (no library import, so no proof checker): each step
/// runs a name, through `exec` or a narrowed handle, and prints the output and exit code, or
/// 100 plus the error code (1 not granted, 2 invalid, 3 too large, 4 not found, 6 timed out).
fn exec_program(steps: &[(&str, &[&str], bool, Option<u32>)]) -> String {
    let mut source = String::from("fn main(out: Console<Write>, exec: Exec):\n    out.print_int(7)\n");
    for (index, (name, args, narrowed, deadline_ms)) in steps.iter().enumerate() {
        let mut receiver = "exec".to_string();
        if *narrowed {
            for byte in name.bytes() { source.push_str(&format!("    exec.raw_name_byte(U8.truncate({byte}))\n")); }
            source.push_str(&format!("    let only{index} = exec.raw_only()\n    out.print_int(only{index}.raw_status())\n"));
            receiver = format!("only{index}");
        }
        for arg in *args {
            for byte in arg.bytes() { source.push_str(&format!("    {receiver}.raw_arg_byte(U8.truncate({byte}))\n")); }
            source.push_str(&format!("    {receiver}.raw_arg_end()\n"));
        }
        for byte in name.bytes() { source.push_str(&format!("    {receiver}.raw_name_byte(U8.truncate({byte}))\n")); }
        if let Some(ms) = deadline_ms {
            source.push_str(&format!("    {receiver}.raw_deadline_ms({ms})\n"));
        }
        source.push_str(&format!("    let run{index} = {receiver}.raw_run()\n    let status{index} = run{index}.raw_status()\n    if status{index} == 0:\n        var byte{index} = run{index}.raw_out_byte()\n        while byte{index} < 256:\n            out.print_byte(U8.truncate(byte{index}))\n            byte{index} = run{index}.raw_out_byte()\n        out.print_int(run{index}.raw_code())\n    else:\n        out.print_int(100 + status{index})\n"));
    }
    source
}

/// Exec through the broker, with the Wasmtime host's exec fixture as the reference
/// (tests/fixtures/host/exec.wy): only the granted names, each the file of that name opened
/// without a link, arguments whole, no environment, no input, no inherited descriptor,
/// stopped with its process group.
/// The Exec builder natively, from a launch plan: the same program and output as the Wasm
/// fixture `host/exec-command.wy` (input, standard error, every narrowing and refusal).
#[test]
fn native_exec_command_builder_narrows_only() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/host");
    fs::create_dir(fixture.0.join("bin")).unwrap();
    let bin = fixture.0.join("bin").canonicalize().unwrap();
    for name in ["answer", "nap"] {
        fs::write(bin.join(name), fs::read_to_string(fixtures.join("exec-command-bin").join(name)).unwrap()).unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    // The library's text and Vec code is proved: this needs the proof checker, as
    // wasm_cli.rs's proof tests do.
    let Some(binding) = std::env::var_os("WITCHY_WASM_TEST_READY_BINDING") else {
        eprintln!("skipped: WITCHY_WASM_TEST_READY_BINDING is not set, and the library's Vec code is proved");
        return;
    };
    let source = fs::read_to_string(fixtures.join("exec-command.wy")).unwrap();
    let expected = fs::read_to_string(fixtures.join("exec-command.expected")).unwrap();
    let manifest = format!("[launch]\nversion = 1\n[exec]\nexec = {{ dir = \"{}\", programs = [\"answer\", \"nap\"] }}\n[limits]\nexec-bytes = 4096\ndeadline-ms = 10000\n", bin.display());
    fs::write(fixture.0.join("command.wy"), &source).unwrap();
    fs::write(fixture.0.join("command.grants.toml"), &manifest).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_witchyc"));
    command.args(["build", "command.wy", "-o", "command", "--target=macos-arm64", "--manifest=command.grants.toml"])
        .current_dir(&fixture.0).env("WITCHY_WASM_READY_BINDING", binding);
    if let Some(lean) = std::env::var_os("WITCHY_WASM_TEST_LEAN_ROOT") { command.env("WITCHY_WASM_LEAN_ROOT", lean); }
    let turn = super::share_library_proofs(&mut command);
    let built = command.output().unwrap();
    drop(turn);
    assert!(built.status.success(), "{built:?}");
    let ran = run(&fixture, "command", &[]);
    assert_eq!(String::from_utf8_lossy(&ran.stdout), expected, "{ran:?}");
}

/// `Command.env` and `Command.cwd` natively, from a launch plan: the same programs and output as
/// the Wasm fixtures `host/exec-command-env.wy` and `host/exec-command-cwd.wy`. The variable names
/// are the document's (`env` on the Exec entry), shown in the review; the directory is a granted
/// one, given to the run as its opened descriptor.
#[test]
fn native_exec_command_env_and_cwd() {
    use std::os::unix::fs::PermissionsExt;
    let Some(binding) = std::env::var_os("WITCHY_WASM_TEST_READY_BINDING") else {
        eprintln!("skipped: WITCHY_WASM_TEST_READY_BINDING is not set, and the library's Vec code is proved");
        return;
    };
    let fixture = Fixture::new();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/host");
    fs::create_dir_all(fixture.0.join("bin")).unwrap();
    fs::create_dir_all(fixture.0.join("data/sub")).unwrap();
    fs::write(fixture.0.join("data/marker"), "top\n").unwrap();
    fs::write(fixture.0.join("data/sub/marker"), "below\n").unwrap();
    let root = fixture.0.canonicalize().unwrap();
    let bin = root.join("bin");
    for name in ["where", "showenv"] {
        fs::write(bin.join(name), fs::read_to_string(fixtures.join("exec-command-bin").join(name)).unwrap()).unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    let build = |name: &str, manifest: String| {
        fs::write(root.join(format!("{name}.wy")), fs::read_to_string(fixtures.join(format!("exec-command-{name}.wy"))).unwrap()).unwrap();
        fs::write(root.join(format!("{name}.grants.toml")), manifest).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_witchyc"));
        command.args(["build", &format!("{name}.wy"), "-o", name, "--target=macos-arm64", &format!("--manifest={name}.grants.toml")])
            .current_dir(&root).env("WITCHY_WASM_READY_BINDING", &binding);
        if let Some(lean) = std::env::var_os("WITCHY_WASM_TEST_LEAN_ROOT") { command.env("WITCHY_WASM_LEAN_ROOT", lean); }
        let turn = super::share_library_proofs(&mut command);
        let built = command.output().unwrap();
        drop(turn);
        assert!(built.status.success(), "{built:?}");
        let ran = Command::new(root.join(name)).current_dir(&root).output().unwrap();
        assert!(ran.status.success(), "{ran:?}");
        assert_eq!(String::from_utf8_lossy(&ran.stdout), fs::read_to_string(fixtures.join(format!("exec-command-{name}.expected"))).unwrap(), "{ran:?}");
    };
    let cwd_plan = format!("[launch]\nversion = 1\n[exec]\nexec = {{ dir = \"{}\", programs = [\"where\"] }}\n[dirs]\ndata = {{ root = {{ from = \"cwd\", path = \"data\" }}, rights = [\"Read\"] }}\n", bin.display());
    std::thread::scope(|scope| {
        // The manifests and executables use separate names in this fixture.
        let cwd = scope.spawn(|| build("cwd", cwd_plan));
        let env_plan = format!("[launch]\nversion = 1\n[exec]\nexec = {{ dir = \"{}\", programs = [\"showenv\"], env = [\"LANG\", \"TZ\"] }}\n", bin.display());
        build("env", env_plan);
        let plan = fixture.run(&["native-plan", "env.wy", "env.grants.toml"]);
        assert!(plan.status.success(), "{plan:?}");
        assert!(String::from_utf8_lossy(&plan.stdout).contains("only the variables LANG, TZ (values from the program, which can change what the program does)"), "{plan:?}");
        for bad in ["env = [\"lang\"]", "env = [\"A=B\"]", "env = [\"TZ\", \"TZ\"]", "env = \"TZ\"", "env = [\"LD_PRELOAD\"]", "env = [\"DYLD_INSERT_LIBRARIES\"]", "env = [\"BASH_ENV\"]", "env = [\"PATH\"]"] {
            fs::write(root.join("bad.grants.toml"), format!("[launch]\nversion = 1\n[exec]\nexec = {{ dir = \"{}\", programs = [\"showenv\"], {bad} }}\n", bin.display())).unwrap();
            let refused = fixture.run(&["native-plan", "env.wy", "bad.grants.toml"]);
            assert!(!refused.status.success(), "{bad}: {refused:?}");
        }
        // The manifest runs check the proved Command library.
        // Development launch flags are admitted before guest code and this probe
        // checks that the admitted names reach the child through the raw broker.
        let raw = exec_program(&[("showenv", &[], true, None)]);
        let invoke = "    let run0 = only0.raw_run()\n";
        let (setup, output) = raw.split_once(invoke).unwrap();
        let mut source = setup.to_owned();
        for (name, value) in [("LANG", "C.UTF-8"), ("TZ", "UTC")] {
            for byte in name.bytes() {
                source.push_str(&format!("    only0.raw_env_name_byte(U8.truncate({byte}))\n"));
            }
            for byte in value.bytes() {
                source.push_str(&format!("    only0.raw_env_value_byte(U8.truncate({byte}))\n"));
            }
            source.push_str("    only0.raw_env_end()\n");
        }
        source.push_str(invoke);
        source.push_str(output);
        fs::write(root.join("env-dev.wy"), source).unwrap();
        let dev = fixture.run(&["build", "env-dev.wy", "-o", "env-dev", "--target=macos-arm64"]);
        assert!(dev.status.success(), "{dev:?}");
        let grant = format!("--exec={}:showenv", bin.display());
        for names in ["LD_PRELOAD", "LANG,DYLD_INSERT_LIBRARIES", "PATH", "BASH_ENV", "GIT_DIR", "HOME"] {
            let ran = Command::new(root.join("env-dev")).current_dir(&root).args(["--output", &grant, &format!("--exec-env={names}")]).output().unwrap();
            refused(&ran, "invalid --exec-env");
        }
        let allowed = Command::new(root.join("env-dev")).current_dir(&root).args(["--output", &grant, "--exec-env=LANG,TZ"]).output().unwrap();
        assert!(allowed.status.success(), "{allowed:?}");
        assert_eq!(allowed.stdout, b"7\n0\nLANG=C.UTF-8 TZ=UTC HOME=unset USER=unset\n0\n", "{allowed:?}");
        cwd.join().unwrap();
    });
}

#[test]
fn native_exec_runs_only_granted_programs_with_nothing_else() {
    std::thread::scope(|scope| {
        let selection = scope.spawn(assert_exec_name_selection);
        assert_exec_deadlines();
        selection.join().unwrap();
    });
}

fn write_exec_fixture(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn assert_exec_name_selection() {
    let fixture = Fixture::new();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/host");
    fs::create_dir(fixture.0.join("bin")).unwrap();
    // Native path grants never follow a link, and the temporary directory is under one.
    let bin = fixture.0.join("bin").canonicalize().unwrap();
    write_exec_fixture(&bin.join("greet"), &fs::read_to_string(fixtures.join("exec-bin/greet")).unwrap());
    symlink("/bin/ls", bin.join("lister")).unwrap();
    fs::write(bin.join("notes"), "not a program\n").unwrap();
    // As exec.wy: arguments whole, each run starts with none, ungranted and invalid names,
    // a link never followed, a non-executable not run, a narrowed Exec runs its one name.
    let source = exec_program(&[("greet", &["world; rm -rf /"], false, None), ("greet", &[], false, None), ("sh", &[], false, None), ("../bin/greet", &[], false, None),
        ("lister", &[], false, None), ("notes", &[], false, None), ("greet", &[], true, None)]);
    let expected = "7\nhello, world; rm -rf / (1)\n0\nhello,  (0)\n0\n101\n102\n104\n104\n0\nhello,  (0)\n0\n";
    fs::write(fixture.0.join("exec.wy"), &source).unwrap();
    let built = fixture.run(&["build", "exec.wy", "-o", "exec-dev", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let grant = format!("--exec={}:greet,lister,notes", bin.display());
    let dev = Command::new(fixture.0.join("exec-dev")).args(["--output", &grant, "--deadline-ms=5000"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&dev.stdout), expected, "{dev:?}");
    refused(&Command::new(fixture.0.join("exec-dev")).args(["--output"]).output().unwrap(), "Exec requires");
    // A narrowed Exec cannot run another granted name.
    let narrow = exec_program(&[("greet", &[], true, None)]).replace("    only0.raw_name_byte(U8.truncate(103))\n    only0.raw_name_byte(U8.truncate(114))\n    only0.raw_name_byte(U8.truncate(101))\n    only0.raw_name_byte(U8.truncate(101))\n    only0.raw_name_byte(U8.truncate(116))\n", "    only0.raw_name_byte(U8.truncate(115))\n    only0.raw_name_byte(U8.truncate(104))\n");
    fs::write(fixture.0.join("narrow.wy"), &narrow).unwrap();
    assert!(fixture.run(&["build", "narrow.wy", "-o", "narrow", "--target=macos-arm64"]).status.success());
    let narrowed = Command::new(fixture.0.join("narrow")).args(["--output", &format!("--exec={}:greet,sh", bin.display())]).output().unwrap();
    assert_eq!(narrowed.stdout, b"7\n0\n101\n", "{narrowed:?}");
    // The same program, manifest-bound: the programs and directory are the plan's.
    let manifest = format!("[launch]\nversion = 1\n[exec]\nexec = {{ dir = \"{}\", programs = [\"greet\", \"lister\", \"notes\"] }}\n", bin.canonicalize().unwrap().display());
    let built = build(&fixture, "exec", &source, &manifest);
    assert!(built.status.success(), "{built:?}");
    let manifested = run(&fixture, "exec", &[]);
    assert_eq!(String::from_utf8_lossy(&manifested.stdout), expected, "{manifested:?}");
    refused(&run(&fixture, "exec", &[&grant]), "not a declared argument");
    let plan = fixture.run(&["native-plan", "exec.wy", "exec.grants.toml"]);
    assert!(String::from_utf8_lossy(&plan.stdout).contains("Exec, running only greet, lister, notes"), "{plan:?}");
    // The program run is the file opened at start: replacing the name afterwards changes nothing
    // within this launch (checked by the launch identity, not by name).
}

fn assert_exec_deadlines() {
    use std::os::unix::process::ExitStatusExt;
    let fixture = Fixture::new();
    let root = fixture.0.canonicalize().unwrap();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    // What a child receives: no variable, no descriptor past the standard three, no input;
    // a run past its time is stopped with everything it started; output past the budget fails.
    write_exec_fixture(&bin.join("probe"), "#!/bin/sh\necho \"home=${HOME-unset} secret=${SECRET-unset}\"\nfor fd in 3 4 5 6 7 8 9; do if [ -e /dev/fd/$fd ]; then echo \"open $fd\"; fi; done\nread line && echo \"read $line\"\nexit 3\n");
    let write_slow = |programs: &Path, markers: &Path| {
        let path = programs.join("slow");
        write_exec_fixture(&path, &format!("#!/bin/sh\n(sleep 3; touch {0}/leftover) &\nprintf '%s %s\\n' \"$$\" \"$!\" > {0}/started-pids\nexec /bin/sleep 10\n", markers.display()));
    };
    write_slow(&bin, &root);
    write_exec_fixture(&bin.join("loud"), "#!/bin/sh\necho 0123456789\n");
    // Exercise the real cutoff with a shorter command deadline. The runtime's
    // deadline selection test checks the exact default independently.
    let source = exec_program(&[("probe", &[], false, None), ("slow", &[], false, Some(900)), ("loud", &[], false, None)]);
    fs::write(fixture.0.join("probe.wy"), source).unwrap();
    // The run-deadline launch starts `slow` first: with `probe` ahead of it, a loaded host spent
    // the 700 ms on two shell startups, `slow` never started, and its PIDs never appeared.
    fs::write(fixture.0.join("sooner.wy"), exec_program(&[("slow", &[], false, Some(900)), ("loud", &[], false, None)])).unwrap();
    std::thread::scope(|scope| {
        let sooner = scope.spawn(|| fixture.run(&["build", "sooner.wy", "-o", "sooner", "--target=macos-arm64"]));
        let built = fixture.run(&["build", "probe.wy", "-o", "probe", "--target=macos-arm64"]);
        assert!(built.status.success(), "{built:?}");
        let built = sooner.join().unwrap();
        assert!(built.status.success(), "{built:?}");
    });
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).env("SECRET", "s3cret").args(["-c", script]).output().unwrap();

    // Give each launch its own process markers so both deadlines can be tested concurrently.
    let launch_probe = root.join("launch-deadline");
    let launch_bin = launch_probe.join("bin");
    fs::create_dir_all(&launch_bin).unwrap();
    fs::copy(bin.join("loud"), launch_bin.join("loud")).unwrap();
    write_slow(&launch_bin, &launch_probe);
    std::thread::scope(|scope| {
        let command_deadline = scope.spawn(|| {
            let started = std::time::Instant::now();
            let ran = shell(&format!("exec ./probe --output --exec={}:probe,slow,loud --exec-bytes=30 3<probe.wy 4<probe.wy </dev/zero", bin.display()));
            assert_eq!(ran.stdout, b"7\nhome=unset secret=unset\n3\n106\n103\n", "{ran:?}");
            assert!(started.elapsed() < std::time::Duration::from_secs(4), "{:?}", started.elapsed());
            assert_exec_probes_stopped(&exec_probe_pids(&root.join("started-pids"), 2));
            assert!(!root.join("leftover").exists(), "a program the run started outlived its run");
        });

        // The run's own deadline stops a program sooner, and its group with it.
        let started = std::time::Instant::now();
        let ran = shell(&format!("exec ./sooner --output --exec={}:slow,loud --deadline-ms=700", launch_bin.display()));
        assert!(started.elapsed() < std::time::Duration::from_millis(1800), "{ran:?} {:?}", started.elapsed());
        const SIGALRM: i32 = 14;
        if ran.status.success() {
            // The broker can report the clipped command budget expiring just before
            // the launch alarm arrives. The next command has no budget left either.
            assert_eq!(ran.stdout, b"7\n106\n106\n", "{ran:?}");
            assert!(ran.stderr.is_empty(), "{ran:?}");
        } else {
            assert_eq!(ran.status.signal(), Some(SIGALRM), "{ran:?}");
        }
        assert_exec_probes_stopped(&exec_probe_pids(&launch_probe.join("started-pids"), 2));
        assert!(!launch_probe.join("leftover").exists(), "a program outlived the run's deadline");
        command_deadline.join().unwrap();
    });
}

fn assert_exec_write_grants_are_disjoint(overlap: &Path, probe: &Path) {
    let fixture = Fixture::new();
    let base = fixture.0.canonicalize().unwrap();
    let bin = base.join("bin");
    fs::create_dir(&bin).unwrap();
    fs::copy(probe, bin.join("probe")).unwrap();
    // Scan of b924e73a: by identity, every writable grant in either direction.
    let exec_grant = format!("--exec={}:probe", bin.display());
    for root in [base.display().to_string(), bin.display().to_string()] {
        refused(&Command::new(overlap).args(["--output", &exec_grant, &format!("--dir={root}"), "--dir-rights=write"]).output().unwrap(), "overlaps a writable");
    }
    fs::create_dir(bin.join("inner")).unwrap();
    refused(&Command::new(overlap).args(["--output", &exec_grant, &format!("--dir={}", bin.join("inner").display()), "--dir-rights=write"]).output().unwrap(), "overlaps a writable");
    fs::create_dir(fixture.0.join("elsewhere")).unwrap();
    let apart = Command::new(overlap).args(["--output", &exec_grant, &format!("--dir={}", base.join("elsewhere").display()), "--dir-rights=write"]).output().unwrap();
    assert_eq!(apart.stdout, MARK, "{apart:?}");
    let written = "fn main(exec: Exec, result: File<Write>, console: Console<Write>):\n    console.print_int(7)\n    result.raw_abandon()\n";
    fs::write(fixture.0.join("written.wy"), written).unwrap();
    assert!(fixture.run(&["build", "written.wy", "-o", "written", "--target=macos-arm64"]).status.success());
    for target in [bin.join("probe"), bin.join("new"), base.join("program")] {
        refused(&Command::new(fixture.0.join("written")).args(["--output", &exec_grant, &format!("--write-file={}", target.display())]).output().unwrap(), "overlaps a writable");
    }
    let manifest = format!("[launch]\nversion = 1\n[flags]\nresult = {{ type = \"path\" }}\n[exec]\nexec = {{ dir = \"{}\", programs = [\"probe\"] }}\n[files]\nresult = {{ from = \"arg\", name = \"result\" }}\n", bin.display());
    assert!(build(&fixture, "written-manifest", written, &manifest).status.success());
    refused(&run(&fixture, "written-manifest", &["--result", &bin.join("probe").display().to_string()]), "overlaps a writable");
    assert_eq!(run(&fixture, "written-manifest", &["--result", "elsewhere/out"]).stdout, MARK);
}

/// Scan of df5fe687: everything the guest sends the broker is hostile. The Exec request decoder
/// is fuzzed directly with random, truncated and oversized requests and argument counts.
#[test]
fn native_exec_request_decoder_survives_hostile_requests() {
    let fixture = Fixture::new();
    let driver = fixture.0.join("exec-fuzz");
    #[cfg(not(witchy_native_sanitized))]
    let mut compiler = Command::new(option_env!("RUSTC").unwrap_or("rustc"));
    #[cfg(witchy_native_sanitized)]
    let mut compiler = Command::new(env!("WITCHY_NATIVE_SANITIZER_RUSTC"));
    #[cfg(witchy_native_sanitized)]
    compiler.args(["-Zsanitizer=address", "-C", concat!("linker=", env!("WITCHY_NATIVE_SANITIZER_CLANG")),
        "-C", "link-arg=--ld-path=/usr/bin/ld"]);
    let built = compiler.current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["--edition=2024", "-Copt-level=2", "-C", concat!("link-arg=", env!("OUT_DIR"), "/", env!("WITCHY_NATIVE_ARCHIVE_FILE")),
            "tests/fixtures/native-sandbox/exec-fuzz.rs", "-o"])
        .arg(&driver).output().unwrap();
    assert!(built.status.success(), "{built:?}");
    let ran = Command::new(&driver).current_dir(&fixture.0).output().unwrap();
    assert!(ran.status.success(), "{ran:?}");
    let text = String::from_utf8_lossy(&ran.stdout);
    let counts: Vec<u64> = text.split_whitespace().skip(1).map(|n| n.parse().unwrap()).collect();
    assert!(counts.len() == 3 && counts.iter().all(|n| *n > 0), "{text}");
    assert_eq!(counts[0] + counts[1], 60_000, "{text}");
}

/// A written file in a launch plan: its directory is resolved as a directory source is (links,
/// `within`, device), the file is made or replaced whole when closed, or with `mode = "create"`
/// made only if absent; a file with a second name is not replaced.
#[test]
fn manifest_written_file_is_made_in_its_resolved_directory() {
    let fixture = Fixture::new();
    for dir in ["allowed/out", "outside"] { fs::create_dir_all(fixture.0.join(dir)).unwrap(); }
    fs::write(fixture.0.join("outside/target"), b"Z").unwrap();
    symlink("../outside", fixture.0.join("allowed/escape")).unwrap();
    let source = "fn main(result: File<Write>, out: Console<Write>):\n    out.print_int(7)\n    let wrote = result.raw_write_byte(U8.truncate(65))\n    out.print_int(result.raw_close())\n";
    let manifest = |mode: &str| format!("[launch]\nversion = 1\n[flags]\nresult = {{ type = \"path\" }}\n[files]\nresult = {{ path = {{ from = \"arg\", name = \"result\" }}, mode = \"{mode}\", within = {{ from = \"cwd\", path = \"allowed\" }} }}\n");
    assert!(build(&fixture, "replace", source, &manifest("replace")).status.success());
    assert!(build(&fixture, "create", source, &manifest("create")).status.success());
    assert_eq!(run(&fixture, "replace", &["--result", "allowed/out/a"]).stdout, b"7\n0\n");
    assert_eq!(fs::read(fixture.0.join("allowed/out/a")).unwrap(), b"A");
    assert_eq!(run(&fixture, "replace", &["--result", "allowed/out/a"]).stdout, b"7\n0\n");
    // Made only if absent: status 6 (exists), the file unchanged.
    fs::write(fixture.0.join("allowed/out/a"), b"kept").unwrap();
    assert_eq!(run(&fixture, "create", &["--result", "allowed/out/a"]).stdout, b"7\n6\n");
    assert_eq!(fs::read(fixture.0.join("allowed/out/a")).unwrap(), b"kept");
    assert_eq!(run(&fixture, "create", &["--result", "allowed/out/new"]).stdout, b"7\n0\n");
    refused(&run(&fixture, "replace", &["--result", "outside/target"]), "outside the directory");
    refused(&run(&fixture, "replace", &["--result", "allowed/escape/target"]), "symbolic link");
    refused(&run(&fixture, "replace", &["--result", "allowed/../outside/target"]), "`..`");
    refused(&run(&fixture, "replace", &["--result", "allowed/out/"]), "names no file");
    fs::hard_link(fixture.0.join("outside/target"), fixture.0.join("allowed/out/linked")).unwrap();
    assert_eq!(run(&fixture, "replace", &["--result", "allowed/out/linked"]).stdout, b"7\n2\n");
    assert_eq!(fs::read(fixture.0.join("outside/target")).unwrap(), b"Z");
    let plan = fixture.run(&["native-plan", "create.wy", "create.grants.toml"]);
    assert!(String::from_utf8_lossy(&plan.stdout).contains("made only if absent"), "{plan:?}");
}

/// A socket-entry server may hold directory grants: each connection's broker gets the roots the
/// supervisor opened once, and the file byte and entry budgets are the run's, shared by every
/// worker, so serving more connections never spends a fresh budget.
#[test]
fn socket_entry_servers_share_directories_and_budgets_across_connections() {
    use std::{
        io::{Read, Write},
        net::TcpStream,
        process::{ExitStatus, Stdio},
    };
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("root")).unwrap();
    fs::write(fixture.0.join("root/a.txt"), b"AB").unwrap();
    let mut source = String::from("fn main(socket: Socket, root: Dir<Read>):\n    let ignored = socket.raw_recv_byte()\n");
    for byte in b"a.txt" { source.push_str(&format!("    root.raw_path_byte(U8.truncate({byte}))\n")); }
    source.push_str("    let file = root.raw_open_file()\n    let byte = file.raw_read_byte()\n    let closed = file.raw_close()\n    let sent = socket.raw_send_byte(U8.truncate(byte))\n    let done = socket.raw_close()\n");
    fs::write(fixture.0.join("server.wy"), &source).unwrap();
    let built = fixture.run(&["build", "server.wy", "-o", "server", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let serve = |binary: &str, name: &str, extra: &[&str], inputs: &[&[u8]]| -> (ExitStatus, Vec<Vec<u8>>) {
        let log = fixture.0.join(format!("{name}.stderr"));
        let mut server = Command::new(fixture.0.join(binary))
            .current_dir(&fixture.0)
            .args(["--listen=127.0.0.1:0", "--instance-per-connection", "--deadline-ms=5000", "--dir=root"])
            .arg(format!("--connections={}", inputs.len()))
            .args(extra)
            .stderr(Stdio::from(fs::File::create(&log).unwrap()))
            .spawn().unwrap();
        let endpoint = await_loopback_listener(&mut server, &log, Duration::from_secs(2));
        let mut replies = Vec::new();
        for input in inputs {
            let mut peer = TcpStream::connect_timeout(&endpoint, Duration::from_secs(2)).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            peer.write_all(input).unwrap();
            let mut reply = Vec::new();
            if let Err(error) = peer.read_to_end(&mut reply) {
                assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset, "{name}: {error}");
            }
            replies.push(reply);
        }
        (server.wait().unwrap(), replies)
    };
    let (status, uncapped) = serve("server", "uncapped", &[], &[b"x", b"x", b"x"]);
    assert!(status.success(), "{status:?}");
    assert_eq!(uncapped, [b"A".to_vec(), b"A".to_vec(), b"A".to_vec()]);
    // Two bytes for the whole run, so the third connection finds the budget spent.
    let (_, capped) = serve("server", "capped", &["--fs-bytes=2"], &[b"x", b"x", b"x"]);
    assert_eq!(capped, [b"A".to_vec(), b"A".to_vec(), Vec::new()]);

    // Entries are the run's too: each connection makes one directory; one entry for the run.
    let mut maker = String::from("fn main(socket: Socket, root: Dir<Write>):\n    let byte = socket.raw_recv_byte()\n    root.raw_path_byte(U8.truncate(byte))\n    let made = root.raw_make_dir()\n    let sent = socket.raw_send_byte(U8.truncate(48 + made))\n    let done = socket.raw_close()\n");
    maker.push('\n');
    fs::write(fixture.0.join("maker.wy"), &maker).unwrap();
    assert!(fixture.run(&["build", "maker.wy", "-o", "maker", "--target=macos-arm64"]).status.success());
    let (_, replies) = serve("maker", "directory", &["--dir-rights=write", "--fs-entries=1"], &[b"x", b"y"]);
    // 0 made; 3 TooLarge (no entry left).
    assert_eq!(replies, [b"0".to_vec(), b"3".to_vec()]);
    assert!(fixture.0.join("root/x").is_dir() && !fixture.0.join("root/y").exists());
}

/// Limits in the plan are enforced as the development flags of those names, and shown by
/// `native-plan`; an optional argument stands for its declared default when absent.
#[test]
fn manifest_limits_and_optional_arguments() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("default")).unwrap();
    fs::create_dir_all(fixture.0.join("given")).unwrap();
    fs::write(fixture.0.join("default/a.txt"), b"D").unwrap();
    fs::write(fixture.0.join("given/a.txt"), b"G").unwrap();
    let manifest = "[launch]\nversion = 1\n[flags]\nproject = { type = \"path\", required = false, default = { from = \"cwd\", path = \"default\" } }\n[dirs]\nproject = { root = { from = \"arg\", name = \"project\" }, rights = [\"Read\"] }\n[limits]\noutput-bytes = 4\nfs-bytes = 1\n";
    let built = build(&fixture, "tool", &reader("Dir<Read>"), manifest);
    assert!(built.status.success(), "{built:?}");
    // "7\n" "0\n" fits 4 bytes; the third line does not: the output budget stops the run.
    let absent = run(&fixture, "tool", &[]);
    assert_eq!(absent.stdout, b"7\n0\n", "{absent:?}");
    assert!(!absent.status.success());
    let plan = String::from_utf8(fixture.run(&["native-plan", "tool.wy", "tool.grants.toml"]).stdout).unwrap();
    assert!(plan.contains("limits: fs-bytes = 1, output-bytes = 4") && plan.contains("or when absent default under the startup directory"), "{plan}");
    let roomy = manifest.replace("output-bytes = 4", "output-bytes = 64");
    assert!(build(&fixture, "roomy", &reader("Dir<Read>"), &roomy).status.success());
    assert_eq!(run(&fixture, "roomy", &[]).stdout, b"7\n0\n68\n");
    assert_eq!(run(&fixture, "roomy", &["--project", "given"]).stdout, b"7\n0\n71\n");
    // A limit is not a launch flag of a manifest-bound executable.
    refused(&run(&fixture, "roomy", &["--output-bytes=1000"]), "not a declared argument");
    // The deadline: a run that blocks on its input is stopped at the plan's limit.
    let blocked = "fn main(input: Console<Read>, out: Console<Write>):\n    out.print_int(7)\n    out.print_int(input.next_byte())\n";
    assert!(build(&fixture, "blocked", blocked, "[launch]\nversion = 1\n[input]\ninput = { stdin = true }\n[limits]\ndeadline-ms = 300\n").status.success());
    let started = std::time::Instant::now();
    let mut child = Command::new(fixture.0.join("blocked")).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    // Held open (`wait` would close it): the input never ends, only the deadline does.
    let _input = child.stdin.take();
    let status = child.wait().unwrap();
    assert!(!status.success() && started.elapsed() < std::time::Duration::from_secs(2), "{status:?} {:?}", started.elapsed());
}

/// `main(connection: Socket, ...)` in a launch plan: the executable serves its planned
/// connections, one sandboxed worker each, with a directory and the plan's limits.
#[test]
fn manifest_socket_entry_server() {
    use std::io::{Read, Write};
    use std::{net::TcpStream, process::Stdio};
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("root")).unwrap();
    fs::write(fixture.0.join("root/a.txt"), b"S").unwrap();
    let mut source = String::from("fn main(socket: Socket, root: Dir<Read>):\n    let ignored = socket.raw_recv_byte()\n");
    for byte in b"a.txt" { source.push_str(&format!("    root.raw_path_byte(U8.truncate({byte}))\n")); }
    source.push_str("    let file = root.raw_open_file()\n    let byte = file.raw_read_byte()\n    let closed = file.raw_close()\n    let sent = socket.raw_send_byte(U8.truncate(byte))\n    let done = socket.raw_close()\n");
    // Let the manifest-bound server acquire and hold the port it announces.
    let manifest = "[launch]\nversion = 1\n[listen]\nsocket = \"127.0.0.1:0\"\n[dirs]\nroot = { root = { from = \"cwd\", path = \"root\" }, rights = [\"Read\"] }\n[limits]\nconnections = 2\nconnection-deadline-ms = 2000\ndeadline-ms = 5000\n";
    let built = build(&fixture, "server", &source, manifest);
    assert!(built.status.success(), "{built:?}");
    let log = fixture.0.join("server.stderr");
    let mut server = Command::new(fixture.0.join("server")).current_dir(&fixture.0)
        .stderr(Stdio::from(fs::File::create(&log).unwrap())).spawn().unwrap();
    let endpoint = await_loopback_listener(&mut server, &log, Duration::from_secs(2));
    for _ in 0..2 {
        let mut peer = TcpStream::connect_timeout(&endpoint, Duration::from_secs(2)).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        peer.write_all(b"x").unwrap();
        let mut reply = Vec::new();
        peer.read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"S");
    }
    assert!(server.wait().unwrap().success(), "the server stops after its planned connections");
    let plan = String::from_utf8(fixture.run(&["native-plan", "server.wy", "server.grants.toml"]).stdout).unwrap();
    assert!(plan.contains("Socket, one connection accepted on 127.0.0.1:0") && plan.contains("connections = 2"), "{plan}");
}

/// A secret beside a read-only directory: every file the directory opens is compared with the
/// secrets' objects on the opened descriptor, and one stops the run. A launcher that passes a
/// key on a pipe names the key file's identity (`--secret-object`, a plan's `object`), which no
/// grant may then reach. A writable directory beside a secret stays refused.
#[cfg(witchy_native_aws_lc)]
#[test]
fn secrets_beside_directories_and_declared_secret_objects() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("data")).unwrap();
    fs::write(fixture.0.join("data/key"), b"K").unwrap();
    fs::write(fixture.0.join("data/plain"), b"P").unwrap();
    let mut source = String::from("fn main(out: Console<Write>, token: Secret<Reveal>, data: Dir<Read>):\n    out.print_int(7)\n");
    let open = |source: &mut String, name: &str, index: usize| {
        for byte in name.bytes() { source.push_str(&format!("    data.raw_path_byte(U8.truncate({byte}))\n")); }
        source.push_str(&format!("    let file{index} = data.raw_open_file()\n    out.print_int(file{index}.raw_status())\n    let closed{index} = file{index}.raw_close()\n"));
    };
    open(&mut source, "plain", 0);
    open(&mut source, "key", 1);
    fs::write(fixture.0.join("dir.wy"), &source).unwrap();
    assert!(fixture.run(&["build", "dir.wy", "-o", "dir", "--target=macos-arm64"]).status.success());
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", script]).output().unwrap();
    let flags = "--output --secret-slot=token:reveal-bytes:fd:3 --dir=data";
    // The plain file opens; the key's own file, by any name, ends the run.
    let stopped = shell(&format!("exec ./dir {flags} 3<data/key"));
    assert!(!stopped.status.success() && stopped.stdout.starts_with(b"7\n0\n") && !stopped.stdout.ends_with(b"0\n0\n"), "{stopped:?}");
    fs::write(fixture.0.join("other"), b"O").unwrap();
    assert_eq!(shell(&format!("exec ./dir {flags} 3<other")).stdout, b"7\n0\n0\n");
    // A key passed on a pipe: the runtime sees only the pipe, unless the launcher names the file.
    let identity = fs::metadata(fixture.0.join("data/key")).unwrap();
    let object = format!("{}:{}", identity.dev(), identity.ino());
    assert_eq!(shell(&format!("cat data/key | (exec ./dir {flags} 3<&0 </dev/null)")).stdout, b"7\n0\n0\n");
    let named = shell(&format!("cat data/key | (exec ./dir {flags} --secret-object={object} 3<&0 </dev/null)"));
    assert!(!named.status.success() && !named.stdout.ends_with(b"0\n0\n"), "{named:?}");
    // The same in a launch plan, with the object as a declared argument.
    let manifest = "[launch]\nversion = 1\n[flags]\ntoken-fd = { type = \"fd\", secret = true }\nkey-object = { type = \"object\" }\n[secrets]\ntoken = { use = \"reveal-bytes\", from = { from = \"arg\", name = \"token-fd\" }, object = { from = \"arg\", name = \"key-object\" } }\n[dirs]\ndata = { root = { from = \"cwd\", path = \"data\" }, rights = [\"Read\"] }\n";
    assert!(build(&fixture, "planned", &source, manifest).status.success());
    let ok = shell("exec ./planned --token-fd 3 --key-object 1:1 3<other");
    assert_eq!(ok.stdout, b"7\n0\n0\n", "{ok:?}");
    let planned = shell(&format!("cat data/key | (exec ./planned --token-fd 3 --key-object {object} 3<&0 </dev/null)"));
    assert!(!planned.status.success() && !planned.stdout.ends_with(b"0\n0\n"), "{planned:?}");
    refused(&shell("exec ./planned --token-fd 3 --key-object 1 3<other"), "DEVICE:INODE");
    // Writable directories beside a secret: allowed at build and at launch (round 3, item 3).
    let writable = manifest.replace("rights = [\"Read\"]", "rights = [\"Write\"]");
    let write_source = "fn main(out: Console<Write>, token: Secret<Reveal>, data: Dir<Write>):\n    out.print_int(7)\n";
    assert!(build(&fixture, "writable", write_source, &writable).status.success());
}

/// Round 3, item 3, and the scan of its first commit: secrets beside writable directories.
/// A writable root that holds a secret's object at any depth (so also any directory on the way
/// to it) is refused at launch, by an identity walk on opened descriptors. One moved in after
/// launch, by another process, is still neither read, written, moved nor dropped: every open to
/// read or to append compares the opened descriptor first (a match stops the run), and a rename
/// (either side, of a file or of a directory holding one, in every mode), remove or replace
/// whose tree holds a secret's object is refused. Creates write new files.
#[cfg(witchy_native_aws_lc)]
#[test]
fn secrets_beside_writable_directories_by_descriptor() {
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("data/sub")).unwrap();
    fs::create_dir_all(fixture.0.join("keys")).unwrap();
    fs::write(fixture.0.join("keys/key"), b"K").unwrap();
    fs::write(fixture.0.join("data/decoy"), b"D").unwrap();
    let path = |source: &mut String, dir: &str, name: &str, indent: &str| {
        for byte in name.bytes() { source.push_str(&format!("{indent}{dir}.raw_path_byte(U8.truncate({byte}))\n")); }
    };
    let mut source = String::from("fn main(out: Console<Write>, input: Console<Read>, token: Secret<Reveal>, view: Dir<Read>, data: Dir<Write>):\n    out.print_int(7)\n");
    // A create-only create: a new file.
    path(&mut source, "data", "fresh", "    ");
    source.push_str("    let fresh = data.raw_open_new()\n    let a = fresh.raw_write_byte(U8.truncate(65))\n    out.print_int(fresh.raw_close())\n");
    // Wait for the key to be moved in, to data/sub/key.
    source.push_str("    out.print_int(input.next_byte())\n");
    // Rename the directory holding it, rename it, remove it, replace it: refused (2).
    for (from, to) in [("sub", "elsewhere"), ("sub/key", "moved"), ("decoy", "sub/key")] {
        path(&mut source, "data", from, "    ");
        source.push_str("    data.raw_rename_from()\n");
        path(&mut source, "data", to, "    ");
        source.push_str("    out.print_int(data.raw_rename())\n");
    }
    path(&mut source, "data", "sub/key", "    ");
    source.push_str("    out.print_int(data.raw_remove())\n");
    path(&mut source, "data", "sub/key", "    ");
    source.push_str("    let replaced = data.raw_open_file()\n    let b = replaced.raw_write_byte(U8.truncate(66))\n    out.print_int(replaced.raw_close())\n");
    // Append to it: the opened descriptor is the key's; the run stops before a byte.
    path(&mut source, "data", "sub/key", "    ");
    source.push_str("    let appended = data.raw_open_append()\n    let c = appended.raw_write_byte(U8.truncate(67))\n    out.print_int(appended.raw_close())\n");
    fs::write(fixture.0.join("ops.wy"), &source).unwrap();
    let built = fixture.run(&["build", "ops.wy", "-o", "ops", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let shell = |script: &str| Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", script]).stdin(std::process::Stdio::null()).output().unwrap();
    let flags = "--output --stream-output --input-stdin --input-bytes=1 --secret-slot=token:reveal-bytes:fd:3 --dir=data --write-dir=data";
    // A writable root holding the key, directly or below a directory: refused at launch.
    fs::rename(fixture.0.join("keys/key"), fixture.0.join("data/key")).unwrap();
    refused(&shell(&format!("exec ./ops {flags} 3<data/key")), "holds a secret's object");
    fs::rename(fixture.0.join("data/key"), fixture.0.join("data/sub/key")).unwrap();
    refused(&shell(&format!("exec ./ops {flags} 3<data/sub/key")), "holds a secret's object");
    fs::rename(fixture.0.join("data/sub/key"), fixture.0.join("keys/key")).unwrap();
    // The key outside at launch, moved in by another process while the run waits.
    let mut run = Command::new("/bin/sh").current_dir(&fixture.0).args(["-c", &format!("exec ./ops {flags} 3<keys/key")])
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap();
    let mut first = [0u8; 4];
    run.stdout.as_mut().unwrap().read_exact(&mut first).unwrap();
    assert_eq!(&first, b"7\n0\n");
    fs::rename(fixture.0.join("keys/key"), fixture.0.join("data/sub/key")).unwrap();
    run.stdin.take().unwrap().write_all(b"x").unwrap();
    let ops = run.wait_with_output().unwrap();
    assert!(!ops.status.success(), "{ops:?}");
    assert_eq!(String::from_utf8_lossy(&ops.stdout), "120\n2\n2\n2\n2\n2\n", "{ops:?}");
    assert_eq!(fs::read(fixture.0.join("data/sub/key")).unwrap(), b"K");
    assert_eq!(fs::read(fixture.0.join("data/fresh")).unwrap(), b"A");
    assert_eq!(fs::read(fixture.0.join("data/decoy")).unwrap(), b"D");
    fs::rename(fixture.0.join("data/sub/key"), fixture.0.join("keys/key")).unwrap();

    // A racing process moves the key in and out under the decoy's name while the guest appends
    // to and reads that name: whatever each open reached, the key's inode is neither read nor
    // written (a launch that found it in the root is refused).
    let mut race = String::from("fn main(out: Console<Write>, token: Secret<Reveal>, view: Dir<Read>, data: Dir<Write>):\n    var i: U64 = 0\n    while i < 200:\n");
    path(&mut race, "data", "decoy", "        ");
    race.push_str("        let appended = data.raw_open_append()\n        let w = appended.raw_write_byte(U8.truncate(66))\n        let closed = appended.raw_close()\n");
    path(&mut race, "view", "decoy", "        ");
    race.push_str("        let read = view.raw_open_file()\n        if read.raw_status() == 0:\n            out.print_int(read.raw_read_byte())\n        let shut = read.raw_close()\n        i = i + 1\n");
    fs::write(fixture.0.join("race.wy"), &race).unwrap();
    let built = fixture.run(&["build", "race.wy", "-o", "race", "--target=macos-arm64"]);
    assert!(built.status.success(), "{built:?}");
    let mut key = fs::File::open(fixture.0.join("keys/key")).unwrap();
    let identity = key.metadata().unwrap();
    let root = fixture.0.clone();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let swapping = {
        let (stop, root) = (stop.clone(), root.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = fs::rename(root.join("data/decoy"), root.join("keys/decoy"));
                let _ = fs::rename(root.join("keys/key"), root.join("data/decoy"));
                let _ = fs::rename(root.join("data/decoy"), root.join("keys/key"));
                let _ = fs::rename(root.join("keys/decoy"), root.join("data/decoy"));
            }
        })
    };
    let race_flags = "--output --secret-slot=token:reveal-bytes:fd:3 --dir=data --write-dir=data";
    // The key's file by its identity (volfs), whatever its name is now.
    let invocation = format!("exec ./race {race_flags} 3</.vol/{}/{}", identity.dev(), identity.ino());
    let results = std::thread::scope(|scope| {
        let start = || (0..5).map(|_| shell(&invocation)).collect::<Vec<_>>();
        let batches: Vec<_> = (0..4).map(|_| scope.spawn(start)).collect();
        batches.into_iter().map(|batch| batch.join()).collect::<Vec<_>>()
    });
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    swapping.join().unwrap();
    let mut stopped = 0;
    for ran in results.into_iter().flat_map(|batch| batch.unwrap()) {
        assert!(!String::from_utf8_lossy(&ran.stdout).lines().any(|line| line == "75"), "the key's byte was read: {ran:?}");
        stopped += usize::from(!ran.status.success());
    }
    let mut bytes = Vec::new();
    use std::io::Seek;
    key.seek(std::io::SeekFrom::Start(0)).unwrap();
    key.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"K", "the key's inode was written");
    assert!(stopped > 0, "no run reached the key's inode through a moved name: the race did not run");
}

/// Review of 5fccb36f: an exec directory under an unreadable ancestor passed the overlap check
/// (the walk up stopped early and read as "not inside") and the guest appended to a granted
/// program; a secret sat beside Exec; a program that left its group outlived the run; the
/// run's private directory was left behind with what the program wrote.
#[test]
fn exec_review_findings_stay_closed() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let root = fixture.0.canonicalize().unwrap();
    let bin = root.join("locked/bin");
    fs::create_dir_all(&bin).unwrap();
    let script = |name: &str, text: &str| {
        fs::write(bin.join(name), text).unwrap();
        fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    };
    script("prog", "#!/bin/sh\necho original\n");
    let reported_cwd = root.join("worker-cwd");
    script("litter", &format!("#!/bin/sh\nprintf '%s\\n' \"$PWD\" > '{}'\necho x > leftover\nmkdir sub\necho y > sub/leftover\n", reported_cwd.display()));
    script("escape", &format!("#!/bin/sh\n/usr/bin/perl -e 'use POSIX; my $session = POSIX::setsid(); defined($session) && $session >= 0 or die; close STDOUT; open(PID, \">{0}/escaped-pid\") or die; print PID \"$$\\n\"; close PID; sleep 3; open(F, \">{0}/escaped\"); print F 1;' &\nsleep 10\n", root.display()));
    // Round 3, item 6: a grandchild that leaves the program's group and is orphaned at once
    // (its parent exits) has no parent to walk from: it is found by the program's session.
    script("orphan", &format!("#!/bin/sh\n/usr/bin/perl -e 'pipe(R, W) or die; my $child = fork(); defined($child) or die; if ($child == 0) {{ close R; setpgrp(0, 0) or die; close STDOUT; open(PID, \">{0}/orphaned-pid\") or die; print PID \"$$\\n\"; close PID; print W \"ready\\n\"; close W; sleep 3; open(F, \">{0}/orphaned\"); print F 1; exit 0; }} close W; <R> or die;' && echo done\n", root.display()));
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o300)).unwrap();
    // A writable root above an unreadable ancestor of the exec directory: refused.
    let overlap = "fn main(exec: Exec, dir: Dir<Write>, console: Console<Write>):\n    console.print_int(7)\n";
    fs::write(fixture.0.join("overlap.wy"), overlap).unwrap();
    assert!(fixture.run(&["build", "overlap.wy", "-o", "overlap", "--target=macos-arm64"]).status.success());
    let overlap = fixture.0.join("overlap");
    let grant = format!("--exec={}:prog,litter,escape,orphan", bin.display());
    refused(&Command::new(&overlap).args(["--output", &grant, &format!("--dir={}", root.display()), "--dir-rights=write"]).output().unwrap(), "overlaps a writable");
    // The private directory goes with what the program left in it; a program that left its
    // group is stopped with the run's tree when the run times it out.
    let source = exec_program(&[
        ("litter", &[], false, None),
        ("escape", &[], false, Some(600)),
        ("orphan", &[], false, None),
    ]);
    fs::write(fixture.0.join("run.wy"), source).unwrap();
    // The other grant scenarios use the same executable with a private directory.
    let ran = std::thread::scope(|scope| {
        scope.spawn(|| assert_exec_write_grants_are_disjoint(&overlap, &bin.join("prog")));
        assert!(fixture.run(&["build", "run.wy", "-o", "run", "--target=macos-arm64"]).status.success());
        Command::new(fixture.0.join("run")).args(["--output", &grant]).output().unwrap()
    });
    assert_eq!(String::from_utf8_lossy(&ran.stdout), "7\n0\n106\ndone\n0\n", "{ran:?}");
    for name in ["escaped-pid", "orphaned-pid"] {
        assert_exec_probes_stopped(&exec_probe_pids(&root.join(name), 1));
    }
    fs::set_permissions(root.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!root.join("escaped").exists(), "a program that left its group outlived its run");
    assert!(!root.join("orphaned").exists(), "an orphan that left the program's group outlived it");
    let reported = fs::read_to_string(reported_cwd).unwrap();
    let private = Path::new(reported.strip_suffix('\n').unwrap());
    assert!(private.is_absolute() && private.file_name().unwrap().to_string_lossy().starts_with("witchy-exec-"), "{private:?}");
    assert_eq!(fs::symlink_metadata(private).unwrap_err().kind(), std::io::ErrorKind::NotFound, "{private:?}");
}

/// A secret beside Exec is refused at build and at launch: a program it starts runs outside the
/// sandbox with the user's authority.
#[cfg(witchy_native_aws_lc)]
#[test]
fn secrets_are_refused_beside_exec() {
    let fixture = Fixture::new();
    let source = "fn main(out: Console<Write>, token: Secret<Seal>, exec: Exec):\n    out.print_int(7)\n";
    let manifest = "[launch]\nversion = 1\n[secrets]\ntoken = { use = \"seal\", from = \"env:TOKEN\" }\n[exec]\nexec = { dir = \"/usr/bin\", programs = [\"true\"] }\n";
    let built = build(&fixture, "tool", source, manifest);
    assert!(!built.status.success() && String::from_utf8_lossy(&built.stderr).contains("cannot be combined with Exec"), "{built:?}");
    fs::write(fixture.0.join("dev.wy"), source).unwrap();
    assert!(fixture.run(&["build", "dev.wy", "-o", "dev", "--target=macos-arm64"]).status.success());
    refused(&Command::new(fixture.0.join("dev")).env("TOKEN", "t").args(["--output", "--secret-slot=token:seal:env:TOKEN", "--exec=/usr/bin:true"]).output().unwrap(), "cannot be combined with Exec");
}

/// Round 3, item 2: a socket-entry server with a read file, the input, outbound Net, Exec and a
/// writable directory beside its Socket, from a launch plan. Each connection runs in a fresh
/// worker: it reads the file and the input from their start (nothing another connection read
/// moved them), dials only the planned endpoint, runs only the planned program, and has its own
/// file byte and entry budgets beside the run's (`fs-bytes-per-connection`,
/// `fs-entries-per-connection`, shown by `native-plan`).
#[test]
fn manifest_socket_server_with_file_input_net_and_exec() {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let fixture = Fixture::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::create_dir_all(root.join("data")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("f.txt"), b"FG").unwrap();
    fs::write(root.join("bin/greet"), "#!/bin/sh\nprintf g\n").unwrap();
    fs::set_permissions(root.join("bin/greet"), fs::Permissions::from_mode(0o755)).unwrap();
    // The outbound peer: answers each connection with the byte it was sent, plus one.
    let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
    let up = upstream.local_addr().unwrap();
    let echo = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut peer, _) = upstream.accept().unwrap();
            let mut byte = [0u8; 1];
            peer.read_exact(&mut byte).unwrap();
            peer.write_all(&[byte[0] + 1]).unwrap();
        }
    });
    let mut source = String::from("fn main(socket: Socket, file: File<Read>, input: Console<Read>, api: Net, exec: Exec, data: Dir<Write>):\n    let c = socket.raw_recv_byte()\n");
    source.push_str("    let f = socket.raw_send_byte(U8.truncate(file.raw_read_byte()))\n");
    source.push_str("    let i = socket.raw_send_byte(U8.truncate(input.next_byte()))\n");
    for byte in up.to_string().bytes() { source.push_str(&format!("    api.raw_addr_byte(U8.truncate({byte}))\n")); }
    source.push_str("    let peer = api.raw_connect()\n    let asked = peer.raw_send_byte(U8.truncate(c))\n    let n = socket.raw_send_byte(U8.truncate(peer.raw_recv_byte()))\n    let gone = peer.raw_close()\n");
    for byte in b"greet" { source.push_str(&format!("    exec.raw_name_byte(U8.truncate({byte}))\n")); }
    source.push_str("    let run = exec.raw_run()\n    let e = socket.raw_send_byte(U8.truncate(run.raw_out_byte()))\n");
    // Two directories per connection; one entry each.
    source.push_str("    data.raw_path_byte(U8.truncate(c))\n    let one = socket.raw_send_byte(U8.truncate(48 + data.raw_make_dir()))\n");
    source.push_str("    data.raw_path_byte(U8.truncate(c))\n    data.raw_path_byte(U8.truncate(50))\n    let two = socket.raw_send_byte(U8.truncate(48 + data.raw_make_dir()))\n");
    // The first connection reads a second byte of the file: past its one byte, its worker
    // stops (and what it queued is not sent); the second connection's budget is its own.
    source.push_str("    if c == 97:\n        let g = socket.raw_send_byte(U8.truncate(file.raw_read_byte()))\n    let done = socket.raw_close()\n");
    let manifest = format!("[launch]\nversion = 1\n[listen]\nsocket = \"127.0.0.1:0\"\n[files]\nfile = {{ from = \"cwd\", path = \"f.txt\" }}\n[input]\ninput = {{ stdin = true }}\n[net]\napi = [\"{up}\"]\n[exec]\nexec = {{ dir = \"{}\", programs = [\"greet\"] }}\n[dirs]\ndata = {{ root = {{ from = \"cwd\", path = \"data\" }}, rights = [\"Write\"] }}\n[limits]\nconnections = 2\nconnection-deadline-ms = 3000\ndeadline-ms = 10000\ninput-bytes = 4\nfs-bytes-per-connection = 1\nfs-entries-per-connection = 1\n", root.join("bin").display());
    let built = build(&fixture, "server", &source, &manifest);
    assert!(built.status.success(), "{built:?}");
    let plan = String::from_utf8(fixture.run(&["native-plan", "server.wy", "server.grants.toml"]).stdout).unwrap();
    assert!(plan.contains("fs-bytes-per-connection = 1") && plan.contains("fs-entries-per-connection = 1"), "{plan}");
    let log = root.join("server.stderr");
    let mut server = Command::new(fixture.0.join("server")).current_dir(&root).stdin(Stdio::piped())
        .stderr(Stdio::from(fs::File::create(&log).unwrap())).spawn().unwrap();
    // The input is read once, to its end, before the first connection.
    server.stdin.take().unwrap().write_all(b"IJ").unwrap();
    let endpoint = await_loopback_listener(&mut server, &log, Duration::from_secs(3));
    let mut replies = Vec::new();
    for name in [b'a', b'b'] {
        let mut peer = TcpStream::connect_timeout(&endpoint, Duration::from_secs(3)).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        peer.write_all(&[name]).unwrap();
        let mut reply = Vec::new();
        let _ = peer.read_to_end(&mut reply);
        replies.push(String::from_utf8_lossy(&reply).into_owned());
    }
    let ended = server.wait().unwrap();
    echo.join().unwrap();
    assert!(ended.success(), "{ended:?} {replies:?}");
    assert_eq!(fs::read_to_string(log).unwrap(), format!("witchy: listening on {endpoint}\n"));
    // File and input from their start, the peer's answer, the program's output, one entry
    // made and the second TooLarge (3): each connection's own budget.
    assert_eq!(replies, ["", "FIcg03"]);
    assert!(root.join("data/a").is_dir() && root.join("data/b").is_dir() && !root.join("data/a2").exists());
}

/// The broker stops the program and its descendants when the worker's connection deadline fires.
#[test]
fn manifest_socket_server_stops_exec_at_connection_deadline() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    let fixture = Fixture::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    let started_pids = root.join("connection-pids");
    fs::write(root.join("bin/slow"), format!("#!/bin/sh\n(sleep 5; touch {0}/outlived) &\nprintf '%s %s\\n' \"$$\" \"$!\" > {0}/connection-pids\nsleep 5; touch {0}/outlived\n", root.display())).unwrap();
    fs::set_permissions(root.join("bin/slow"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut slow = String::from("fn main(socket: Socket, exec: Exec):\n    let c = socket.raw_recv_byte()\n");
    for byte in b"slow" { slow.push_str(&format!("    exec.raw_name_byte(U8.truncate({byte}))\n")); }
    slow.push_str("    let run = exec.raw_run()\n    let done = socket.raw_close()\n");
    let slow_plan = format!("[launch]\nversion = 1\n[listen]\nsocket = \"127.0.0.1:0\"\n[exec]\nexec = {{ dir = \"{}\", programs = [\"slow\"] }}\n[limits]\nconnections = 1\nconnection-deadline-ms = 1000\ndeadline-ms = 8000\n", root.join("bin").display());
    // The deadline gives the shell time to record its PIDs before it fires: at 200 ms a host at
    // load 100 stopped the worker before `/bin/sh` had started, and the probe never appeared. The
    // program sleeps 5 s, so only the deadline can stop it within the bound below.
    assert!(build(&fixture, "slow", &slow, &slow_plan).status.success());
    let started = Instant::now();
    let log = root.join("slow.stderr");
    let mut slow_server = Command::new(fixture.0.join("slow")).current_dir(&root)
        .stderr(Stdio::from(fs::File::create(&log).unwrap())).spawn().unwrap();
    let endpoint = await_loopback_listener(&mut slow_server, &log, Duration::from_secs(3));
    let mut peer = TcpStream::connect_timeout(&endpoint, Duration::from_secs(3)).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
    peer.write_all(b"x").unwrap();
    let mut reply = Vec::new();
    let _ = peer.read_to_end(&mut reply);
    let slow_ended = slow_server.wait().unwrap();
    // The connection deadline, not the program's end, stopped the worker. The bound on the time
    // taken leaves room for the worker's sandbox install, which the deadline does not count.
    assert_eq!(fs::read_to_string(log).unwrap(),
        format!("witchy: listening on {endpoint}\nwitchy: limit reached: --connection-deadline-ms\n"), "{slow_ended:?}");
    assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
    assert_exec_probes_stopped(&exec_probe_pids(&started_pids, 2));
    assert!(!root.join("outlived").exists(), "a program outlived its stopped connection: {slow_ended:?}");
}

#[test]
fn development_socket_server_requires_bounded_console_input() {
    let fixture = Fixture::new();
    let reader = "fn main(socket: Socket, input: Console<Read>):\n    let done = socket.raw_close()\n";
    fs::write(fixture.0.join("dev.wy"), reader).unwrap();
    assert!(fixture.run(&["build", "dev.wy", "-o", "dev", "--target=macos-arm64"]).status.success());
    let unbounded = Command::new(fixture.0.join("dev")).args(["--listen=127.0.0.1:0", "--instance-per-connection", "--connections=1", "--input-stdin"]).stdin(std::process::Stdio::null()).output().unwrap();
    refused(&unbounded, "reads its input once, before the first connection");
}

/// The launch plan's directory descriptor determines whether it overlaps the Exec directory.
#[test]
fn manifest_exec_checks_its_granted_directory_for_overlap() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::create_dir_all(root.join("data")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("bin/greet"), "#!/bin/sh\nprintf g\n").unwrap();
    fs::set_permissions(root.join("bin/greet"), fs::Permissions::from_mode(0o755)).unwrap();
    // A combined Dir beside Exec launches, checked on the launch's own descriptor; the exec
    // directory inside it is still refused.
    let source = "fn main(out: Console<Write>, exec: Exec, data: Dir):\n    out.print_int(7)\n";
    let plan = |exec_dir: &Path| format!("[launch]\nversion = 1\n[exec]\nexec = {{ dir = \"{}\", programs = [\"greet\"] }}\n[dirs]\ndata = {{ root = {{ from = \"cwd\", path = \"data\" }}, rights = [\"Read\", \"Write\"] }}\n", exec_dir.display());
    assert!(build(&fixture, "combined", source, &plan(&root.join("bin"))).status.success());
    fs::create_dir_all(root.join("(launch plan directory)")).unwrap();
    let ran = Command::new(fixture.0.join("combined")).current_dir(&root).output().unwrap();
    assert_eq!(ran.stdout, b"7\n", "{ran:?}");
    fs::create_dir_all(root.join("data/bin")).unwrap();
    fs::copy(root.join("bin/greet"), root.join("data/bin/greet")).unwrap();
    assert!(build(&fixture, "inside", source, &plan(&root.join("data/bin"))).status.success());
    refused(&Command::new(fixture.0.join("inside")).current_dir(&root).output().unwrap(), "overlaps a writable");
}

/// SIGTERM to a server mid-connection stops its worker, broker, and running program.
#[test]
fn manifest_server_stop_terminates_its_exec_program() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    let fixture = Fixture::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::create_dir(root.join("bin")).unwrap();
    let started_pid = root.join("server-pid");
    write_exec_fixture(&root.join("bin/slow"), &format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > {0}/server-pid\nsleep 10; touch {0}/outlived\n", root.display()));
    let mut slow = String::from("fn main(socket: Socket, exec: Exec):\n    let c = socket.raw_recv_byte()\n");
    for byte in b"slow" { slow.push_str(&format!("    exec.raw_name_byte(U8.truncate({byte}))\n")); }
    slow.push_str("    let run = exec.raw_run()\n    let done = socket.raw_close()\n");
    let slow_plan = format!("[launch]\nversion = 1\n[listen]\nsocket = \"127.0.0.1:0\"\n[exec]\nexec = {{ dir = \"{}\", programs = [\"slow\"] }}\n[limits]\nconnections = 1\nconnection-deadline-ms = 30000\ndeadline-ms = 60000\n", root.join("bin").display());
    assert!(build(&fixture, "slow", &slow, &slow_plan).status.success());
    // The server picks its own port and announces it: a port probed here and released could be
    // taken by another test's listener before the server binds it.
    let log = root.join("slow.stderr");
    let mut server = Command::new(fixture.0.join("slow")).current_dir(&root).stderr(fs::File::create(&log).unwrap()).spawn().unwrap();
    let endpoint = await_loopback_listener(&mut server, &log, Duration::from_secs(3));
    let mut peer = TcpStream::connect_timeout(&endpoint, Duration::from_secs(3)).unwrap();
    peer.write_all(b"x").unwrap();
    let pids = exec_probe_pids(&started_pid, 1);
    let started = std::time::Instant::now();
    assert!(Command::new("/bin/kill").args(["-TERM", &server.id().to_string()]).status().unwrap().success());
    let _ = server.wait();
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "{:?}", started.elapsed());
    let mut reply = Vec::new();
    let _ = peer.set_read_timeout(Some(std::time::Duration::from_secs(2))); // the stopped server may already have closed it (EINVAL on macOS)
    let _ = peer.read_to_end(&mut reply);
    assert_exec_probes_stopped(&pids);
    assert!(!root.join("outlived").exists(), "a program outlived its stopped server");
}

/// Scan of Grimoire's placement commit: a launcher that checked a root binds it. `identity =
/// { from = "arg", name = ID }` on a directory or read file: the runtime opens the root as the
/// plan says and refuses unless the opened object is the DEVICE:INODE given, so a name swapped
/// after the launcher's check cannot substitute another directory.
#[test]
fn manifest_identity_binds_a_checked_root() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.0.join("data")).unwrap();
    fs::create_dir_all(fixture.0.join("other")).unwrap();
    fs::write(fixture.0.join("data/a.txt"), b"D").unwrap();
    let manifest = "[launch]\nversion = 1\n[flags]\ndata-id = { type = \"object\" }\n[dirs]\nproject = { root = { from = \"cwd\", path = \"data\" }, rights = [\"Read\"], identity = { from = \"arg\", name = \"data-id\" } }\n";
    assert!(build(&fixture, "bound", &reader("Dir<Read>"), manifest).status.success());
    let checked = fs::metadata(fixture.0.join("data")).unwrap();
    let id = format!("{}:{}", checked.dev(), checked.ino());
    assert_eq!(run(&fixture, "bound", &["--data-id", &id]).stdout, b"7\n0\n68\n");
    // Swapped after the check: the name now reaches another directory.
    fs::rename(fixture.0.join("data"), fixture.0.join("checked")).unwrap();
    fs::rename(fixture.0.join("other"), fixture.0.join("data")).unwrap();
    refused(&run(&fixture, "bound", &["--data-id", &id]), "not the one named DEVICE:INODE");
    refused(&run(&fixture, "bound", &[]), "data-id");
}

/// Round 4, item B1: a broker killed mid-run leaves its private directory; a later run with Exec
/// removes those of brokers that no longer exist, and nothing else: not a live broker's, not one
/// of another mode, not a link named like one (nor what it points at).
#[test]
fn stale_exec_directories_are_swept_and_nothing_else() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let root = fixture.0.canonicalize().unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::write(root.join("bin/greet"), "#!/bin/sh\nprintf g\n").unwrap();
    fs::set_permissions(root.join("bin/greet"), fs::Permissions::from_mode(0o755)).unwrap();
    let base = String::from_utf8(Command::new("/usr/bin/getconf").arg("DARWIN_USER_TEMP_DIR").output().unwrap().stdout).unwrap();
    let base = Path::new(base.trim());
    let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
    let dead = gone.id();
    gone.wait().unwrap();
    let tag = format!("{:06}", std::process::id() % 1_000_000);
    let stale = base.join(format!("witchy-exec-{dead}-S{tag}stale"));
    let other_mode = base.join(format!("witchy-exec-{dead}-M{tag}modes"));
    let live = base.join(format!("witchy-exec-{}-L{tag}alive", std::process::id()));
    let link = base.join(format!("witchy-exec-{dead}-K{tag}links"));
    for (dir, mode) in [(&stale, 0o700), (&other_mode, 0o755), (&live, 0o700)] {
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("sub/left"), b"x").unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
    }
    fs::create_dir_all(root.join("target")).unwrap();
    fs::write(root.join("target/kept"), b"k").unwrap();
    std::os::unix::fs::symlink(root.join("target"), &link).unwrap();
    fs::write(fixture.0.join("run.wy"), exec_program(&[("greet", &[], false, None)])).unwrap();
    assert!(fixture.run(&["build", "run.wy", "-o", "run", "--target=macos-arm64"]).status.success());
    let ran = Command::new(fixture.0.join("run")).args(["--output", &format!("--exec={}:greet", root.join("bin").display())]).output().unwrap();
    let swept = !stale.exists();
    let kept = other_mode.join("sub/left").exists() && live.join("sub/left").exists() && link.is_symlink() && root.join("target/kept").exists();
    for dir in [&other_mode, &live] { let _ = fs::remove_dir_all(dir); }
    let _ = fs::remove_file(&link);
    let _ = fs::remove_dir_all(&stale);
    assert_eq!(ran.stdout, b"7\ng0\n", "{ran:?}");
    assert!(swept, "a dead broker's directory was left");
    assert!(kept, "the sweep removed something that was not a dead broker's directory");
}

/// Round 4, item B2: the supervisor records and forgets its worker and broker with its stop
/// signals blocked, so SIGTERM at any moment (before a connection, while one is set up, served
/// or torn down) stops it promptly and leaves no worker or broker behind. A stress test.
#[test]
fn a_server_stopped_at_any_moment_leaves_nothing() {
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::os::unix::process::ExitStatusExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = Fixture::new();
    let source = "fn main(socket: Socket, out: Console<Write>):\n    let c = socket.raw_recv_byte()\n    let sent = socket.raw_send_byte(U8.truncate(c))\n    let done = socket.raw_close()\n";
    fs::write(fixture.0.join("echo.wy"), source).unwrap();
    assert!(fixture.run(&["build", "echo.wy", "-o", "echo", "--target=macos-arm64"]).status.success());
    let binary = fixture.0.join("echo");
    // Keep the ordinary suite short; use a larger count for a dedicated stress run.
    let rounds = std::env::var("WITCHY_TEST_STRESS_ROUNDS").ok()
        .and_then(|value| value.parse::<u64>().ok()).filter(|&n| (1..=100).contains(&n)).unwrap_or(4);
    for round in 0..rounds {
        let stderr = fixture.0.join("echo.stderr");
        let mut server = Command::new(&binary).arg("--listen=127.0.0.1:0")
            .args(["--instance-per-connection", "--connections=1000", "--deadline-ms=20000", "--output"])
            .stdout(std::process::Stdio::null()).stderr(fs::File::create(&stderr).unwrap()).spawn().unwrap();
        let ended = AtomicBool::new(false);
        let require_echo = rounds > 1 && round + 1 == rounds;
        let (echoed, echo) = std::sync::mpsc::channel();
        let echo_timeout = std::time::Duration::from_secs(3);
        let saw_echo = std::thread::scope(|scope| {
            let ended = &ended;
            let stderr = &stderr;
            scope.spawn(move || {
                let load_timeout = if require_echo { echo_timeout } else { std::time::Duration::from_millis(400) };
                let until = std::time::Instant::now() + load_timeout;
                let mut endpoint = None::<SocketAddr>;
                let mut notified = false;
                while !ended.load(Ordering::Relaxed) && std::time::Instant::now() < until {
                    if endpoint.is_none() {
                        endpoint = fs::read_to_string(stderr).unwrap().lines().find_map(|line| {
                            line.strip_prefix("witchy: listening on ")?.parse().ok()
                        });
                    }
                    let Some(endpoint) = endpoint else {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    };
                    match TcpStream::connect_timeout(&endpoint, std::time::Duration::from_millis(50)) {
                        Ok(mut peer) => {
                            if peer.write_all(b"x").is_ok() && require_echo && !notified {
                                let mut reply = [0];
                                let left = until.saturating_duration_since(std::time::Instant::now());
                                if !left.is_zero() && peer.set_read_timeout(Some(left)).is_ok()
                                    && peer.read_exact(&mut reply).is_ok() && reply == *b"x"
                                {
                                    let _ = echoed.send(());
                                    notified = true;
                                }
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(_) => {}
                    }
                }
            });
            // Exercise a real worker in one round even if the earlier stop
            // timers all fire before the server has accepted a connection.
            let saw_echo = !require_echo || echo.recv_timeout(echo_timeout).is_ok();
            std::thread::sleep(std::time::Duration::from_millis(5 + (round * 37) % 150));
            let started = std::time::Instant::now();
            send_signal(server.id(), "-TERM");
            let status = server.wait().unwrap();
            ended.store(true, Ordering::Relaxed);
            const SIGTERM: i32 = 15;
            assert!(
                status.signal() == Some(SIGTERM) || status.code() == Some(128 + SIGTERM),
                "round {round}: {status:?}: {}", String::from_utf8_lossy(&fs::read(&stderr).unwrap())
            );
            assert!(started.elapsed() < std::time::Duration::from_secs(3), "round {round}: {:?}", started.elapsed());
            saw_echo
        });
        let reap_deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
        loop {
            let left = Command::new("/usr/bin/pgrep").arg("-f").arg(binary.to_str().unwrap()).output().unwrap();
            if left.status.code() == Some(1) && left.stdout.is_empty() && left.stderr.is_empty() {
                break;
            }
            assert!(left.status.success() && !left.stdout.is_empty() && left.stderr.is_empty(), "round {round}: {left:?}");
            assert!(std::time::Instant::now() < reap_deadline, "round {round}: processes left: {}", String::from_utf8_lossy(&left.stdout));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(saw_echo, "round {round}: no client received an echo before shutdown");
    }
}

fn send_signal(pid: u32, signal: &str) {
    assert!(Command::new("/bin/kill").args([signal, &pid.to_string()]).status().unwrap().success());
}
