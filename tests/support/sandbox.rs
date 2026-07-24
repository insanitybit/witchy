use super::support::coven::*;

#[cfg(unix)]
use std::path::PathBuf;
use std::process::{Command, Output};

/// RFC-0013: `witchy sandbox --grants <doc>` mints the capability set from a grant
/// document — binding each `File`/`Dir` `main` parameter to the same-named entry —
/// and cross-checks it against the computed footprint, aborting on an under-grant.
pub(crate) fn grant_document_run_binds_by_name_and_cross_checks() {
    let dir = unique("grants");
    let cfg = dir.join("cfg.txt");
    std::fs::write(&cfg, "config-body").unwrap();
    let prog = dir.join("prog.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console, config: File[Read]):\n    console.print(config.read())\n",
    )
    .unwrap();

    // A sufficient grant binds `config` by name and runs.
    let ok = dir.join("ok.toml");
    std::fs::write(
        &ok,
        format!("[files]\nconfig = {{ path = \"{}\", rights = [\"Read\"] }}\n", cfg.display()),
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", ok.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "grant run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("config-body"), "got: {}", stdout(&out));

    // An under-grant (no `[files].config`) aborts with a clear error, nonzero exit.
    let under = dir.join("under.toml");
    std::fs::write(&under, "[net]\nx = [\"h:1\"]\n").unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", under.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "an under-grant must abort the run");
    assert!(
        stderr(&out).contains("insufficient") || stdout(&out).contains("insufficient"),
        "expected an insufficiency error: out={} err={}",
        stdout(&out),
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

pub(crate) fn grant_document_file_rights_are_parameter_exact() {
    let dir = unique("grant-rights");
    let input = dir.join("input.txt");
    let output = dir.join("output.txt");
    std::fs::write(&input, "in").unwrap();
    let prog = dir.join("prog.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console, input: File[Read], output: File[Write]):\n    console.print(input.read())\n    output.write(\"out\")\n",
    )
    .unwrap();

    let swapped = dir.join("swapped.toml");
    std::fs::write(
        &swapped,
        format!(
            "[files]\ninput = {{ path = \"{}\", rights = [\"Write\"] }}\noutput = {{ path = \"{}\", rights = [\"Read\"] }}\n",
            input.display(),
            output.display()
        ),
    )
    .unwrap();

    let out = Command::new(BIN)
        .args(["sandbox", "--grants", swapped.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "swapped grant rights must abort the run");
    assert!(
        stderr(&out).contains("do not match") || stdout(&out).contains("do not match"),
        "expected exact-rights mismatch: out={} err={}",
        stdout(&out),
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0005 Stage 2 / RFC-0012: direct `--file` grants are minted as File
/// externrefs in the compiled sandbox path, for both read and write leaf ops.
pub(crate) fn sandbox_direct_file_grants_read_and_write() {
    let dir = unique("filegrant");
    let input = dir.join("input.txt");
    let output = dir.join("output.txt");
    std::fs::write(&input, "direct-read").unwrap();

    let read_prog = dir.join("read.witchy");
    std::fs::write(
        &read_prog,
        "fn main(console: Console, config: File[Read]):\n    console.print(config.read())\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--file", input.to_str().unwrap(), read_prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "direct --file read failed: {}\n{}",
        stderr(&out),
        stdout(&out)
    );
    assert!(stdout(&out).contains("direct-read"), "got: {}", stdout(&out));

    let write_prog = dir.join("write.witchy");
    std::fs::write(
        &write_prog,
        "fn main(console: Console, log: File[Write]):\n    log.write(\"direct-write\")\n    console.print(\"wrote\")\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--file", output.to_str().unwrap(), write_prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "direct --file write failed: {}\n{}",
        stderr(&out),
        stdout(&out)
    );
    assert!(stdout(&out).contains("wrote"), "got: {}", stdout(&out));
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "direct-write");

    let _ = std::fs::remove_dir_all(&dir);
}

/// SEC-009 regression: `witchy sandbox` for a `Dir`-binding `main` must FAIL CLOSED
/// when no `--dir` is granted (deny by omission) rather than silently defaulting to
/// the cwd — exactly as a `File`-binding `main` requires `--file`. An explicit
/// `--dir` still runs. (The dev `witchy <file>` path keeps its cwd convenience and is
/// not a security boundary, so it is intentionally not covered here.)
pub(crate) fn sandbox_dir_requires_explicit_grant() {
    let dir = unique("dirgrant");
    let prog = dir.join("prog.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console, dir: Dir):\n    let ok = dir.exists(\"prog.witchy\")\n    console.print(\"${ok}\")\n",
    )
    .unwrap();

    // No --dir: deny by omission — aborts with a clear error, nonzero exit.
    let out = Command::new(BIN).args(["sandbox", prog.to_str().unwrap()]).output().unwrap();
    assert!(!out.status.success(), "a Dir-binding main with no --dir must abort");
    assert!(
        stderr(&out).contains("no subtree was granted") || stdout(&out).contains("no subtree was granted"),
        "expected a deny-by-omission error: out={} err={}",
        stdout(&out),
        stderr(&out)
    );

    // An explicit --dir runs.
    let out = Command::new(BIN)
        .args(["sandbox", "--dir", dir.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "explicit --dir run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("true"), "got: {}", stdout(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
pub(crate) fn sandbox_dir_list_rejects_non_utf8_names() {
    use std::os::unix::ffi::OsStringExt;

    let dir = unique("dir-list-nonutf8");
    let grant = dir.join("grant");
    std::fs::create_dir_all(&grant).unwrap();
    std::fs::write(grant.join("normal.txt"), "ok").unwrap();
    let bad = PathBuf::from(std::ffi::OsString::from_vec(vec![0xbd, 0xb2, b'=', 0xbc]));
    if std::fs::write(grant.join(bad), "hidden").is_err() {
        // Some Unix filesystems reject non-UTF-8 names at creation time. The
        // runtime bug is only observable where such an entry can exist.
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let prog = dir.join("prog.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console, root: Dir):\n    let names = root.list()\n    console.print(\"listed\")\n",
    )
    .unwrap();

    let source = Command::new(BIN)
        .args(["sandbox", "--dir", grant.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!source.status.success(), "source sandbox must reject non-UTF8 names");
    assert!(
        stdout(&source).contains("not valid UTF-8") || stderr(&source).contains("not valid UTF-8"),
        "expected UTF-8 error from source sandbox: out={} err={}",
        stdout(&source),
        stderr(&source)
    );

    let wasm = dir.join("prog.wasm");
    let emit = Command::new(BIN)
        .args(["emit-wasm", prog.to_str().unwrap(), "-o", wasm.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(emit.status.success(), "emit-wasm failed: {}\n{}", stderr(&emit), stdout(&emit));

    let artifact = Command::new(BIN)
        .args(["sandbox", "--dir", grant.to_str().unwrap(), wasm.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!artifact.status.success(), "wasm sandbox must reject non-UTF8 names");
    assert!(
        stdout(&artifact).contains("not valid UTF-8") || stderr(&artifact).contains("not valid UTF-8"),
        "expected UTF-8 error from wasm sandbox: out={} err={}",
        stdout(&artifact),
        stderr(&artifact)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// SEC-004: `crypto.reveal` gates the SIGNING key only. A named `--secret`
/// value-secret (e.g. an OAuth client secret) stays revealable, but the signing key
/// (`--signing-key`) is sign-only — revealing it aborts. Closes the seed-exfiltration
/// hole while preserving the legitimate value-secret use.
pub(crate) fn sandbox_reveal_gates_signing_key_only() {
    let dir = unique("reveal");
    // A named value-secret reveals fine.
    let named = dir.join("named.witchy");
    std::fs::write(
        &named,
        "import crypto\nimport secretstore\nfn main(console: Console, store: SecretStore):\n    console.print(crypto.reveal(secretstore.require(store, \"token\")))\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--secret", "token=s3cr3t", named.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "named-secret reveal failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("s3cr3t"), "got: {}", stdout(&out));

    // The signing key is NOT revealable.
    let seedfile = dir.join("seed.hex");
    std::fs::write(&seedfile, "41".repeat(32)).unwrap();
    let signing = dir.join("signing.witchy");
    std::fs::write(
        &signing,
        "import crypto\nfn main(console: Console, key: Secret):\n    console.print(crypto.reveal(key))\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--signing-key", seedfile.to_str().unwrap(), signing.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "revealing the signing key must abort");
    assert!(
        stderr(&out).contains("not revealable") || stdout(&out).contains("not revealable"),
        "expected a not-revealable error: out={} err={}",
        stdout(&out),
        stderr(&out)
    );

    // RFC-0060: a NAMED secret granted `,use-only` is usable by opaque ref but
    // NOT revealable — the same `token` program that reveals fine above must
    // abort when the grant carries the use-only modifier.
    let out = Command::new(BIN)
        .args(["sandbox", "--secret", "token=s3cr3t,use-only", named.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "revealing a use-only secret must abort: {}\n{}",
        stderr(&out),
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("use-only") || stdout(&out).contains("use-only"),
        "expected a use-only-not-revealable error for the use-only secret: out={} err={}",
        stdout(&out),
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0102: an Env grant document binds a named `Env` parameter to an
/// allow-list. The footprint admits the Env family, while the runtime enforces
/// the individual variable names.
pub(crate) fn grant_document_env_is_name_scoped() {
    let dir = unique("env-grant");
    std::fs::create_dir_all(&dir).unwrap();
    let allowed = dir.join("allowed.witchy");
    let denied = dir.join("denied.witchy");
    let grants = dir.join("grants.toml");
    let no_env = dir.join("no-env.toml");
    let wrong_binding = dir.join("wrong-binding.toml");

    std::fs::write(
        &allowed,
        "import option\nfn main(console: Console, runtime: Env):\n    match runtime.get_env(\"RFC0102_ALLOWED\"):\n        Some(value) -> console.print(value)\n        None -> console.print(\"unset\")\n",
    )
    .unwrap();
    std::fs::write(
        &denied,
        "import option\nfn main(console: Console, runtime: Env):\n    match runtime.get_env(\"RFC0102_DENIED\"):\n        Some(value) -> console.print(value)\n        None -> console.print(\"unset\")\n",
    )
    .unwrap();
    std::fs::write(&grants, "[env]\nruntime = [\"RFC0102_ALLOWED\"]\n").unwrap();
    std::fs::write(&no_env, "").unwrap();
    std::fs::write(&wrong_binding, "[env]\nother = [\"RFC0102_ALLOWED\"]\n").unwrap();

    let permitted = Command::new(BIN)
        .args(["sandbox", "--grants", grants.to_str().unwrap(), allowed.to_str().unwrap()])
        .env("RFC0102_ALLOWED", "visible")
        .output()
        .unwrap();
    assert!(
        permitted.status.success() && stdout(&permitted).contains("visible"),
        "allowed Env name must be readable: out={} err={}",
        stdout(&permitted),
        stderr(&permitted)
    );

    let omitted = Command::new(BIN)
        .args(["sandbox", "--grants", grants.to_str().unwrap(), denied.to_str().unwrap()])
        .env("RFC0102_DENIED", "hidden")
        .output()
        .unwrap();
    assert!(!omitted.status.success(), "an omitted Env name must be denied");
    assert!(
        stdout(&omitted).contains("not in this Env grant's allow-list")
            || stderr(&omitted).contains("not in this Env grant's allow-list"),
        "expected Env allow-list diagnostic: out={} err={}",
        stdout(&omitted),
        stderr(&omitted)
    );

    let missing_family = Command::new(BIN)
        .args(["sandbox", "--grants", no_env.to_str().unwrap(), allowed.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!missing_family.status.success(), "omitting the Env family must fail the footprint check");
    assert!(
        stdout(&missing_family).contains("is insufficient")
            || stderr(&missing_family).contains("is insufficient"),
        "expected footprint diagnostic: out={} err={}",
        stdout(&missing_family),
        stderr(&missing_family)
    );

    let mismatched = Command::new(BIN)
        .args([
            "sandbox",
            "--grants",
            wrong_binding.to_str().unwrap(),
            allowed.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!mismatched.status.success(), "an Env grant for a different parameter must not bind");
    assert!(
        stdout(&mismatched).contains("[env].runtime")
            || stderr(&mismatched).contains("[env].runtime"),
        "expected same-name Env binding diagnostic: out={} err={}",
        stdout(&mismatched),
        stderr(&mismatched)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

pub(crate) fn env_only_is_monotone_and_typed() {
    let dir = unique("env-only");
    std::fs::create_dir_all(&dir).unwrap();
    let allowed = dir.join("allowed.witchy");
    let denied = dir.join("denied.witchy");
    let mistyped = dir.join("mistyped.witchy");
    let grants = dir.join("grants.toml");

    std::fs::write(
        &allowed,
        "import option\nfn main(console: Console, runtime: Env):\n    let public = runtime.only([\"RFC0102_PUBLIC\"])\n    match public.get_env(\"RFC0102_PUBLIC\"):\n        Some(value) -> console.print(value)\n        None -> console.print(\"unset\")\n",
    )
    .unwrap();
    std::fs::write(
        &denied,
        "import option\nfn main(console: Console, runtime: Env):\n    let public = runtime.only([\"RFC0102_PUBLIC\"])\n    let widened = public.only([\"RFC0102_PRIVATE\"])\n    match widened.get_env(\"RFC0102_PRIVATE\"):\n        Some(value) -> console.print(value)\n        None -> console.print(\"unset\")\n",
    )
    .unwrap();
    std::fs::write(
        &mistyped,
        "fn main(runtime: Env):\n    let invalid = runtime.only([1])\n",
    )
    .unwrap();
    std::fs::write(
        &grants,
        "[env]\nruntime = [\"RFC0102_PUBLIC\", \"RFC0102_PRIVATE\"]\n",
    )
    .unwrap();

    let retained = Command::new(BIN)
        .args(["sandbox", "--grants", grants.to_str().unwrap(), allowed.to_str().unwrap()])
        .env("RFC0102_PUBLIC", "visible")
        .output()
        .unwrap();
    assert!(
        retained.status.success() && stdout(&retained).contains("visible"),
        "Env.only must preserve retained names: out={} err={}",
        stdout(&retained),
        stderr(&retained)
    );

    let cannot_widen = Command::new(BIN)
        .args(["sandbox", "--grants", grants.to_str().unwrap(), denied.to_str().unwrap()])
        .env("RFC0102_PRIVATE", "hidden")
        .output()
        .unwrap();
    assert!(!cannot_widen.status.success(), "nested Env.only must not regain authority");
    assert!(
        stdout(&cannot_widen).contains("not in this Env grant's allow-list")
            || stderr(&cannot_widen).contains("not in this Env grant's allow-list"),
        "expected attenuation diagnostic: out={} err={}",
        stdout(&cannot_widen),
        stderr(&cannot_widen)
    );

    let bad_names = Command::new(BIN)
        .args(["check", mistyped.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!bad_names.status.success(), "Env.only must require List(String)");
    assert!(
        stdout(&bad_names).contains("List(String)")
            || stderr(&bad_names).contains("List(String)"),
        "expected Env.only list diagnostic: out={} err={}",
        stdout(&bad_names),
        stderr(&bad_names)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
pub(crate) fn grant_document_exec_is_name_scoped_monotone_and_typed() {
    let dir = unique("exec-grant");
    std::fs::create_dir_all(&dir).unwrap();
    let allowed = dir.join("allowed.witchy");
    let denied = dir.join("denied.witchy");
    let mistyped = dir.join("mistyped.witchy");
    let grants = dir.join("grants.toml");
    let wrong_binding = dir.join("wrong-binding.toml");

    std::fs::write(
        &allowed,
        "fn main(console: Console, root: Dir[Read], runner: Exec):\n    console.print(runner.exec(root, \"bin/echo\", \"hello\", \"\"))\n",
    )
    .unwrap();
    std::fs::write(
        &denied,
        "fn main(console: Console, root: Dir[Read], runner: Exec):\n    let public = runner.only([\"bin/echo\"])\n    let widened = public.only([\"bin/sh\"])\n    console.print(widened.exec(root, \"bin/sh\", \"\", \"\"))\n",
    )
    .unwrap();
    std::fs::write(
        &mistyped,
        "fn main(runner: Exec):\n    let invalid = runner.only([1])\n",
    )
    .unwrap();
    std::fs::write(
        &grants,
        "[dirs]\nroot = { root = \"/\", rights = [\"Read\"] }\n\
         [exec]\nrunner = { programs = [\"bin/echo\", \"bin/sh\"], child-paths = [\"/etc\"] }\n",
    )
    .unwrap();
    std::fs::write(
        &wrong_binding,
        "[dirs]\nroot = { root = \"/\", rights = [\"Read\"] }\n\
         [exec]\nother = [\"bin/echo\"]\n",
    )
    .unwrap();

    let permitted = Command::new(BIN)
        .args(["sandbox", "--grants", grants.to_str().unwrap(), allowed.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        permitted.status.success() && stdout(&permitted).contains("hello"),
        "allowed Exec program must run: out={} err={}",
        stdout(&permitted),
        stderr(&permitted)
    );

    let cannot_widen = Command::new(BIN)
        .args(["sandbox", "--grants", grants.to_str().unwrap(), denied.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!cannot_widen.status.success(), "nested Exec.only must not regain authority");
    assert!(
        stdout(&cannot_widen).contains("not in this Exec grant's allow-list")
            || stderr(&cannot_widen).contains("not in this Exec grant's allow-list"),
        "expected Exec attenuation diagnostic: out={} err={}",
        stdout(&cannot_widen),
        stderr(&cannot_widen)
    );

    let mismatched = Command::new(BIN)
        .args([
            "sandbox",
            "--grants",
            wrong_binding.to_str().unwrap(),
            allowed.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!mismatched.status.success(), "a differently named Exec grant must not bind");
    assert!(
        stdout(&mismatched).contains("[exec].runner")
            || stderr(&mismatched).contains("[exec].runner"),
        "expected same-name Exec binding diagnostic: out={} err={}",
        stdout(&mismatched),
        stderr(&mismatched)
    );

    let bad_programs = Command::new(BIN)
        .args(["check", mistyped.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!bad_programs.status.success(), "Exec.only must require List(String)");
    assert!(
        stdout(&bad_programs).contains("List(String)")
            || stderr(&bad_programs).contains("List(String)"),
        "expected Exec.only list diagnostic: out={} err={}",
        stdout(&bad_programs),
        stderr(&bad_programs)
    );

    let _ = std::fs::remove_dir_all(&dir);
}
