//! Active CLI controls for compiler publication and the native executable.
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

#[path = "wasm_cli/compiler_session.rs"]
mod compiler_session;
use compiler_session::CompilerSession;

#[path = "wasm_cli/diagnostic_proofs.rs"]
mod diagnostic_proofs;

#[cfg(all(unix, any(target_arch = "aarch64", target_arch = "x86_64")))]
#[path = "wasm_cli/vector_object.rs"]
mod vector_object;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir();
        loop {
            let path = root.join(format!(
                "witchy-wasm-cli-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create fixture: {error}"),
            }
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_witchyc"))
            .args(args)
            .current_dir(&self.0)
            .env_clear()
            .env("PATH", "")
            .output()
            .expect("execute compiler")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// For a test that builds, with the configured checker, a program whose subject is not proof but
/// which imports the proved standard library (Vec, String, console): the build shares one proof
/// cache (`docs/wasm-proof-cache.md`) with the other such builds, and they take turns, across
/// threads and test processes alike (a lock file beside it). The library's obligations are then
/// sent to Lean once per checker binding and library source, not once per test; a hit is an
/// obligation that binding's kernel accepted, under a key over its exact Lean module and the
/// library's sources, so nothing is admitted that was not proved. Each such test re-proved the
/// whole library (about 260 obligations) on its own, several at once, and the checker's deadline
/// fired under load (docs/decisions.md, 2026-11-25). With a warm cache these builds do not start
/// Lean; fresh checking is covered by the proof tests, which keep caches of their own. The cache is
/// `WITCHY_PROOF_CACHE` when the test run sets it, else one kept in Cargo's target directory.
/// Hold the returned lock until the build has finished.
#[cfg_attr(not(all(target_arch = "aarch64", target_os = "macos")), allow(dead_code))]
fn share_library_proofs(command: &mut Command) -> fs::File {
    use std::os::unix::fs::DirBuilderExt;
    let cache = std::env::var_os("WITCHY_PROOF_CACHE").map(PathBuf::from).unwrap_or_else(|| {
        let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("witchy-library-proofs");
        // Only its owner may write it: whoever can write a proof cache can claim a proof.
        match fs::DirBuilder::new().mode(0o700).create(&cache) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => panic!("create the shared proof cache: {error}"),
        }
        cache
    });
    // The compiler refuses a cache reached through a link (`/tmp` and `/var` on macOS).
    let cache = fs::canonicalize(cache).unwrap();
    let turn = fs::OpenOptions::new().create(true).truncate(false).write(true)
        .open(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("witchy-library-proofs.lock")).unwrap();
    turn.lock().unwrap();
    command.env("WITCHY_PROOF_CACHE", cache);
    turn
}

#[test]
fn build_is_the_direct_wasm_alias_without_external_tools() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("input.wy"), "fn main():\n    return\n").unwrap();
    for command in ["build", "wasm"] {
        let output = format!("{command}.wasm");
        let result = fixture.run(&[command, "input.wy", "-o", &output]);
        assert!(result.status.success(), "{command}: {:?}", result.stderr);
        assert!(result.stdout.is_empty());
        let bytes = fs::read(fixture.0.join(output)).unwrap();
        assert!(bytes.starts_with(b"\0asm\x01\0\0\0"));
    }
    assert_eq!(
        fs::read(fixture.0.join("build.wasm")).unwrap(),
        fs::read(fixture.0.join("wasm.wasm")).unwrap()
    );
}

