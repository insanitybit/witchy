//! e2e: sandbox grants tests (extracted from tests/e2e.rs).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::sandbox;

use super::support::coven::*;

/// RFC-0013: `witchy sandbox --grants <doc>` mints the capability set from a grant
/// document — binding each `File`/`Dir` `main` parameter to the same-named entry —
/// and cross-checks it against the computed footprint, aborting on an under-grant.
#[test]
fn grant_document_run_binds_by_name_and_cross_checks() {
    sandbox::grant_document_run_binds_by_name_and_cross_checks();
}

#[test]
fn grant_document_file_rights_are_parameter_exact() {
    sandbox::grant_document_file_rights_are_parameter_exact();
}

#[test]
fn grant_document_env_is_name_scoped() {
    sandbox::grant_document_env_is_name_scoped();
}

/// RFC-0005 Stage 2 / RFC-0012: direct `--file` grants are minted as File
/// externrefs in the compiled sandbox path, for both read and write leaf ops.
#[test]
fn sandbox_direct_file_grants_read_and_write() {
    sandbox::sandbox_direct_file_grants_read_and_write();
}

/// SEC-009 regression: `witchy sandbox` for a `Dir`-binding `main` must FAIL CLOSED
/// when no `--dir` is granted (deny by omission) rather than silently defaulting to
/// the cwd — exactly as a `File`-binding `main` requires `--file`. An explicit
/// `--dir` still runs. (The dev `witchy <file>` path keeps its cwd convenience and is
/// not a security boundary, so it is intentionally not covered here.)
#[test]
fn sandbox_dir_requires_explicit_grant() {
    sandbox::sandbox_dir_requires_explicit_grant();
}

/// RFC-0077 rider 2: authority-free mock constructors are a `witchy test`
/// privilege, not a generally available way to mint capability-shaped values.
/// Pin every production entry that can otherwise reach linking or comptime.
#[test]
fn testing_mock_dir_is_rejected_by_production_entry_paths() {
    sandbox::testing_mock_dir_is_rejected_by_production_entry_paths();
}

#[cfg(unix)]
#[test]
fn sandbox_dir_list_rejects_non_utf8_names() {
    sandbox::sandbox_dir_list_rejects_non_utf8_names();
}

/// SEC-004: `crypto.reveal` gates the SIGNING key only. A named `--secret`
/// value-secret (e.g. an OAuth client secret) stays revealable, but the signing key
/// (`--signing-key`) is sign-only — revealing it aborts. Closes the seed-exfiltration
/// hole while preserving the legitimate value-secret use.
#[test]
fn sandbox_reveal_gates_signing_key_only() {
    sandbox::sandbox_reveal_gates_signing_key_only();
}

