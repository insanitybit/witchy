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

/// (RFC-0039) The capability gate, made structural: a component WITHOUT a `UiFetch`
/// cannot construct `Cmd.Http`. `glamour.http_get` REQUIRES the token as its leading
/// argument, so a fetch attempt without one FAILS TO COMPILE — the unauthorized
/// effect is unrepresentable, not merely rejected by a host at runtime.
#[test]
fn glamour_http_effect_requires_a_uifetch_token() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    // Try to build an HTTP effect WITHOUT holding a `UiFetch`.
    std::fs::write(
        work.join("bad.witchy"),
        "import glamour\n\ntype Msg:\n    Got(Int, String)\n\npub fn go() -> Cmd(Msg):\n    glamour.http_get(\"/x\", \"Got\")\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["compile", "bad.witchy", "--out", "bad.wasm"])
        .current_dir(&work)
        .output()
        .expect("spawn witchy compile");
    let produced = work.join("bad.wasm").exists();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        !out.status.success() && !produced,
        "an HTTP effect without a UiFetch MUST NOT compile (the capability gate). output:\n{msg}"
    );
}

/// (RFC-0039) The same gate on the credential-port effect: a component WITHOUT a
/// `CredentialPort` cannot invoke a host port. `glamour.port` REQUIRES the token as its
/// leading argument (the port NAME is carried by the token, not a free string), so an
/// attempt to run e.g. `promote` without holding that authority FAILS TO COMPILE. This is
/// the sharp case from the RFC's motivation — `Cmd.Port("promote", …)` from any component —
/// made unrepresentable rather than merely denied by host policy at runtime.
#[test]
fn glamour_port_effect_requires_a_credential_token() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-port-gate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    // Try to invoke the `promote` port WITHOUT holding a `CredentialPort` — passing the
    // port name as a bare string the way the old ambient API allowed.
    std::fs::write(
        work.join("bad.witchy"),
        "import glamour\n\ntype Msg:\n    Done(String)\n\npub fn go() -> Cmd(Msg):\n    glamour.port(\"promote\", \"acme/charts\", \"Done\")\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["compile", "bad.witchy", "--out", "bad.wasm"])
        .current_dir(&work)
        .output()
        .expect("spawn witchy compile");
    let produced = work.join("bad.wasm").exists();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        !out.status.success() && !produced,
        "a port effect without a CredentialPort MUST NOT compile (the capability gate). output:\n{msg}"
    );
}

/// (RFC-0039) The DoD differential: a component CANNOT read another component's password.
/// A password entered into a `secret_input` stays in host custody; the rune holds only an
/// opaque `SecretRef`. `SecretRef`/`SecretInput` are sealed Glamour capabilities, so a
/// consuming module may HOLD and PASS one but cannot DESTRUCTURE it — an attempt to unwrap a
/// `SecretRef` to recover the host slot (the password's locator) FAILS TO COMPILE. Combined
/// with the secret never becoming a msg/model `String` (see the node driver below), a
/// sibling cannot observe another component's secret, by construction.
#[test]
fn glamour_password_is_unreadable_by_a_sibling_component() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-secret-seal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();
    // A component tries to read the secret behind an opaque `SecretRef` by destructuring the
    // sealed capability to recover the host slot.
    std::fs::write(
        work.join("bad.witchy"),
        "import glamour\n\nfn steal(r: SecretRef) -> String:\n    match r:\n        SecretRef(slot) -> slot\n\nfn main(console: Console, ui: UiRoot):\n    let input = glamour.secret_field(ui, \"login\", \"password\")\n    print(console, steal(glamour.secret_ref(input)))\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["compile", "bad.witchy", "--out", "bad.wasm"])
        .current_dir(&work)
        .output()
        .expect("spawn witchy compile");
    let produced = work.join("bad.wasm").exists();
    let msg = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        !out.status.success() && !produced,
        "destructuring another component's SecretRef MUST NOT compile (the secret seal). output:\n{msg}"
    );
}