#[test]
fn package_check_checks_nopanic_without_granting_an_executable() {
    let fixture = Fixture::new();
    let entry = fixture.0.join("input.wy");
    let main = "\nfn main():\n    return\n";
    fs::write(&entry, format!("nopanic fn tick(clock: &Clock) -> U64:\n    return clock.now_ms()\n{main}")).unwrap();
    let checked = fixture.run(&["wasm-check-package", "input.wy"]);
    assert!(checked.status.success(), "{:?}", checked.stderr);
    let session = CompilerSession::start(&fixture.0, |_| {});
    let (status, reply) = session.request(&["wasm-check-package", entry.to_str().unwrap()]);
    assert_eq!(status, 0, "{reply:?}");
    session.finish();

    let built = fixture.run(&["build", "input.wy", "-o", "output.wasm"]);
    assert!(!built.status.success());
    assert!(String::from_utf8_lossy(&built.stderr).contains("which no parameter of `main` grants"));
    assert!(!fixture.0.join("output.wasm").exists());

    fs::write(entry, format!("nopanic fn bad(x: U64) -> U64:\n    return 1 / x\n{main}")).unwrap();
    let refused = fixture.run(&["wasm-check-package", "input.wy"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("division by zero"));
}

#[test]
fn legacy_build_options_refuse_without_overwriting_output() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("input.wy"), "fn main():\n    return\n").unwrap();
    let destination = fixture.0.join("existing.wasm");
    let sentinel = b"preserve existing output";
    fs::write(&destination, sentinel).unwrap();
    for option in [
        "--library", "--loan-limit", "--executable-calls", "--proof-debug",
        "--fresh-verification", "--native", "--lean",
    ] {
        let result = fixture.run(&["build", option, "input.wy", "-o", "existing.wasm"]);
        assert!(!result.status.success(), "accepted {option}");
        assert_eq!(fs::read(&destination).unwrap(), sentinel);
        assert!(String::from_utf8_lossy(&result.stderr).contains("usage:"));
    }
    // Even a flag-shaped path containing valid source must not revive a legacy option.
    fs::write(fixture.0.join("--proof-debug"), "fn main():\n    return\n").unwrap();
    let result = fixture.run(&["build", "--proof-debug", "-o", "existing.wasm"]);
    assert!(!result.status.success());
    assert_eq!(fs::read(&destination).unwrap(), sentinel);
    let result = fixture.run(&["build", "--library", "library.wy", "input.wy", "-o", "existing.wasm"]);
    assert!(!result.status.success());
    assert_eq!(fs::read(&destination).unwrap(), sentinel);
}

#[test]
fn retired_execution_and_lowering_commands_are_unavailable() {
    let fixture = Fixture::new();
    for args in [
        vec!["lower", "input.wy"],
        vec!["lower-hints", "input.wy"],
        vec!["lower-proofs", "input.wy"],
        vec!["run", "input.wy"],
        vec!["__suite-server", "socket", "log"],
        vec!["__suite-shutdown", "socket"],
    ] {
        let result = fixture.run(&args);
        assert!(!result.status.success(), "accepted {args:?}");
        assert!(String::from_utf8_lossy(&result.stderr).contains("usage:"));
    }
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
}

#[test]
fn large_source_is_checked_without_the_retired_token_ceiling() {
    let fixture = Fixture::new();
    let mut source = String::new();
    for index in 0..64 {
        let previous = if index == 0 { "input".to_string() } else { format!("f{}(input)", index - 1) };
        source.push_str(&format!("fn f{index}(input: U64) -> U64:\n    var value = {previous}\n"));
        for _ in 0..34 { source.push_str("    value += 1\n"); }
        source.push_str("    return value\n");
    }
    source.push_str("fn main(console: Console<Write>):\n    console.print_int(f63(0))\n");
    assert!(source.len() < witchy_two::wasm::MAX_SOURCE_BYTES);
    assert!(witchy_two::lexer::lex(&source).unwrap().len() > 8192);
    fs::write(fixture.0.join("large.wy"), source).unwrap();
    let result = fixture.run(&["wasm-checked", "large.wy", "-o", "large.wasm"]);
    assert!(result.status.success(), "{:?}", result.stderr);
    let selected = (0..64).map(|index| format!("\"f{index}\""))
        .chain(std::iter::once("\"main\"".to_string()))
        // Every program has the call-once trait's methods (2026-11-17, closures that take over owners).
        .chain(["call_once", "discard", "start_task"].iter().map(|m| format!("\"WitchyOnce0.{m}\"")))
        .collect::<Vec<_>>().join(",");
    let report = String::from_utf8(result.stdout).unwrap();
    assert!(report.contains(&format!("\"selected_functions\":[{selected}]")));
    assert!(report.contains("\"all_selected_checked\":true"));
    assert!(fs::read(fixture.0.join("large.wasm")).unwrap().starts_with(b"\0asm\x01\0\0\0"));
}


