//! RFC-0007 ("witchy-WASM in the browser: a pure-compute target") end-to-end test.
//!
//! Drives the committed Node spike (`web/witchy-runtime/spike.mjs`), which:
//!   1. compiles a footprint-EMPTY witchy rune to WASM via the real `witchy`
//!      binary (`witchy compile … --out …`),
//!   2. runs it under the pure-compute JS host (`web/witchy-runtime/witchy-runtime.mjs`)
//!      and asserts the captured output equals the native interpreter run
//!      (`witchy <file>`) byte-for-byte, and
//!   3. proves a capability-using rune is structurally REFUSED — the missing
//!      capability import makes `WebAssembly.instantiate` throw a `LinkError`
//!      (deny-by-omission).
//!
//! Node is the host engine here. If `node` is absent (a CI without it), the test
//! SKIPS cleanly rather than failing, so the Rust suite stays green everywhere;
//! the spike is independently runnable (`node web/witchy-runtime/spike.mjs`).

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

/// Whether a usable `node` is on PATH (>= the ESM/`node:` features the shim uses).
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn browser_shim_runs_pure_rune_and_denies_capabilities() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let spike = manifest.join("web/witchy-runtime/spike.mjs");
    assert!(spike.exists(), "the committed spike script must exist at {}", spike.display());

    // Run the spike from the repo root so its relative shim import resolves, and
    // pass the binary under test (the just-built one, debug or release).
    let out = Command::new("node")
        .arg(&spike)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node spike");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the browser-shim spike failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // Defensive: the spike prints SPIKE OK only when every check passed.
    assert!(stdout.contains("SPIKE OK"), "spike did not report success:\n{stdout}");
}

/// RFC-0041 Phase 0: the shared witchy syntax highlighter (`web/witchy-highlight.js`) is a
/// PURE `source -> HTML` function, so the committed Node test
/// (`web/witchy-runtime/witchy-highlight.test.mjs`) exercises it with no DOM: keywords
/// (incl. `capability`/`grantable`), Title-case types, strings + `${…}` interpolation,
/// comments, duration numbers, that the RETIRED `restrict` verb is not painted as a builtin,
/// and that HTML metacharacters are escaped (no injection through the highlighter).
#[test]
fn witchy_highlighter_colours_current_syntax() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/witchy-highlight.test.mjs");
    assert!(driver.exists(), "the committed highlighter test must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .current_dir(manifest)
        .output()
        .expect("spawn node highlighter test");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the highlighter test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("WITCHY-HIGHLIGHT OK"), "highlighter test did not report success:\n{stdout}");
}