/// (RFC-0039) The runnable proof of host secret custody. The committed Node driver
/// (`web/witchy-runtime/glamour-secret.test.mjs`) mounts a login rune whose `view` renders a
/// `secret_input` and whose `update` emits `submit_secret`, types a password, and submits.
/// It asserts the host credential port receives the REAL password while the rune's
/// model/view only ever hold a non-sensitive status and the port's result — the secret bytes
/// go host -> port and never enter the WASM.
#[test]
fn glamour_secret_input_keeps_the_password_host_side() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-secret.test.mjs");
    assert!(driver.exists(), "the committed secret test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-secret driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour secret-custody test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-SECRET OK"), "secret driver did not report success:\n{stdout}");
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

/// (RFC-0040) The committed Node driver (`user-cap-export.test.mjs`) compiles a
/// cap-gated export `export_step(ui: UiRoot, input: String)`, instantiates it in the
/// pure browser host with a `[user_caps]` grant, and asserts the host-minted
/// `UiRoot`'s policy round-trips into the rune — and that a missing grant traps
/// (parity with the wasmtime host). This is the browser end of the app-root ABI,
/// proving the minted VALUE, not just that the wrapper is well-formed.
#[test]
fn user_cap_export_mints_uiroot_in_the_browser_host() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/user-cap-export.test.mjs");
    assert!(driver.exists(), "the committed user-cap export driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node user-cap-export driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the RFC-0040 user-cap export test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("RFC-0040"), "the user-cap export driver did not report success:\n{stdout}");
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

/// RFC-0015 Phase A: the glamour DOM shell is XSS-safe at the attribute layer.
///
/// The committed Node driver (`web/witchy-runtime/glamour-dom-xss.test.mjs`)
/// compiles a rune whose `view` emits deliberately hostile attributes — a
/// `javascript:` href, a `javascript:` `<img>` src, and a string `onclick` handler
/// — mounts it through the real shell, and asserts the DOM was NEUTRALIZED: URL
/// attributes are scheme-checked (hostile schemes collapse to `#`), `on*` attributes
/// are never written (handlers attach only via the typed `on(event, msg)` path), and
/// safe URLs (relative, https) pass through untouched. This guards the single
/// `applyAttr` choke point both the create and update render paths route through.
#[test]
fn glamour_dom_attribute_layer_neutralizes_xss() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-dom-xss.test.mjs");
    assert!(driver.exists(), "the committed XSS test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-dom xss driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour-dom XSS test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-DOM XSS OK"), "xss driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase C: the async HTTP effect. The committed Node driver
/// (`web/witchy-runtime/glamour-http.test.mjs`) compiles a rune whose `update` returns
/// `http_get("/data", "GotData")` — the rune holds NO `Net`, only describes the request —
/// and drives it through the shell with an INJECTED fake fetch. It asserts the response
/// dispatches back as `GotData(status, body)` and updates the model, that the host shell
/// attached the session credential (`authHeaders`) ITSELF, and that the token never
/// entered the rune's state. This is the effects-as-data network access the coven-web
/// shell needs, with authority kept entirely at the host edge.
#[test]
fn glamour_http_effect_fetches_with_host_attached_auth() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-http.test.mjs");
    assert!(driver.exists(), "the committed HTTP test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-http driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour HTTP-effect test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-HTTP OK"), "http driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase C: client-side routing. The committed Node driver
/// (`web/witchy-runtime/glamour-routing.test.mjs`) compiles a rune that maps `model` (the
/// current path) to a view and returns `navigate(path)` to change it — holding no history
/// authority, only describing the navigation. Driven with an injected fake history /
/// location / popstate, it asserts the initial path is delivered into the view, a `Nav`
/// pushes history AND re-renders, and a popstate (Back) re-delivers the route.
#[test]
fn glamour_routing_navigates_and_handles_back() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-routing.test.mjs");
    assert!(driver.exists(), "the committed routing test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-routing driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour routing test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-ROUTING OK"), "routing driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase C: keyed list reconciliation. The committed Node driver
/// (`web/witchy-runtime/glamour-keyed.test.mjs`) renders a `<ul>` of `keyed(k, <li>)`
/// children, marks each live node, reorders the list, and asserts the SAME node instances
/// appear in the new order — the host MOVED the existing nodes (preserving identity, and
/// thus a focused input's caret) rather than rebuilding by position. Index diffing would
/// fail this; key-based reconciliation passes it.
#[test]
fn glamour_keyed_list_reuses_nodes_on_reorder() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-keyed.test.mjs");
    assert!(driver.exists(), "the committed keyed-list test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-keyed driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour keyed-list test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-KEYED OK"), "keyed driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase D: the coven package-DETAIL page, built on glamour. The committed Node
/// driver (`web/witchy-runtime/glamour-package-page.test.mjs`) mounts the `package_page`
/// example and asserts it composes the registry's key view — the package identity (name,
/// version, copy-able install command), the capability FOOTPRINT as badges (coven's
/// differentiator), and the README + generated API docs rendered inline from Markdown via
/// `markdown.to_vnode`, with a README/Docs tab toggle. This is the template the coven-web
/// shell's package view is built on; it composes the Phase A–C pieces end to end.
#[test]
fn glamour_package_page_renders_identity_footprint_and_docs() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-package-page.test.mjs");
    assert!(driver.exists(), "the committed package-page test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-package-page driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour package-page test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-PACKAGE-PAGE OK"), "package-page driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase D: the d3-runes-chart COMPARTMENT renderer's chart logic. The committed
/// Node test (`projects/coven-web/web/dist/compartments/d3-runes-chart/chart.test.mjs`)
/// exercises the renderer's pure `barChartSvg(points) -> SVG` core — the foreign code that
/// would run isolated in the chart compartment. It asserts one bar per point, scaling to
/// the max, and that only NUMERIC counts reach the SVG (a hostile label cannot inject, so
/// even the in-box `innerHTML` is safe). The live rendering and the iframe/CSP isolation
/// are browser-verified; this gates the renderer's logic.
#[test]
fn d3_compartment_renderer_chart_logic_is_correct() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("projects/coven-web/web/dist/compartments/d3-runes-chart/chart.test.mjs");
    assert!(driver.exists(), "the committed chart test must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .current_dir(manifest)
        .output()
        .expect("spawn node runes-chart test");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the d3-runes-chart renderer test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("RUNES-CHART OK"), "chart test did not report success:\n{stdout}");
}