#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn publishing_session_consumes_multiple_lines_and_reaps_on_eof() {
    use std::io::Write;
    use std::process::Stdio;
    let fixture = Fixture::new();
    fs::write(fixture.0.join("input.wy"), "fn main():\n    return\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_witchyc"))
        .args(["build-session", "input.wy", "-o", "output.wasm"])
        .current_dir(&fixture.0).env_clear().env("PATH", "")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"build first 8974b1bf0323ad70866bf06ea880c502ffc4da79a17905b2146fb1fe7658fdf8\nbuild second 8974b1bf0323ad70866bf06ea880c502ffc4da79a17905b2146fb1fe7658fdf8\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("\"request_id\":\"first\"") && text.contains("\"request_id\":\"second\""));
    assert!(text.contains("\"lean_invocations\":0"));
    assert!(fs::read(fixture.0.join("output.wasm")).unwrap().starts_with(b"\0asm"));
}

#[test]
fn explicit_bad_ready_binding_never_silently_falls_back_or_overwrites() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("input.wy"), "fn main():\n    return\n").unwrap();
    fs::write(fixture.0.join("output.wasm"), "old output").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_witchyc"))
        .args(["build", "input.wy", "-o", "output.wasm"])
        .current_dir(&fixture.0).env_clear().env("PATH", "")
        .env("WITCHY_WASM_READY_BINDING", fixture.0.join("missing-binding"))
        .output().unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(fixture.0.join("output.wasm")).unwrap(), b"old output");
    assert!(String::from_utf8_lossy(&output.stderr).contains("ready binding"));
}

/// `main`'s fixed secrets bind by name: the grant document's order is not `main`'s, and the
/// flags follow `main`, each naming its slot's source and never a value. The module lists the
/// slots, in `main`'s order, for the host to bind.
#[test]
fn fixed_secrets_are_planned_by_name_and_listed_in_the_module() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("input.wy"), "fn main(beta: Secret<Seal>, token: Secret<Reveal>, alpha: Secret<Seal>):\n    return\n").unwrap();
    fs::write(fixture.0.join("app.grants.toml"), "[secrets]\nalpha = { use = \"seal\", from = \"env:KEY_ALPHA\" }\nbeta = { use = \"seal\", from = \"env:KEY_BETA\" }\ntoken = { use = \"reveal-bytes\", from = \"env:REPO_TOKEN\", also_from = [\"fd:3\"] }\n").unwrap();
    let granted = fixture.run(&["wasm-grants", "input.wy", "app.grants.toml"]);
    assert!(granted.status.success(), "{}", String::from_utf8_lossy(&granted.stderr));
    assert_eq!(String::from_utf8(granted.stdout).unwrap(), "--allocator\n--secret-slot=beta:seal:env:KEY_BETA\n--secret-slot=token:reveal-bytes:env:REPO_TOKEN\n--secret-slot=alpha:seal:env:KEY_ALPHA\n");
    // An extra declaration is an error, not a warning.
    fs::write(fixture.0.join("extra.grants.toml"), "[secrets]\nalpha = { use = \"seal\", from = \"env:KEY_ALPHA\" }\nbeta = { use = \"seal\", from = \"env:KEY_BETA\" }\ntoken = { use = \"reveal-bytes\", from = \"env:REPO_TOKEN\" }\nspare = { use = \"seal\", from = \"env:SPARE\" }\n").unwrap();
    let extra = fixture.run(&["wasm-grants", "input.wy", "extra.grants.toml"]);
    assert!(!extra.status.success() && extra.stdout.is_empty());
    assert!(String::from_utf8_lossy(&extra.stderr).contains("[secrets].spare: a fixed secret is declared"));
    let built = fixture.run(&["build", "input.wy", "-o", "input.wasm"]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let bytes = fs::read(fixture.0.join("input.wasm")).unwrap();
    let listed = [&b"\x13witchy.secret-slots\x03"[..], b"\x01\x04beta", b"\x00\x05token", b"\x01\x05alpha"].concat();
    assert!(bytes.windows(listed.len()).any(|w| w == listed), "the module lists its slots in main's order");
}

