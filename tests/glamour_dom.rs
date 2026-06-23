//! RFC-0008 ("A capability-pure frontend framework (MVU over VNode)") capstone
//! test: drive the FULL glamour MVU run loop headlessly.
//!
//! The committed Node driver (`web/witchy-runtime/glamour-dom.test.mjs`):
//!   1. compiles the `counter` demo rune to WASM via the real `witchy` binary
//!      (`witchy compile … --out …`), with glamour as a sibling module;
//!   2. mounts it through the DOM host shell (`web/witchy-runtime/glamour-dom.mjs`)
//!      into a self-contained fake DOM, asserts the initial render (a `<div>` with
//!      two buttons and a `<span>` showing 0), and that the + button carries a
//!      click handler (an `on` attr wired as `addEventListener`);
//!   3. simulates a `+` click — the handler dispatches the `Inc` message back into
//!      the pure rune, which folds it into count+1 — and asserts the `<span>`
//!      re-renders to 1, then 2; a `-` click decrements; and the differ patches
//!      the existing DOM in place (no wholesale replacement).
//!
//! That proves render + event -> update -> re-render end to end: the witchy core
//! stays pure (the `String -> String` `export_step` ABI), and the JS shell holds
//! all the authority (the DOM, the events). Node is the host engine; if `node` is
//! absent the test SKIPS cleanly so the suite stays green everywhere. The driver
//! is independently runnable: `node web/witchy-runtime/glamour-dom.test.mjs`.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

/// Whether a usable `node` is on PATH (>= the ESM/`node:` features the shell uses).
fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn glamour_dom_run_loop_renders_and_updates_on_events() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-dom.test.mjs");
    assert!(driver.exists(), "the committed DOM test driver must exist at {}", driver.display());

    // Run from the repo root so the driver's relative imports resolve; pass the
    // just-built binary (debug or release) so it compiles the counter with this
    // toolchain.
    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-dom driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour-dom run-loop test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    // Defensive: the driver prints GLAMOUR-DOM OK only when every check passed.
    assert!(stdout.contains("GLAMOUR-DOM OK"), "driver did not report success:\n{stdout}");
}