/// RFC-0015 Phase D: the coven catalog/index view on glamour. The committed Node driver
/// (`web/witchy-runtime/glamour-catalog.test.mjs`) mounts the `catalog` example and drives
/// its live search box, asserting that `on_input` carries the field's value into the MVU
/// loop (the rune holds no DOM) and that the keyed rune cards filter by name AND by
/// capability. This is the registry home page built on the framework, and it exercises the
/// `on_input` forms handler added for it.
#[test]
fn glamour_catalog_view_filters_by_name_and_capability() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-catalog.test.mjs");
    assert!(driver.exists(), "the committed catalog test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-catalog driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour catalog-view test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-CATALOG OK"), "catalog driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase D: the version (record-detail) and trust (TUF) view templates render
/// their security fields, identically on both backends. These are the registry's audit
/// pages built on glamour: the version view surfaces the capability footprint, provenance,
/// and promote/yank controls; the trust view surfaces the TUF checks. The shell fills the
/// data from the registry; this gates the templates' rendering + parity.
#[test]
fn glamour_version_and_trust_views_render_security_fields() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-views-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    std::fs::copy(
        manifest.join("projects/glamour/src/glamour.witchy"),
        work.join("glamour.witchy"),
    )
    .unwrap();

    let views: &[(&str, &[&str])] = &[
        ("version_view", &["Js{d3-runes-chart}", "trusted-publisher", "state-released", "promote", "yank"]),
        ("trust_view", &["rollback check", "snapshot signature", "Registry trust"]),
    ];
    for (view, must_contain) in views {
        std::fs::copy(
            manifest.join(format!("projects/glamour/examples/{view}/src/{view}.witchy")),
            work.join(format!("{view}.witchy")),
        )
        .unwrap();
        let prog = work.join(format!("{view}.witchy"));

        // Both backends agree (the prime directive) ...
        let par = Command::new(BIN).arg("parity").arg(&prog).current_dir(&work).output().expect("witchy parity");
        let pout = String::from_utf8_lossy(&par.stdout);
        assert!(par.status.success() && pout.contains("agree"), "{view} must render identically on both backends:\n{pout}");

        // ... and the rendered VNode JSON carries the expected security fields.
        let run = Command::new(BIN).arg(&prog).current_dir(&work).output().expect("witchy run");
        let rout = String::from_utf8_lossy(&run.stdout);
        for needle in *must_contain {
            assert!(rout.contains(needle), "{view} should render `{needle}`:\n{rout}");
        }
    }
    let _ = std::fs::remove_dir_all(&work);
}