#[test]
fn compiler_session_refuses_a_stale_review_then_builds_an_independent_input() {
    let fixture = Fixture::new();
    let session = CompilerSession::start(&fixture.0, |_| {});
    let request = |args: &[&str]| session.request(args);
    let write = |name: &str, data: &str| fs::write(fixture.0.join(name), data).unwrap();
    write("value.wy", "pub fn value() -> U64:\n    return 1\n");
    write("main.wy", "import \"value.wy\"\nfn main(console: Console<Write>):\n    console.print_int(value())\n");
    write("grants.toml", "");
    write("result.wasm", "sentinel");
    let (status, review) = request(&["prepare-grants", "main.wy", "grants.toml", ""]);
    assert_eq!(status, 0, "{}", review[1]);
    write("value.wy", "pub fn value() -> U64:\n    return 2\n");
    let commit = ["build-checked", &review[2], "main.wy", "result.wasm"];
    let (status, stale) = request(&commit);
    assert_eq!(status, 1, "{stale:?}");
    assert!(stale[1].contains("source or policy changed during checking"), "{stale:?}");
    let (status, consumed) = request(&commit);
    assert_eq!(status, 1, "{consumed:?}");
    assert!(consumed[1].contains("grant ticket expired or already consumed"), "{consumed:?}");
    assert_eq!(fs::read(fixture.0.join("result.wasm")).unwrap(), b"sentinel");
    write("other.wy", "fn main():\n    return\n");
    let (status, built) = request(&["build", "other.wy", "result.wasm", "3", "64", "16384"]);
    assert_eq!(status, 0, "{}", built[1]);
    assert!(fs::read(fixture.0.join("result.wasm")).unwrap().starts_with(b"\0asm"));
    assert_eq!(request(&["wasm-caps", "other.wy"]).0, 0);
    // A review may name the module's memory sizes; the reviewed build has exactly those, as a
    // plain build with the same sizes does, and sizes out of range are refused at the review.
    write("heap.wy", "fn main(allocator: Allocator, console: Console<Write>):\n    console.print_int(1)\n");
    let built_with = |sizes: &[&str], name: &str| {
        let (status, built) = request(&[&["build", "heap.wy", name], sizes].concat());
        assert_eq!(status, 0, "{}", built[1]);
        fs::read(fixture.0.join(name)).unwrap()
    };
    let (default, larger) = (built_with(&["3", "64", "16384"], "default.wasm"), built_with(&["8", "128", "32768"], "larger.wasm"));
    assert_ne!(default, larger, "the sizes are in the module");
    let reviewed = |sizes: &[&str]| {
        let (status, review) = request(&[&["prepare-grants", "heap.wy", "grants.toml", ""], sizes].concat());
        if status != 0 { return Err(review[1].clone()); }
        let (status, built) = request(&["build-checked", &review[2], "heap.wy", "reviewed.wasm"]);
        assert_eq!(status, 0, "{}", built[1]);
        Ok(fs::read(fixture.0.join("reviewed.wasm")).unwrap())
    };
    assert!(reviewed(&[]).unwrap() == default, "a review that names no sizes builds the default module");
    assert!(reviewed(&["8", "128", "32768"]).unwrap() == larger, "a review's sizes are the reviewed build's");
    for (sizes, why) in [(["2", "64", "16384"], "heap pages must be between 3 and 16384"), (["8", "100", "16384"], "allocation slots must be a power of two"), (["8", "128", "1000"], "stack bytes must be a multiple of 8"), (["x", "128", "16384"], "invalid heap pages")] {
        assert!(reviewed(&sizes).unwrap_err().contains(why), "{sizes:?}");
    }
    assert_eq!(request(&["prepare-grants", "heap.wy", "grants.toml", "", "8", "128"]).0, 1, "sizes are all three or none");
    session.finish();
    assert!(!fixture.0.join("socket").exists());
}

#[test]
fn package_imports_require_the_importing_packages_own_edge() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/packages");
    witchy_two::wasm::caps_by_file(&fixtures.join("declared-app.wy")).unwrap();
    let error = witchy_two::wasm::caps_by_file(&fixtures.join("undeclared.refused.wy")).unwrap_err();
    assert!(error.contains("package @chain does not require hello-world"), "{error}");
}

