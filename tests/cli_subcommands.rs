//! CLI regression tests for the `witchy` subcommand-consistency fixes: several
//! subcommands used to bypass `mode opt` enforcement, mis-handle a compiled
//! `.wasm` artifact's launch, or mis-count in-language tests. Each test drives the
//! real `witchy` binary (`CARGO_BIN_EXE_witchy`) and is hermetic (its own temp
//! dir), so they can run in parallel. Bug numbers reference `bugs/`.

use std::path::PathBuf;
use std::process::Command;

use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction, MemorySection, MemoryType, Module, TypeSection, ValType,
};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

/// A fresh, unique temp directory for one test.
fn workdir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("witchy-cli-{tag}-{}-{nanos}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &std::path::Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p.to_str().unwrap().to_string()
}

/// A legacy/external wasm module with no `witchy.launch` metadata. `call_args`
/// optionally makes `run` call the import with i32 constants; omitting it tests
/// whether an import family is linked without needing valid operation inputs.
fn legacy_import_module(
    name: &str,
    params: &[ValType],
    results: &[ValType],
    call_args: Option<&[i32]>,
) -> Vec<u8> {
    let mut types = TypeSection::new();
    types.ty().function(params.iter().copied(), results.iter().copied());
    types.ty().function([], []);

    let mut imports = ImportSection::new();
    imports.import("witchy", name, EntityType::Function(0));

    let mut functions = FunctionSection::new();
    functions.function(1);

    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("run", ExportKind::Func, 1);

    let mut run = Function::new([]);
    if let Some(args) = call_args {
        for arg in args {
            run.instruction(&Instruction::I32Const(*arg));
        }
        run.instruction(&Instruction::Call(0));
        for _ in results {
            run.instruction(&Instruction::Drop);
        }
    }
    run.instruction(&Instruction::End);
    let mut code = CodeSection::new();
    code.function(&run);

    let mut module = Module::new();
    module.section(&types);
    module.section(&imports);
    module.section(&functions);
    module.section(&memories);
    module.section(&exports);
    module.section(&code);
    module.finish()
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn crypto_verify_malformed_inputs_are_result_errors() {
    let dir = workdir("crypto-verify-result");
    let p256_basepoint = "046b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c2964fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5";
    let src = write(
        &dir,
        "crypto_result.witchy",
        &format!(
            "import crypto\nfn main(console: Console):\n    match crypto.ed25519_verify(\"not-hex\", \"msg\", \"00\"):\n        Err(e) -> console.print(crypto.verify_error_message(e))\n        Ok(_v) -> console.print(\"unexpected ed25519\")\n    match crypto.ecdsa_p256_verify_hex(\"{p256_basepoint}\", \"zz\", \"00\"):\n        Err(e) -> console.print(crypto.verify_error_message(e))\n        Ok(_v) -> console.print(\"unexpected ecdsa\")\n"
        ),
    );
    let out = run(&[&src]);
    assert!(out.status.success(), "run failed: {}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Ed25519 public key is malformed\nECDSA P-256 message is malformed\n"
    );

    let parity = run(&["parity", &src]);
    assert!(
        parity.status.success() && String::from_utf8_lossy(&parity.stdout).contains("outcome=agree"),
        "parity failed: {}{}",
        String::from_utf8_lossy(&parity.stdout),
        String::from_utf8_lossy(&parity.stderr)
    );
}

// A file that violates `mode opt` (a heap parameter with no ownership convention).
// `check` rejects it; every other compile-ish path must too.
const BAD_OPT: &str = "mode opt\n\nfn helper(xs: List(Int)) -> Int:\n    var acc = 0\n    for x in xs:\n        acc = acc + x\n    acc\n\nfn main(console: Console):\n    console.print(\"sum: ${helper([1, 2, 3])}\")\n";

// ---- BUG-104 / BUG-106: compiled `.wasm` artifact launch ----

#[test]
fn wasm_int_main_is_the_exit_code_not_printed() {
    // BUG-104: `witchy <file>` of a source whose `main` returns Int exits with it;
    // the compiled `.wasm` form used to PRINT the value and exit 0 instead.
    let dir = workdir("wasm-exit");
    let src = write(&dir, "seven.witchy", "fn main() -> Int:\n    7\n");
    let wasm = dir.join("seven.wasm");
    let out = run(&["emit-wasm", &src, "-o", wasm.to_str().unwrap()]);
    assert!(out.status.success(), "emit-wasm failed: {}", String::from_utf8_lossy(&out.stderr));

    for launcher in [vec![wasm.to_str().unwrap()], vec!["sandbox", wasm.to_str().unwrap()]] {
        let out = run(&launcher);
        assert_eq!(out.status.code(), Some(7), "wasm Int main should be the exit code ({launcher:?})");
        assert!(
            String::from_utf8_lossy(&out.stdout).trim().is_empty(),
            "the Int return must NOT be printed ({launcher:?}): {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

#[test]
fn wasm_nil_main_int_output_is_not_eaten_as_exit_code() {
    // The BUG-104 guard: a Nil `main` that prints ints keeps every line and exits 0.
    let dir = workdir("wasm-print");
    let src = write(&dir, "p.witchy", "fn main(console: Console):\n    console.print(\"42\")\n    console.print(\"99\")\n");
    let wasm = dir.join("p.wasm");
    assert!(run(&["emit-wasm", &src, "-o", wasm.to_str().unwrap()]).status.success());
    let out = run(&[wasm.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "42\n99\n");
}

#[test]
fn sandbox_wasm_requires_explicit_dir_grant() {
    // BUG-106: `sandbox <dir-importing.wasm>` must require `--dir`, like the source
    // form — not silently run against the cwd.
    let dir = workdir("wasm-dir");
    let src = write(
        &dir,
        "d.witchy",
        "fn main(console: Console, dir: Dir) -> Int:\n    if dir.exists(\"x\"):\n        console.print(\"yes\")\n    else:\n        console.print(\"no\")\n    0\n",
    );
    let wasm = dir.join("d.wasm");
    assert!(run(&["emit-wasm", &src, "-o", wasm.to_str().unwrap()]).status.success());

    let denied = run(&["sandbox", wasm.to_str().unwrap()]);
    assert!(!denied.status.success(), "sandbox must refuse a Dir-importing wasm with no --dir");
    assert!(String::from_utf8_lossy(&denied.stderr).contains("Dir"));

    let granted = run(&["sandbox", "--dir", dir.to_str().unwrap(), wasm.to_str().unwrap()]);
    assert!(granted.status.success(), "sandbox --dir must run it: {}", String::from_utf8_lossy(&granted.stderr));
}

// ---- BUG-112: SecretStore is mintable empty ----

#[test]
fn secretstore_main_runs_without_a_secret() {
    // BUG-112: a `main(Console, SecretStore)` runs on both backends with an empty
    // store, so `run`/`sandbox` must NOT demand `--secret`; align with `parity`.
    let dir = workdir("secretstore");
    let src = write(&dir, "s.witchy", "fn main(console: Console, store: SecretStore):\n    console.print(\"hi\")\n");
    for args in [vec![src.as_str()], vec!["sandbox", "--dir", dir.to_str().unwrap(), src.as_str()], vec!["parity", src.as_str()]] {
        let out = run(&args);
        assert_eq!(out.status.code(), Some(0), "{args:?} should exit 0: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn wasm_preserves_an_unused_root_secret_contract() {
    // BUG-113: imports alone cannot reveal an unused capability parameter. The
    // artifact's launch metadata must preserve the source-level `Secret` request.
    let dir = workdir("wasm-unused-secret");
    let src = write(
        &dir,
        "unused_secret.witchy",
        "fn main(console: Console, key: Secret):\n    console.print(\"must not run\")\n",
    );
    let wasm = dir.join("unused_secret.wasm");
    let emit = run(&["emit-wasm", &src, "-o", wasm.to_str().unwrap()]);
    assert!(emit.status.success(), "emit-wasm failed: {}", String::from_utf8_lossy(&emit.stderr));

    for program in [src.as_str(), wasm.to_str().unwrap()] {
        let denied = run(&[program]);
        let error = String::from_utf8_lossy(&denied.stderr);
        assert!(!denied.status.success(), "{program} must require its declared root Secret");
        assert!(
            error.contains("Secret") && error.contains("--signing-key"),
            "source and artifact should expose the same launch requirement: {error}",
        );
        assert!(String::from_utf8_lossy(&denied.stdout).is_empty());
    }
}

#[test]
fn legacy_reveal_import_receives_the_signing_key_grant() {
    // BUG-427: the ABI import is `crypto_reveal_len`, not the source spelling
    // `crypto.reveal`. Classification must install the granted key before the
    // reveal host applies the sign-only policy.
    let dir = workdir("wasm-reveal-import");
    let wasm = dir.join("legacy_reveal.wasm");
    std::fs::write(
        &wasm,
        legacy_import_module(
            "crypto_reveal_len",
            &[ValType::I32],
            &[ValType::I32],
            Some(&[0]),
        ),
    )
    .unwrap();
    let seed = dir.join("seed.hex");
    std::fs::write(&seed, "41".repeat(32)).unwrap();

    let out = run(&[
        "sandbox",
        "--signing-key",
        seed.to_str().unwrap(),
        wasm.to_str().unwrap(),
    ]);
    let error = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(!out.status.success(), "the signing key must remain non-revealable");
    assert!(error.contains("not revealable"), "expected the reveal policy guard: {error}");
    assert!(!error.contains("no secret at handle"), "the granted key was not installed: {error}");
}

#[test]
fn legacy_clock_and_tls_import_variants_are_classified() {
    let dir = workdir("wasm-import-variants");

    let clock = dir.join("clock.wasm");
    std::fs::write(
        &clock,
        legacy_import_module("now_monotonic", &[], &[ValType::I64], Some(&[])),
    )
    .unwrap();
    let clock_run = run(&[clock.to_str().unwrap()]);
    assert!(
        clock_run.status.success(),
        "now_monotonic must receive the Clock family: {}",
        String::from_utf8_lossy(&clock_run.stderr),
    );

    let tls = dir.join("tls.wasm");
    std::fs::write(
        &tls,
        legacy_import_module(
            "net_listen_tls",
            &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
            &[ValType::I32],
            None,
        ),
    )
    .unwrap();
    let seed = dir.join("tls-seed.hex");
    std::fs::write(&seed, "42".repeat(32)).unwrap();

    let no_key = run(&[
        "sandbox",
        "--net",
        "127.0.0.1:0",
        tls.to_str().unwrap(),
    ]);
    assert!(!no_key.status.success());
    assert!(String::from_utf8_lossy(&no_key.stderr).contains("Secret"));

    let granted = run(&[
        "sandbox",
        "--net",
        "127.0.0.1:0",
        "--signing-key",
        seed.to_str().unwrap(),
        tls.to_str().unwrap(),
    ]);
    assert!(
        granted.status.success(),
        "net_listen_tls must receive Net Listen and Secret families: {}",
        String::from_utf8_lossy(&granted.stderr),
    );
}

#[test]
fn named_secret_does_not_satisfy_a_bare_secret() {
    // BUG-116: a bare `Secret` main parameter is the ROOT signing-key handle
    // (handle 0). A `--secret name=value` populates a SecretStore; if it also
    // satisfied the bare Secret, the named value would land at handle 0 and
    // `crypto.reveal(key)` would leak it as the root key. Only `--signing-key`
    // mints a bare Secret.
    let dir = workdir("bare-secret");
    let src = write(
        &dir,
        "s.witchy",
        "import crypto\n\nfn main(console: Console, key: Secret):\n    console.print(crypto.reveal(key))\n",
    );
    for pre in [vec![], vec!["sandbox", "--dir", dir.to_str().unwrap()]] {
        let mut args = pre.clone();
        args.push("--secret");
        args.push("token=abc123");
        args.push(src.as_str());
        let out = run(&args);
        assert!(
            !out.status.success(),
            "{args:?}: a named secret must NOT satisfy a bare Secret"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !combined.contains("abc123"),
            "{args:?}: the named secret must never be revealed as the root key: {combined}"
        );
    }
}

// ---- BUG-119 / BUG-163 / BUG-177: mode-opt enforced consistently ----

#[test]
fn mode_opt_enforced_across_subcommands() {
    let dir = workdir("mode-opt");
    let bad = write(&dir, "bad.witchy", BAD_OPT);
    // `check` is the reference: it rejects.
    assert!(!run(&["check", &bad]).status.success(), "check must reject the mode-opt violation");
    // BUG-163 emit-wat, BUG-119 parity, BUG-177 stats + test must all reject too.
    assert!(!run(&["emit-wat", &bad]).status.success(), "BUG-163: emit-wat must enforce mode opt");
    assert!(!run(&["parity", &bad]).status.success(), "BUG-119: parity must enforce mode opt");
    assert!(!run(&["stats", &bad]).status.success(), "BUG-177: stats must enforce mode opt");
    assert!(!run(&["test", &bad]).status.success(), "BUG-177: test must enforce mode opt");
}

#[test]
fn plain_run_does_not_emit_copy_cliff_notes_for_comprehensions() {
    // BUG-560: the plain CLI path used to print a performance note for ordinary
    // code and could expose the parser's synthetic comprehension accumulator
    // (`__comprN`) on stderr. Non-`mode` code is valid; editor advisories belong
    // to the LSP path, not `witchy run`.
    let dir = workdir("plain-compr-note");
    let src = write(
        &dir,
        "compr.witchy",
        "fn main(console: Console):\n    let squares = [n * n for n in 1..6]\n    console.print(__render(squares))\n",
    );
    let out = run(&[&src]);
    assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[1, 4, 9, 16, 25]\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

#[test]
fn check_rejects_programs_the_compiled_backend_cannot_accept() {
    // RFC-0070 D2: `check` is the acceptance boundary for runnable programs. A
    // program that typechecks but cannot be compiled must fail at `check`, not
    // surprise the user later at `run`/`emit-wasm`.
    let dir = workdir("check-compiled-acceptance");
    let bad = write(
        &dir,
        "dict_record_key.witchy",
        "type Key:\n    Key(Int)\n\nfn main(console: Console):\n    var d = dict.new()\n    d = dict.insert(d, Key(1), \"one\")\n    console.print(dict.get_or(d, Key(1), \"missing\"))\n",
    );
    let out = run(&["check", &bad]);
    assert!(!out.status.success(), "check must fail for a compiled-backend reject");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot compile to WASM") && stderr.contains("Dict key type"),
        "check should surface the compiled acceptance error, got: {stderr}"
    );

    let lib = write(&dir, "lib.witchy", "pub fn answer() -> Int:\n    42\n");
    let out = run(&["check", &lib]);
    assert!(out.status.success(), "library-only files remain valid check inputs: {}", String::from_utf8_lossy(&out.stderr));
}

// ---- BUG-120 / BUG-184 / BUG-185: `witchy test` ----

#[test]
fn test_dir_fails_on_a_broken_test_file() {
    // BUG-120: a test file that links but doesn't type-check must FAIL the run, not
    // be silently skipped as "ok. 0 passed".
    let dir = workdir("test-broken");
    write(&dir, "good.witchy", "import testing\nfn test_good():\n    testing.assert(1 + 1 == 2, \"math\")\n");
    write(&dir, "broken.witchy", "fn test_broken():\n    let x = missing_function()\n");
    let out = run(&["test", dir.to_str().unwrap()]);
    assert!(!out.status.success(), "a broken test file must fail the run: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn test_imported_module_tests_are_not_double_counted() {
    // BUG-185: `test_lib` in `lib` runs once (when lib.witchy is swept), not again
    // via `suite` which imports lib.
    let dir = workdir("test-dbl");
    write(&dir, "lib.witchy", "import testing\npub fn double(n: Int) -> Int:\n    n * 2\nfn test_lib():\n    testing.assert(double(2) == 4, \"double\")\n");
    write(&dir, "suite.witchy", "import lib\nimport testing\nfn test_suite():\n    testing.assert(lib.double(3) == 6, \"via lib\")\n");
    let out = run(&["test", dir.to_str().unwrap()]);
    assert!(out.status.success(), "both tests pass: {}", String::from_utf8_lossy(&out.stdout));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 passed"), "exactly 2 distinct tests, not 3: {stdout}");
    assert_eq!(stdout.matches("test_lib ... ok").count(), 1, "test_lib must run exactly once: {stdout}");
}

#[test]
fn async_and_gen_tests_do_not_silently_pass() {
    // BUG-184: an async test with `fail_with` must FAIL (its body actually runs);
    // a passing async test passes; a gen test is reported as a failure.
    let dir = workdir("test-async");
    write(
        &dir,
        "a.witchy",
        "import testing\nasync fn test_async_fails() -> Nil:\n    testing.fail_with(\"boom\")\nasync fn test_async_passes() -> Nil:\n    testing.assert(1 + 1 == 2, \"ok\")\n",
    );
    let out = run(&["test", dir.to_str().unwrap()]);
    assert!(!out.status.success(), "the failing async test must fail the run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("test_async_fails ... FAILED"), "async fail must FAIL: {stdout}");
    assert!(stdout.contains("test_async_passes ... ok"), "async pass must pass: {stdout}");
}

// ---- BUG-178 / BUG-179: `witchy caps` ----

#[test]
fn caps_counts_comptime_emitted_apis() {
    // BUG-178: a `comptime:` block that emits `pub fn generated(net: Net)` adds a
    // real capability-bearing API to the footprint.
    let dir = workdir("caps-comptime");
    let src = write(
        &dir,
        "c.witchy",
        "comptime:\n    emit(\"pub fn generated(net: Net, addr: String) -> String:\")\n    emit(\"    net.connect(addr).recv_all()\")\n\nfn main(console: Console):\n    console.print(\"hi\")\n",
    );
    let out = run(&["caps", &src]);
    assert!(out.status.success(), "caps should succeed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("generated") && stdout.contains("Net"), "comptime-emitted Net API must appear: {stdout}");
}

#[test]
fn doc_counts_comptime_emitted_apis() {
    // BUG-178: docs are release-facing API introspection too; generated public
    // functions must render like handwritten public functions on the CLI path.
    let dir = workdir("doc-comptime");
    let src = write(
        &dir,
        "d.witchy",
        "comptime:\n    emit(\"pub fn generated(net: Net) -> Int:\")\n    emit(\"    7\")\n\npub fn direct() -> Int:\n    0\n",
    );
    let out = run(&["doc", &src]);
    assert!(out.status.success(), "doc should succeed: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("#### `fn generated(net: Net) -> Int`"), "generated API must render: {stdout}");
    assert!(stdout.contains("#### `fn direct() -> Int`"), "handwritten API must still render: {stdout}");
}

#[test]
fn caps_requires_a_typechecking_source() {
    // BUG-179: a footprint over code that doesn't type-check is meaningless.
    let dir = workdir("caps-typeck");
    let src = write(&dir, "m.witchy", "fn main(console: Console):\n    missing(console)\n");
    assert!(!run(&["caps", &src]).status.success(), "caps must require a type-checking source");
}

#[test]
fn caps_diff_grantable_widening_message_matches_exit_code() {
    // BUG-314: adding a grantable (user) capability is a widening — the message and
    // the exit code (2) must agree (was 'OK: no widening' yet exit 2).
    let dir = workdir("caps-diff-uc");
    let old = write(&dir, "old.witchy", "grantable capability UiRoot:\n    policy: String\n\nfn main(console: Console):\n    console.print(\"hi\")\n");
    let new = write(&dir, "new.witchy", "grantable capability UiRoot:\n    policy: String\n\nfn main(console: Console, ui: UiRoot):\n    console.print(\"hi\")\n");
    let out = run(&["caps-diff", &old, &new]);
    assert_eq!(out.status.code(), Some(2), "a grantable-cap widening must exit 2");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("WIDENING"), "the message must flag the widening: {stdout}");
    assert!(!stdout.contains("no widening"), "must not claim 'no widening': {stdout}");
}

// ---- BUG-146: grant-document use-only ----

#[test]
fn grant_document_use_only_blocks_reveal() {
    // BUG-146: a `[secrets]` entry declared `use-only = true` is unrevealable.
    let dir = workdir("grant-use-only");
    let prog = write(
        &dir,
        "r.witchy",
        "import secretstore\nimport crypto\n\nfn main(console: Console, store: SecretStore):\n    let s = secretstore.require(store, \"token\")\n    console.print(crypto.reveal(s))\n",
    );
    let use_only = write(&dir, "use_only.toml", "[secrets]\ntoken = { from = \"env:MY_TOKEN\", use-only = true }\n");
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", &use_only, "--accept-grants", &prog])
        .env("MY_TOKEN", "hunter2")
        .output()
        .expect("spawn");
    let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(combined.contains("use-only") || combined.contains("cannot be revealed"), "reveal must be refused: {combined}");
    assert!(!combined.contains("hunter2"), "the secret must never be revealed: {combined}");

    // A misspelled modifier is a loud parse error (deny_unknown_fields).
    let bad = write(&dir, "bad.toml", "[secrets]\ntoken = { from = \"env:MY_TOKEN\", use_only = true }\n");
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", &bad, "--accept-grants", &prog])
        .env("MY_TOKEN", "hunter2")
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "a misspelled grant key must be rejected");
}