/// RFC-0015 Phase D: the coven-web SHELL on glamour, end to end. The committed Node driver
/// (`web/witchy-runtime/glamour-coven-app.test.mjs`) mounts the `coven_app` rune with an
/// injected fake registry (fetch) and history, then drives the real app loop: the initial
/// route fetches the catalog and renders the rune list; clicking a rune navigates to its
/// package URL, which fetches and renders the docs. The rune holds no `Net` — the host shell
/// performs every fetch — so this proves the routed, data-fetched trusted shell works with
/// authority entirely at the edge (the production shell, minus the browser-only WebAuthn).
#[test]
fn glamour_coven_app_shell_routes_and_fetches() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-coven-app.test.mjs");
    assert!(driver.exists(), "the committed coven-app test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-coven-app driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour coven-app shell test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-COVEN-APP OK"), "coven-app driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase D: the host-shell PORT effect — the mechanism behind session login and
/// the WebAuthn passkey ceremony. The committed Node driver
/// (`web/witchy-runtime/glamour-port.test.mjs`) compiles a rune whose `update` returns
/// `port("passkeyLogin", "", "LoggedIn")` and drives it through an INJECTED port (the real
/// one would call `navigator.credentials` and hold the bearer token in the host). It asserts
/// the port runs on the login click, the signed-in identity renders, and ONLY the outcome
/// ("alice") — never a credential or token — enters the rune. This makes the session/WebAuthn
/// wiring suite-testable; only the real `navigator.credentials` port impl is browser-bound.
#[test]
fn glamour_port_effect_runs_session_login_at_the_host() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-port.test.mjs");
    assert!(driver.exists(), "the committed port test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-port driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour port-effect test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-PORT OK"), "port driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase D: the COMPLETE coven-web frontend as one glamour app — the suite-tested
/// core of the TypeScript->glamour migration. The committed Node driver
/// (`web/witchy-runtime/glamour-coven-web-app.test.mjs`) mounts `coven_web_app` with an
/// injected fake registry, history, and host ports, then drives the whole shell: catalog ->
/// version record -> passkey sign-in -> promote -> API docs -> registry trust. Every fetch
/// and the WebAuthn ceremony run in the host (the rune holds no `Net`, no token, empty
/// footprint), proving the trusted shell that replaces the TS app works with authority
/// entirely at the edge. (The JS bootstrap, build swap, and TS deletion are the follow-up,
/// browser-verified cutover.)
#[test]
fn glamour_coven_web_app_full_shell_works() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-coven-web-app.test.mjs");
    assert!(driver.exists(), "the committed coven-web-app test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-coven-web-app driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the complete coven-web app shell test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-COVEN-WEB-APP OK"), "coven-web-app driver did not report success:\n{stdout}");
}