/// A helper's rearrangement summary (src/compiler/rearrangement.rs) is read from the helper's body,
/// and a caller's proof rests on it. Nothing that outlives one build may keep it apart from that
/// body: in one resident session with one proof cache, the helper is changed between builds so
/// that it leaves the slot full, and the caller that assigns an owner over the slot must then be
/// refused, by `build` and by a reviewed `build-checked`, with the earlier module left in place.
///
/// The refusal is a failed proof, so this needs the proof checker: it runs when
/// `WITCHY_WASM_TEST_READY_BINDING` names a ready binding (with `WITCHY_WASM_TEST_LEAN_ROOT` if
/// the binding needs it) and says so when it does not. Its own variables, because the
/// compiler's `WITCHY_WASM_READY_BINDING` set for a whole test run would give a checker to the
/// tests that expect none (`wasm_scalar.rs`). The parts that need no checker are unit tests
/// (`rearrangement_tests.rs`, `a_changed_helper_body_changes_what_its_caller_must_prove`).
#[test]
fn a_session_never_reuses_a_helper_summary_after_the_helper_leaves_the_slot_full() {
    session_refuses_a_changed_helper(0);
}

#[test]
fn a_session_never_reuses_a_helper_summary_after_the_helper_may_leave_the_slot_full() {
    session_refuses_a_changed_helper(1);
}

