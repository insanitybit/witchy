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

/// RFC-0045: browser execution carries Witchy's structured abort message through
/// the JS host instead of exposing a generic WebAssembly trap. The committed
/// driver compiles and runs four distinct abort classes and compares each thrown
/// JS `Error.message` with the interpreter/wasmtime diagnostic oracle.
#[test]
fn browser_abort_messages_match_runtime_oracles() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/abort-message.test.mjs");
    assert!(driver.exists(), "the committed abort-message driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node abort-message driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the browser abort-message test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(
        stdout.contains("ABORT-MESSAGE OK"),
        "abort-message driver did not report success:\n{stdout}"
    );
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

/// RFC-0041 regression: docs-bundle asset/content URLs must resolve against the BUNDLE ROOT, not
/// the current client route. Guards the bug where a chapter route `/p/<slug>` made a content
/// fetch route-relative (`/p/content/...` → 404), breaking every page after a nav — and would
/// have broken the GitHub Pages deploy the same way. The pure resolver (`web/docs-asset-url.js`)
/// is unit-tested here because the FakeElement docs drivers can't simulate browser base-URL
/// resolution (which is exactly where the bug lived).
#[test]
fn docs_bundle_asset_urls_resolve_against_the_bundle_root() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/docs-asset-url.test.mjs");
    assert!(driver.exists(), "the docs asset-url test must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .current_dir(manifest)
        .output()
        .expect("spawn node docs-asset-url test");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the docs asset-url test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("DOCS-ASSET-URL OK"), "docs asset-url test did not report success:\n{stdout}");
}

/// A wasm asset fetch that gets an HTML 404/index-fallback page must fail LOUDLY. Guards the
/// bug where a bundle served without `witchy.wasm` (a `--allow-missing-compiler` / `just book`
/// build) surfaced only the raw engine throw — `WebAssembly.instantiate(): expected magic word
/// 00 61 73 6d, found 3c 21 44 4f` (`<!DO…`, the server's HTML error page). The committed Node
/// driver (`web/wasm-fetch.test.mjs`) unit-tests the shared checked fetch (`web/wasm-fetch.js`)
/// used by the playground (`web/playground.js`) and the docs boot (`web/docs-boot.js`).
#[test]
fn wasm_asset_fetch_fails_loudly_on_an_html_response() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/wasm-fetch.test.mjs");
    assert!(driver.exists(), "the wasm-fetch test must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .current_dir(manifest)
        .output()
        .expect("spawn node wasm-fetch test");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the wasm-fetch test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("WASM-FETCH OK"), "wasm-fetch test did not report success:\n{stdout}");
}

/// RFC-0041 Phase 2: the RUNNABLE CELL, end to end and headless. The committed Node driver
/// (`web/witchy-runtime/witchy-runnable.test.mjs`) builds a fake DOM with a
/// `<pre><code class="language-witchy">` block (what `markdown.to_vnode` emits), enhances it
/// with `web/witchy-runnable.js`, and DRIVES Run — which compiles+runs the block's source via
/// the same `witchy-host.js` + `web/witchy.wasm` engine the validated playground uses. It
/// asserts the output equals the program's, that a compile error surfaces as an error cell,
/// that enhancement is idempotent, and that non-`witchy` fences are left alone. This is the
/// crux of the runnable book — proven with no browser (`witchy-host.js` runs under Node/V8).
/// Requires `web/witchy.wasm` (built by `scripts/build-playground.sh`); SKIPS cleanly if absent.
#[test]
fn witchy_runnable_cell_compiles_and_runs_in_page() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !manifest.join("web/witchy.wasm").exists() {
        eprintln!("skipping: web/witchy.wasm not built (run scripts/build-playground.sh)");
        return;
    }
    let driver = manifest.join("web/witchy-runtime/witchy-runnable.test.mjs");
    assert!(driver.exists(), "the committed runnable-cell driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .current_dir(manifest)
        .output()
        .expect("spawn node runnable-cell driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the runnable-cell test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("WITCHY-RUNNABLE OK"), "runnable-cell driver did not report success:\n{stdout}");
}

/// RFC-0041 P0: the standalone playground's example programs are CURRENT witchy. The committed
/// Node driver (`web/witchy-runtime/playground-examples.test.mjs`) imports `EXAMPLES` straight
/// from `web/playground.js` and compiles/runs each with the real toolchain — the five console
/// examples must compile + run, and "Capabilities (a type error)" must fail to compile. This is
/// the P0 repair (the old examples used removed forms like `int_to_string`/`<>`/`restrict`) and
/// guards against a future stale edit; the playground's run path itself is the shared, validated
/// `witchy-host.js` (`runWitchy`) + `witchy-highlight.js`.
#[test]
fn playground_examples_are_current_witchy() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/playground-examples.test.mjs");
    assert!(driver.exists(), "the committed playground-examples test must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node playground-examples test");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the playground-examples test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("PLAYGROUND-EXAMPLES OK"), "playground-examples did not report success:\n{stdout}");
}

/// RFC-0091: the OPT-IN teaching/playground capability host in `witchy-runtime.mjs`. The
/// committed Node driver (`web/witchy-runtime/capability-host.test.mjs`) compiles REAL witchy
/// programs and runs them under `instantiate(bytes, { capabilities })`, proving:
///   * `Clock` reads real wall/monotonic time on the existing i64 ABI;
///   * `Env` is empty by default and reads a page-supplied immutable map (output matching the
///     native oracle);
///   * `Dir` is a per-run in-memory tree whose read/write/append/list/subtree/exists/is_dir/
///     File-handle output is byte-identical to the native interpreter run over a real fixture,
///     and which NEVER touches a real filesystem;
///   * a `..`/absolute path is refused by BOTH the shim and the native run (confinement parity);
///   * `dir.only(Dir.ext(".log"))` denies a non-matching file (entry policy); and
///   * `SecretStore` uses page-supplied opaque grants with native Ed25519/reveal parity and
///     preserves use-only policy; and
///   * the DEFAULT host still `LinkError`s on capability programs, while `Exec`/bare `Secret`/
///     `Net` stay denied by omission EVEN under the opt-in host.
///
/// This is the parity + non-widening guarantee for RFC-0091 phase 1.
#[test]
fn browser_capability_host_runs_portable_providers_and_denies_the_rest() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/capability-host.test.mjs");
    assert!(driver.exists(), "the committed capability-host test must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node capability-host test");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the capability-host test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("CAPABILITY-HOST OK"), "capability-host driver did not report success:\n{stdout}");
}
