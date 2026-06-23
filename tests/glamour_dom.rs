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

/// RFC-0008 EFFECTS-AS-DATA capstone: prove the rune -> `Cmd` -> shell-performs ->
/// msg -> update loop end to end, headlessly, with a FAKE CLOCK.
///
/// The committed Node driver (`web/witchy-runtime/glamour-dom-timer.test.mjs`)
/// compiles the `autocounter` demo — whose `update` returns `After(1000, Tick)` —
/// mounts it through the shell with an INJECTED fake timer (`opts.setTimeout`),
/// then advances the fake clock and asserts the count auto-increments WITHOUT any
/// user click. The rune holds no `Clock`: it only DESCRIBES the timer as a `Cmd`
/// value; the capability-HOLDING shell performs the `setTimeout` and dispatches the
/// deferred `Tick` back as a msg. That is "authority lives at the edge" made
/// observable: the effect runs, but only the shell could run it.
#[test]
fn glamour_dom_timer_effect_dispatches_msg_via_fake_clock() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-dom-timer.test.mjs");
    assert!(driver.exists(), "the committed timer test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-dom timer driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour-dom timer-effect test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("GLAMOUR-DOM TIMER OK"),
        "timer driver did not report success:\n{stdout}"
    );
}

/// The headline RFC-0008 property, asserted directly: the effects-using demo rune
/// has an EMPTY capability footprint. The `After(1000, Tick)` timer is the SHELL's
/// authority; the rune only describes it as data, so `witchy caps` must report no
/// `Net`/`Dir`/`Clock` for the app's view/update core. (`main`'s `Console` is
/// output, not authority — the per-export breakdown shows `export_step` is `(none)`
/// and only `main` carries `Console`.)
#[test]
fn glamour_autocounter_footprint_is_empty() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let app = manifest.join("projects/glamour/examples/autocounter/src/autocounter.witchy");
    assert!(app.exists(), "the autocounter demo must exist at {}", app.display());

    let out = Command::new(BIN)
        .arg("caps")
        .arg(&app)
        .current_dir(manifest)
        .output()
        .expect("run `witchy caps` on the autocounter rune");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "`witchy caps` failed:\n--- stderr ---\n{stderr}");
    // The effects-using export holds NO authority — the timer is the shell's.
    assert!(
        stdout.contains("export_step  (none)"),
        "the effects-using export must have an empty footprint:\n{stdout}"
    );
    // The rune touches no ambient authority: no Net/Dir/Clock anywhere.
    for forbidden in ["Net", "Dir", "Clock"] {
        assert!(
            !stdout.contains(forbidden),
            "the capability-pure rune must not demand `{forbidden}`:\n{stdout}"
        );
    }
}

/// RFC-0008 DOGFOODING capstone: drive the glamour SYNTAX HIGHLIGHTER rune
/// headlessly — the proving ground for coven-web's sandbox-highlighter migration
/// (projects/coven-web/PLAN.md WS-I/M6).
///
/// The committed Node driver (`web/witchy-runtime/highlighter.test.mjs`) compiles
/// the `highlighter` demo to WASM, calls its `export_render({src})` export through
/// the RFC-0007 pure-compute host shim, parses the returned VNode JSON, and renders
/// it into a fake DOM with createElement/textContent ONLY. It asserts the
/// highlighted structure (a `pre>code`, the keyword `fn` in `span.kw`, the comment
/// in `span.com`, the string in `span.str`) AND — the security headline — that a
/// snippet containing `<script>` renders as ESCAPED text in a DOM text node, never
/// a live `<script>` element. Node is the host engine; if `node` is absent the
/// test SKIPS cleanly. The driver is independently runnable:
/// `node web/witchy-runtime/highlighter.test.mjs <witchy-binary>`.
#[test]
fn glamour_highlighter_renders_classed_spans_and_escapes_xss() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/highlighter.test.mjs");
    assert!(driver.exists(), "the committed highlighter test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node highlighter driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour highlighter test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("HIGHLIGHTER OK"), "highlighter driver did not report success:\n{stdout}");
}

/// The headline RFC-0008 property for the highlighter: its render export is
/// capability-EMPTY. A syntax highlighter that renders genuinely-untrusted package
/// SOURCE is exactly where the empty-footprint + sandbox composition matters most,
/// so `witchy caps` must report no `Net`/`Dir`/`Clock` for `export_render`.
/// (`main`'s `Console` is output, not authority — the per-export breakdown shows
/// `export_render` is `(none)`.)
#[test]
fn glamour_highlighter_footprint_is_empty() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let app = manifest.join("projects/glamour/examples/highlighter/src/highlighter.witchy");
    assert!(app.exists(), "the highlighter demo must exist at {}", app.display());

    let out = Command::new(BIN)
        .arg("caps")
        .arg(&app)
        .current_dir(manifest)
        .output()
        .expect("run `witchy caps` on the highlighter rune");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "`witchy caps` failed:\n--- stderr ---\n{stderr}");
    // The render export holds NO authority — it only computes a VNode tree.
    assert!(
        stdout.contains("export_render  (none)"),
        "the highlighter render export must have an empty footprint:\n{stdout}"
    );
    // The rune touches no ambient authority: no Net/Dir/Clock anywhere.
    for forbidden in ["Net", "Dir", "Clock"] {
        assert!(
            !stdout.contains(forbidden),
            "the capability-pure highlighter must not demand `{forbidden}`:\n{stdout}"
        );
    }
}