/// One changed helper per test (`case` picks it): each case is a dozen checked builds.
fn session_refuses_a_changed_helper(case: usize) {
    use std::os::unix::fs::PermissionsExt;

    let Some(binding) = std::env::var_os("WITCHY_WASM_TEST_READY_BINDING") else {
        eprintln!("skipped: WITCHY_WASM_TEST_READY_BINDING is not set, and the refusal is a failed proof");
        return;
    };
    let fixture = Fixture::new();
    // The proof cache refuses a directory reached through a link (`/var` on macOS).
    let root = fs::canonicalize(&fixture.0).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(root.join("proofs")).unwrap();
    fs::set_permissions(root.join("proofs"), fs::Permissions::from_mode(0o700)).unwrap();
    let session = CompilerSession::start(&root, |command| {
        command.env("WITCHY_WASM_READY_BINDING", binding)
            .env("WITCHY_PROOF_CACHE", root.join("proofs"));
        if let Some(lean) = std::env::var_os("WITCHY_WASM_TEST_LEAN_ROOT") {
            command.env("WITCHY_WASM_LEAN_ROOT", lean);
        }
    });
    let request = |args: &[&str]| {
        // The proof helper's deadline fires spuriously on a busy machine: ask again.
        for attempt in 0.. {
            let (status, fields) = session.request(args);
            if attempt < 3 && args[0] == "build" && fields.iter().any(|f| f.contains("native diagnostic deadline")) { continue; }
            return (status, fields);
        }
        unreachable!()
    };
    let write = |name: &str, data: &str| fs::write(root.join(name), data).unwrap();
    let helper = |body: &str| format!("pub type Slot<T>:\n    Vacant\n    Full(T)\n\npub nopanic fn drain<T: Any>(slot: &mut Slot<T>) -> Slot<T>:\n{body}");
    const EXCHANGE: &str = "    memory.exchange(&mut *slot, &mut previous)\n";
    // Leaves the slot vacant and returns what was in it.
    let empties = helper(&format!("    var previous: Slot<T> = Vacant\n{EXCHANGE}    return move previous\n"));
    // Exchanges twice: the slot holds what it held, which may be an owner.
    let leaves_full = helper(&format!("    var previous: Slot<T> = Vacant\n{EXCHANGE}{EXCHANGE}    return move previous\n"));
    // Not a rearrangement at all (a branch): the caller knows nothing of the slot.
    let may_empty = helper(&format!("    var previous: Slot<T> = Vacant\n    match &*slot:\n        Vacant:\n            return move previous\n        Full(_):\n            ()\n{EXCHANGE}    return move previous\n"));
    write("main.wy", "import \"helper.wy\"\n\ntype Shelf<T>:\n    slot: Slot<T>\n\nnopanic fn swap_in<T: Any>(shelf: &mut Shelf<T>, item: T) -> Slot<T>:\n    let held = drain(&mut (*shelf).slot)\n    (*shelf).slot = Full(move item)\n    return move held\n\nfn main(out: Console<Write>):\n    out.print_int(1)\n");
    write("grants.toml", "");
    let build = ["build", "main.wy", "result.wasm", "3", "64", "16384"];
    const REFUSAL: &str = "not proved: an owner assigned over a value that may still hold one";
    let refused = |fields: &[String]| fields.iter().any(|f| f.contains(REFUSAL));

    write("helper.wy", &empties);
    let (status, built) = request(&build);
    assert_eq!(status, 0, "{built:?}");
    let accepted = fs::read(root.join("result.wasm")).unwrap();
    assert!(accepted.starts_with(b"\0asm"));
    // The same source again is the session's image and the cache's proofs: still accepted.
    assert_eq!(request(&build).0, 0);

    for (name, changed) in [("leaves the slot full", &leaves_full), ("may leave the slot full", &may_empty)].into_iter().skip(case).take(1) {
        // The caller's file is untouched; only the helper's body differs.
        write("helper.wy", changed);
        let (status, built) = request(&build);
        assert_eq!(status, 1, "a helper that {name} was accepted: {built:?}");
        assert!(refused(&built), "{name}: {built:?}");
        assert_eq!(fs::read(root.join("result.wasm")).unwrap(), accepted, "{name}: the refused build wrote a module");
        // The reviewed route: reviewed with the changed helper, it is refused when built.
        let (status, review) = request(&["prepare-grants", "main.wy", "grants.toml", ""]);
        assert_eq!(status, 0, "{}", review[1]);
        let (status, built) = request(&["build-checked", &review[2], "main.wy", "result.wasm"]);
        assert!(status == 1 && refused(&built), "{name}, reviewed: {built:?}");
        assert_eq!(fs::read(root.join("result.wasm")).unwrap(), accepted);
        // Reviewed with the good helper, which is changed before the build: the review is stale.
        write("helper.wy", &empties);
        let (status, review) = request(&["prepare-grants", "main.wy", "grants.toml", ""]);
        assert_eq!(status, 0, "{}", review[1]);
        write("helper.wy", changed);
        let (status, stale) = request(&["build-checked", &review[2], "main.wy", "result.wasm"]);
        assert_eq!(status, 1, "{name}, stale review: {stale:?}");
        assert!(stale[1].contains("source or policy changed during checking"), "{name}, stale review: {stale:?}");
        assert_eq!(fs::read(root.join("result.wasm")).unwrap(), accepted);
        // And back: the good helper is accepted again, and the changed one refused again after it.
        write("helper.wy", &empties);
        assert_eq!(request(&build).0, 0, "{name}: the restored helper");
        write("helper.wy", changed);
        let (status, built) = request(&build);
        assert!(status == 1 && refused(&built), "{name}, after the good build was seen again: {built:?}");
    }
    session.finish();
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_slice_reads_the_selected_field_and_requires_the_output_grant() {
    use std::os::unix::process::ExitStatusExt;

    let fixture = Fixture::new();
    let source = r#"type Parcel:
    tag: U64
    data: [U64; 3]

fn read(values: &[U64], index: U64) -> U64:
    var sum: U64 = 0
    for i in 0..values.len():
        sum += values[i]
    for item in values:
        sum += *item
    return sum + values[index]

fn read_parcel(parcel: &Parcel, index: U64) -> U64:
    return read(parcel.data.slice(), index)

fn main(console: Console<Write>):
    let parcel = Parcel(999, [11, 22, 33])
    console.print_int(read_parcel(&parcel, 1) + 1)
"#;
    let input = fixture.0.join("input.wy");
    let executable = fixture.0.join("program");
    fs::write(&input, source).unwrap();
    let build = || fixture.run(&["build", "input.wy", "-o", "program", "--target=macos-arm64"]);
    let result = build();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let original = fs::read(&executable).unwrap();
    let run = |args: &[&str]| Command::new(&executable).args(args).output().unwrap();
    let refused = run(&[]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(refused.stdout.is_empty());
    let result = run(&["--output"]);
    assert!(result.status.success(), "{:?}", result.stderr);
    assert_eq!(result.stdout, b"155\n");

    // The walks' structural bounds apply only to their own indexed reads.
    fs::write(
        &input,
        source.replace("read_parcel(&parcel, 1)", "read_parcel(&parcel, 3)"),
    )
    .unwrap();
    let rebuilt = fixture.run(&["build", "input.wy", "-o", "bad-index", "--target=macos-arm64"]);
    assert!(
        rebuilt.status.success(),
        "{}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    let out_of_bounds = Command::new(fixture.0.join("bad-index"))
        .arg("--output")
        .output()
        .unwrap();
    // The native trap terminates the process with SIGILL or SIGTRAP on macOS.
    assert!(
        matches!(out_of_bounds.status.signal(), Some(4 | 5)),
        "{out_of_bounds:?}"
    );
    assert!(out_of_bounds.stdout.is_empty());

    // A refused rebuild leaves the executable intact. (This used `main(input: Console<Read>)`,
    // refused until native standard input landed in 2026-11-22; it now builds, so a program the
    // checker refuses stands in.)
    fs::write(input, "fn main(console: Console<Write>):\n    console.print_int(undeclared)\n").unwrap();
    let refused = build();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("undeclared"), "{refused:?}");
    assert_eq!(fs::read(&executable).unwrap(), original);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_scalar_locals_cross_branches_loops_and_shared_borrows() {
    let fixture = Fixture::new();
    let source = r#"fn read(value: &U64) -> U64:
    return *value

fn positive_quotient(divisor: U64) -> Bool:
    return 12 / divisor > 0

fn run(limit: U64, use_borrow: Bool) -> U64:
    var borrowed: U64 = 11
    var total: U64 = 0
    for i in 0..limit:
        if use_borrow && i % 2 == 0:
            total += read(&borrowed)
        else:
            total += read(&(i + 0))
        borrowed += 1
    return total + borrowed

fn main(console: Console<Write>):
    let first = run(4, true)
    if first == 43 || positive_quotient(0):
        console.print_int(first)
    let second = run(4, false)
    if !(second != 21 && positive_quotient(0)):
        console.print_int(second)
"#;
    fs::write(fixture.0.join("input.wy"), source).unwrap();
    let result = fixture.run(&["build", "input.wy", "-o", "program", "--target=macos-arm64"]);
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let result = Command::new(fixture.0.join("program")).arg("--output").output().unwrap();
    assert!(result.status.success(), "{:?}", result.stderr);
    assert_eq!(result.stdout, b"43\n21\n");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_inlined_guard_keeps_the_callers_slice_check() {
    use std::os::unix::process::ExitStatusExt;

    let fixture = Fixture::new();
    let source = r#"fn present(s: &[U64], i: U64) -> Bool:
    return i < s.len() && s[i] != 0

fn next(i: U64) -> U64:
    return i + 1

fn main(console: Console<Write>):
    let a = [7, 8, 9]
    let s = a.slice()
    if present(s, 0) && !present(s, 3):
        console.print_int(s[next(1)])
"#;
    let binary = fixture.0.join("program");
    for bad_index in [false, true] {
        let input = if bad_index { source.replace("next(1)", "next(2)") } else { source.into() };
        fs::write(fixture.0.join("input.wy"), input).unwrap();
        let built = fixture.run(&["build", "input.wy", "-o", "program", "--target=macos-arm64"]);
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        let symbols = Command::new("nm").arg("-j").arg(&binary).output().unwrap();
        assert!(symbols.status.success());
        let names = String::from_utf8(symbols.stdout).unwrap();
        assert!(names.lines().any(|name| name == "_witchy_entry"));
        assert!(!names.contains("_witchy_function_"), "{names}");
        let run = Command::new(&binary).arg("--output").output().unwrap();
        if bad_index {
            assert!(matches!(run.status.signal(), Some(4 | 5)), "{run:?}");
        } else {
            assert!(run.status.success());
            assert_eq!(run.stdout, b"9\n");
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_aggregate_call_results_preserve_lanes() {
    let fixture = Fixture::new();
    let source = r#"fn flip(a: [U64; 2]) -> [U64; 2]:
    return [a[1], a[0]]

fn pair(a: &U64) -> [U64; 2]:
    return [*a, *a + 1]

fn main(console: Console<Write>):
    let n: U64 = 8
    let a = pair(&n)
    let b = flip(a)
    console.print_int(b[0] * 10 + b[1])
"#;
    fs::write(fixture.0.join("input.wy"), source).unwrap();
    let built = fixture.run(&["build", "input.wy", "-o", "program", "--target=macos-arm64"]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let run = Command::new(fixture.0.join("program")).arg("--output").output().unwrap();
    assert!(run.status.success(), "{:?}", run.stderr);
    assert_eq!(run.stdout, b"98\n");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_shared_parameter_reads_respect_paths_and_loan_lifetimes() {
    let fixture = Fixture::new();
    let source = r#"type Row:
    cells: [U64; 3]
    factor: U64

fn plus_one(value: U64) -> U64:
    return value + 1

fn across(x: &U64) -> U64:
    return plus_one(*x) + *x

fn borrowed(x: &U64) -> U64:
    return *x + across(x) + *x

fn weighted(row: &Row) -> U64:
    var total: U64 = 0
    for cell in row.cells:
        total += *cell * row.factor
    return total

fn subset(row: &Row, count: U64, index: U64) -> U64:
    var total: U64 = 0
    var i: U64 = 0
    while i < count:
        total += row.cells[index] * row.factor
        i += 1
    return total

fn main(console: Console<Write>):
    let row = Row([1, 2, 3], 2)
    console.print_int(weighted(&row))
    console.print_int(subset(&row, 0, 99))
    var x: U64 = 5
    console.print_int(borrowed(&x))
    x += 3
    console.print_int(across(&x))
"#;
    fs::write(fixture.0.join("input.wy"), source).unwrap();
    let built = fixture.run(&["build", "input.wy", "-o", "program", "--target=macos-arm64"]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let run = Command::new(fixture.0.join("program")).arg("--output").output().unwrap();
    assert!(run.status.success(), "{:?}", run.stderr);
    assert_eq!(run.stdout, b"12\n0\n21\n17\n");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_forwarded_results_still_consume_logical_frame_budget() {
    let fixture = Fixture::new();
    let mut source = String::from(
        "fn make(x: U64) -> [U64; 32]:\n    return [x; 32]\n\nfn main(console: Console<Write>):\n",
    );
    // Each binding uses 32 logical lanes for its local and 32 for the call.
    // The console local takes the frame past 64 KiB at the 128th binding.
    for i in 0..128 {
        source.push_str(&format!("    let v{i} = make({i})\n"));
    }
    source.push_str("    console.print_int(v127[0])\n");
    fs::write(fixture.0.join("input.wy"), source).unwrap();
    let built = fixture.run(&["build", "input.wy", "-o", "program", "--target=macos-arm64"]);
    assert!(!built.status.success());
    assert!(String::from_utf8_lossy(&built.stderr).contains("64 KiB native frame limit"));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_float_bits_and_buffer_owner_results_match_existing_fixtures() {
    for (source, expected, allocator) in [
        (
            r#"fn main(console: Console<Write>):
    let nan = 0.0 / 0.0
    console.print_int(F64.bits(-nan))
    console.print_int(F64.bits(2.0))
    console.print_int(F64.bits(F64.from_bits(0x8000_0000_0000_0000)))
    console.print_int(F64.bits(F64.from_bits(1)))
    console.print_int(F64.bits(3.141592653589793))
"#,
            b"9221120237041090560\n4611686018427387904\n9223372036854775808\n1\n4614256656552045848\n".as_slice(),
            false,
        ),
        (
            include_str!("fixtures/buffer/try-init-buffers.wy"),
            include_bytes!("fixtures/buffer/try-init-buffers.expected").as_slice(),
            true,
        ),
        (
            include_str!("fixtures/native-layout/packed.wy"),
            include_bytes!("fixtures/native-layout/packed.expected").as_slice(),
            true,
        ),
    ] {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("input.wy"), source).unwrap();
        let built = fixture.run(&["build", "input.wy", "-o", "program", "--target=macos-arm64"]);
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));

        let executable = fixture.0.join("program");
        let mut command = Command::new(executable);
        command.arg("--output");
        if allocator {
            let denied = command.output().unwrap();
            assert_eq!(denied.status.code(), Some(2));
            assert!(denied.stdout.is_empty());
            assert!(String::from_utf8_lossy(&denied.stderr).contains("Allocator requires --allocator"));
            command.arg("--allocator");
        }
        let result = command.output().unwrap();
        assert!(result.status.success(), "{:?}", result.stderr);
        assert_eq!(result.stdout, expected);
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"))]
#[path = "wasm_cli/native_linux.rs"]
mod native_linux;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[path = "wasm_cli/native_macos.rs"]
mod native_macos;

#[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
#[path = "wasm_cli/native_file_reads.rs"]
mod native_file_reads;

#[cfg(all(target_arch = "aarch64", any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
#[path = "wasm_cli/native_tasks.rs"]
mod native_tasks;

#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
#[path = "wasm_cli/native_manifest.rs"]
mod native_manifest;