/// RFC-0058 §1 positive control: the seeded-divergence lever is a fault injector for
/// `witchy parity` that must (a) be INERT on the program-run path and (b) force a
/// DIVERGE on the parity path when armed — the self-test proving the gate can fail.
#[test]
fn seeded_divergence_is_inert_on_run_but_fails_parity() {
    let dir = unique("seeded-div");
    let prog = dir.join("p.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console):\n    console.print(\"hello\")\n    console.print(\"${1 + 2}\")\n",
    )
    .unwrap();
    let p = prog.to_str().unwrap();

    // (a) The program-run path is INERT: identical output + exit whether or not the
    // lever is set. It must never touch real execution.
    let run_off = Command::new(BIN).arg(p).output().unwrap();
    let run_on = Command::new(BIN).arg(p).env("WITCHY_SEEDED_DIVERGENCE", "1").output().unwrap();
    assert_eq!(run_off.status.code(), run_on.status.code(), "seeded lever changed the run-path exit code");
    assert_eq!(stdout(&run_off), stdout(&run_on), "seeded lever perturbed the run-path output — NOT inert");
    assert!(!stdout(&run_off).contains("seeded-divergence"), "run-path output leaked the sentinel");

    // (b) Unset: parity AGREES (exit 0, machine `outcome=agree`).
    let par_off = Command::new(BIN).args(["parity", p]).output().unwrap();
    assert_eq!(par_off.status.code(), Some(0), "parity should agree unset: {}{}", stdout(&par_off), stderr(&par_off));
    assert!(stdout(&par_off).contains("outcome=agree"), "missing agree stats line: {}", stdout(&par_off));

    // (b) Armed: parity DIVERGES with the distinct DIVERGE exit code and `outcome=diverge`.
    let par_on = Command::new(BIN).args(["parity", p]).env("WITCHY_SEEDED_DIVERGENCE", "1").output().unwrap();
    assert_eq!(
        par_on.status.code(),
        Some(3),
        "armed seeded divergence must fail parity with the DIVERGE code: {}{}",
        stdout(&par_on),
        stderr(&par_on)
    );
    assert!(stdout(&par_on).contains("outcome=diverge"), "missing diverge stats line: {}", stdout(&par_on));

    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0058 §1: prove the lever is inert in release by STATIC inspection — the env
/// var name must appear ONLY in the binary's parity command
/// (`src/commands/execution.rs`), never
/// in the compiler/runtime/interpreter crates that back the program-run path.
#[test]
fn seeded_divergence_var_is_absent_from_release_crates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut hits = Vec::new();
    find_str_in_rs(&root.join("crates"), "WITCHY_SEEDED_DIVERGENCE", &mut hits);
    assert!(
        hits.is_empty(),
        "the seeded-divergence lever is referenced on a RELEASE code path (crates/): {hits:?} — it must live only on the `witchy parity` path"
    );
    // Sanity: it IS present in the binary's parity command (else this proves nothing).
    let execution = std::fs::read_to_string(root.join("src/commands/execution.rs")).unwrap();
    assert!(
        execution.contains("WITCHY_SEEDED_DIVERGENCE"),
        "the seeded-divergence lever vanished from src/commands/execution.rs"
    );
}

/// Recursively collect `.rs` files under `dir` that contain `needle` (skipping
/// `target/`). Used to statically prove an env var is off the release code path.
fn find_str_in_rs(dir: &Path, needle: &str, hits: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            find_str_in_rs(&path, needle, hits);
        } else if path.extension().is_some_and(|e| e == "rs")
            && std::fs::read_to_string(&path).is_ok_and(|s| s.contains(needle))
        {
            hits.push(path);
        }
    }
}

/// (BUG-406) `witchy run <project> --net <addr>` must FORWARD the runtime `--net`
/// grant to the inner sandboxed run of the compiled app. The front-end consumes
/// `--net` for its own registry reach; before the fix it dropped it from the app's
/// run, so a program that connects at runtime was compiled and then run with an
/// EMPTY Net allow-list — its grant silently lost. With the fix the app connects;
/// without any `--net` it is still denied (the grant is the discriminator, not an
/// over-grant).
#[test]
fn run_forwards_net_grant_to_the_sandboxed_app() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    // A one-shot loopback echo server on a free port.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let server = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut r = BufReader::new(stream);
            let mut line = String::new();
            let _ = r.read_line(&mut line);
            let _ = r.get_mut().write_all(line.as_bytes());
        }
    });

    // A hermetic project whose app needs Net at runtime.
    let work = unique("net-forward");
    let app = work.join("netapp");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(app.join("witchy.toml"), "[rune]\nname = \"netapp\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(
        app.join("src/netapp.witchy"),
        format!(
            "fn main(console: Console, net: Net):\n    let s = net.connect(\"{addr}\")\n    s.send_line(\"ping\")\n    console.print(s.recv_line())\n"
        ),
    )
    .unwrap();

    // WITH --net: the grant is forwarded, so the app connects and echoes.
    let out = Command::new(BIN)
        .current_dir(&work)
        .args(["run", "netapp", "--net", &addr])
        .output()
        .expect("spawn witchy run");
    assert!(
        out.status.success() && stdout(&out).contains("ping"),
        "net grant must reach the app: status {:?} stdout {} stderr {}",
        out.status.code(),
        stdout(&out),
        stderr(&out),
    );
    server.join().ok();

    // WITHOUT --net: still denied (no silent over-grant).
    let denied = Command::new(BIN)
        .current_dir(&work)
        .args(["run", "netapp"])
        .output()
        .expect("spawn witchy run");
    assert!(
        !denied.status.success()
            && (stdout(&denied).contains("not permitted") || stderr(&denied).contains("not permitted")),
        "no --net must remain denied: stdout {} stderr {}",
        stdout(&denied),
        stderr(&denied),
    );
}