/// Regression: the `String -> String` WASM export ABI must not leak. The bump allocator
/// (`__galloc`) never frees, so a long-lived run loop (glamour MVU: one call per event) would
/// otherwise accumulate one call's allocations forever and eventually exhaust WASM memory —
/// `__galloc` returns an out-of-bounds pointer and the app crashes (observed after a few dozen
/// coven-web navigations). The fix exports each module's `__heap` pointer and the host
/// (`witchy-runtime.mjs`) resets it to its base after every call. The committed driver compiles
/// an allocation-heavy export and drives 3000 calls, asserting memory stays bounded and the heap
/// returns to base each time.
#[test]
fn wasm_string_export_does_not_leak_across_calls() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/heap-reset.test.mjs");
    assert!(driver.exists(), "the committed heap-reset driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node heap-reset driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the WASM heap-reset regression test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("HEAP-RESET OK"), "heap-reset driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase B: the `compartment` primitive isolates foreign code. The committed
/// Node driver (`web/witchy-runtime/glamour-compartment.test.mjs`) compiles a rune that
/// embeds `glamour.compartment("d3-runes-chart", grant, "ChartResized")` — dropping in a
/// third-party chart — and asserts the host shell renders it as a LOCKED-DOWN iframe
/// (`sandbox="allow-scripts"` with no `allow-same-origin` → opaque origin; loaded from the
/// sealed `/compartments/<id>/` path) and never inlines the foreign renderer or the grant
/// into the trusted DOM. The browser enforces the origin/CSP isolation at runtime; this
/// proves there is no code path that puts foreign content anywhere but the boxed frame —
/// the configuration behind "even a compromised d3 stays contained".
#[test]
fn glamour_compartment_isolates_foreign_code() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-compartment.test.mjs");
    assert!(driver.exists(), "the committed compartment test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-compartment driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour compartment isolation test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-COMPARTMENT OK"), "compartment driver did not report success:\n{stdout}");
}

/// RFC-0015 Phase A3 PARITY: the Markdown renderer produces byte-identical VNode JSON on
/// the interpreter and the compiled WASM backend — the prime directive, for the pure
/// `markdown.to_vnode` string-processing path. Renders a document exercising headings,
/// bold, inline code, a sanitized link, and a list, then runs `witchy parity`.
#[test]
fn glamour_markdown_renders_identically_on_both_backends() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let work = std::env::temp_dir().join(format!("glamour-md-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    for f in ["glamour.witchy", "markdown.witchy"] {
        std::fs::copy(manifest.join("projects/glamour/src").join(f), work.join(f)).unwrap();
    }
    let prog = "import glamour\nimport markdown\nimport json\nimport reflect\n\
type Msg derive(Reflect):\n    Noop\n\n\
fn msg_to_json(m: Msg) -> Json:\n    json.value_of(m)\n\n\
fn main(console: Console):\n    \
print(console, glamour.to_json(markdown.to_vnode(\"# Title\\n\\nA **bold** word, `code`, a [link](https://example.com), and:\\n\\n- one\\n- two\\n\"), msg_to_json))\n";
    std::fs::write(work.join("mdparity.witchy"), prog).unwrap();

    let out = Command::new(BIN)
        .arg("parity")
        .arg(work.join("mdparity.witchy"))
        .current_dir(&work)
        .output()
        .expect("run witchy parity on the markdown program");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        out.status.success() && stdout.contains("agree"),
        "markdown rendering must be identical on both backends:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}

/// RFC-0015 Phase A3: the Markdown renderer (`markdown.to_vnode`) is XSS-safe.
///
/// The committed Node driver (`web/witchy-runtime/glamour-markdown.test.mjs`) compiles
/// a rune that renders deliberately hostile UNTRUSTED Markdown — a raw `<script>` tag
/// and a `javascript:` link — through `markdown.to_vnode`, mounts it, and asserts the
/// DOM is inert: no `<script>` element is ever created (the raw HTML shows as literal
/// text — glamour has no HTML-string sink), and the link's `javascript:` href is
/// neutralized to `#`. Normal Markdown (heading, bold, list) still renders to real
/// elements. This is the safe-by-construction README/doc rendering RFC-0015 relies on.
#[test]
fn glamour_markdown_renderer_is_xss_safe() {
    if !node_available() {
        eprintln!("skipping: `node` is not available on PATH");
        return;
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let driver = manifest.join("web/witchy-runtime/glamour-markdown.test.mjs");
    assert!(driver.exists(), "the committed markdown test driver must exist at {}", driver.display());

    let out = Command::new("node")
        .arg(&driver)
        .arg(BIN)
        .current_dir(manifest)
        .output()
        .expect("spawn node glamour-markdown driver");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "the glamour markdown XSS test failed:\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("GLAMOUR-MARKDOWN OK"), "markdown driver did not report success:\n{stdout}");
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
