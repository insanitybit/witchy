//! End-to-end tests for coven, the witchy package manager. These drive the real
//! `witchy` binary (via `CARGO_BIN_EXE_witchy`) through the full supply-chain
//! lifecycle: scaffold, publish (staged), promote (second factor), add (gated),
//! build, run, audit. Each test is hermetic — its own temp `WITCHY_HOME` and
//! working tree — so they can run in parallel.

mod support;
#[path = "support/registry.rs"]
mod registry;
#[path = "support/package_manager.rs"]
mod package_manager;
#[path = "support/sandbox.rs"]
mod sandbox;
use support::coven::*;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The trusted-publishing token lifecycle over one registry, end to end. On the
/// FIRST publish to a namespace the token's identity binds it (TOFU), and SEC-023
/// requires that first bind's repository org to equal the namespace — so an
/// attacker's CI token (`evilcorp/fork`) cannot land-grab a victim's intended
/// namespace, while a matching org (`acme/secure-repo`) claims it. Once bound,
/// every later `pm publish` (carrying `COVEN_ID_TOKEN`) must match the bound
/// policy: a token from a different repository is refused 403 (namespace hijack),
/// a token from the right repo but a non-release workflow is refused 403, and a
/// REPLAYED token — right repo, right workflow, but a `jti` already consumed by a
/// successful publish — is refused 403 (single-use, SEC-022). A real workflow
/// mints a fresh token per run, so the bound CI identity re-publishes with a
/// fresh token; a distinct human then promotes.
#[test]
fn trusted_publishing_binds_repo_single_use_and_first_bind() {
    registry::trusted_publishing_binds_repo_single_use_and_first_bind();
}

/// SEC-018: on a trusted registry, `yank` requires an EXISTING maintainer of the
/// namespace — it is not an unauthenticated operation. A non-maintainer token is
/// refused 403; the maintainer (the human who promoted, bound TOFU) may yank.
#[test]
fn trusted_yank_requires_a_maintainer() {
    registry::trusted_yank_requires_a_maintainer();
}

/// Trusted publishing against a ROTATING-key issuer: the registry trusts a JWKS document
/// (not a single pinned key) and selects the verifying key by each token's `kid` — exactly
/// what verifying a real GitHub Actions OIDC token requires, since GitHub rotates its
/// signing keys. A token whose `kid` is present in the JWKS verifies and publishes; a token
/// whose `kid` is absent (a key that rotated away / was never published) is refused 401,
/// even though it is otherwise a well-formed token from the trusted issuer.
#[test]
fn trusted_publishing_verifies_a_jwks_issuer_by_kid() {
    registry::trusted_publishing_verifies_a_jwks_issuer_by_kid();
}

/// The registry generates browsable API docs on demand: `GET /coven/doc` renders the
/// published rune's stored source to the same Markdown `witchy doc` emits (types and
/// public functions with their doc-comments). This is safe on untrusted published code
/// because `compiler.doc` only PARSES the source — it never runs it — and the source is
/// hash-verified against the signed record before rendering.
#[test]
fn coven_serves_generated_api_docs() {
    registry::coven_serves_generated_api_docs();
}

/// The registry audits the FOREIGN-CODE compartments a package embeds (RFC-0015): GET
/// /coven/compartments scans the published source for `compartment("<id>"` call sites and
/// reports the renderer ids — the `Js` governance signal ("what third-party code does this
/// package run?"), surfaced at the registry layer (not the compiler). A package that
/// embeds none reports none.
#[test]
fn coven_audits_embedded_compartments() {
    registry::coven_audits_embedded_compartments();
}

/// A trusted registry requires a valid identity token to publish: an anonymous
/// publish (no `COVEN_ID_TOKEN`) is refused 401, and a token from an UNTRUSTED
/// issuer (a rogue IdP whose key the registry doesn't list) is also refused 401.
/// The front-end forwards whatever token the environment provides; the server is
/// the gate.
#[test]
fn token_required_and_untrusted_issuer_refused() {
    registry::token_required_and_untrusted_issuer_refused();
}

/// The front-end verifies the registry's TUF chain on `add` and re-verifies it on
/// `verify --online`. `add` pins the registry's signed snapshot version into the lock; a
/// later online verify re-fetches the signed snapshot + timestamp roles and checks the
/// whole chain against the root key. Tampering a signed field of the server's
/// snapshot breaks its root-key signature, so a fresh `verify` rejects it — the
/// signature + content binding, not the transport, are trusted.
#[test]
fn tuf_chain_verified_and_snapshot_tamper_rejected() {
    registry::tuf_chain_verified_and_snapshot_tamper_rejected();
}

/// A registry lock's snapshot version and root key are one trust record. A
/// malformed value or either missing half is a diagnostic, never an unpinned
/// fallback that reaches the network.
#[test]
fn witchy_pm_verify_rejects_malformed_registry_trust_pins() {
    registry::witchy_pm_verify_rejects_malformed_registry_trust_pins();
}

/// BUG-386: a TUF role can be validly signed yet structurally incomplete. The
/// verifier must reject that before old defaulting helpers can turn absent fields
/// into `0`, `""`, or `JsonNull`.
#[test]
fn tuf_chain_rejects_validly_signed_malformed_roles() {
    registry::tuf_chain_rejects_validly_signed_malformed_roles();
}

/// A registry rollback — serving an OLDER TUF snapshot version than the one the
/// project last pinned — is refused. We simulate having seen a much newer snapshot
/// by bumping the lock's pinned `registry_snapshot_version`; the server's actual
/// (older) snapshot version is now below the pin, which `verify` rejects.
#[test]
fn tuf_rollback_is_rejected() {
    registry::tuf_rollback_is_rejected();
}

/// The front-end refuses a tampered registry record via its Ed25519 signature.
/// The SLSA provenance attestation is part of the signed record, so editing it on
/// the server (`trusted-publisher` → `evil-publisher`) breaks the root-key
/// signature. A fresh `pm add` fetches the tampered record and rejects it — the
/// content address + the signature, not the transport, are trusted.
#[test]
fn networked_registry_signature_detects_tampering() {
    registry::networked_registry_signature_detects_tampering();
}

/// `pm new` scaffolds a runnable rune and `pm run` compiles + runs it through the
/// embedded compiler (the cargo→rustc split of RFC-0004) — no registry needed.
#[test]
fn scaffold_and_run() {
    let base = unique("scaffold");
    let out = Command::new(BIN)
        .current_dir(&base)
        .args(["pm", "new", "app"])
        .output()
        .expect("spawn witchy pm new");
    assert!(out.status.success(), "new failed: {}", stderr(&out));
    let out = Command::new(BIN)
        .current_dir(&base)
        .args(["pm", "run", "app"])
        .output()
        .expect("spawn witchy pm run");
    assert!(out.status.success(), "run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("hello from app"), "got: {}", stdout(&out));
    let _ = std::fs::remove_dir_all(&base);
}

/// The cargo-like consumer lifecycle through the **witchy front-end** against a
/// real coven, stage gate included: trusted CI `publish` (staged, no long-lived
/// API key) → a staged rune is NOT addable (no released version satisfies it) →
/// a DISTINCT human `promote` (released; machines stage, humans release) →
/// `witchy pm add` (fetched over HTTP, signature + content verified, vendored
/// into the project's `vendor/` and pinned in `witchy.lock` by content hash) →
/// `witchy pm run` a consumer that imports it → `list` reflects `released`. The
/// front-end is canonical: the vendored basename `strkit` is the import name,
/// and the lock pins `sha256:`.
#[test]
fn full_lifecycle_publish_promote_add_use() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "lifecycle");
    let app = fe.new_app();
    let lib = fe.lib(
        "acme/strkit",
        "0.1.0",
        "pub fn shout(s: String) -> String:\n    \"HEY \" + s\n",
    );

    // Publish via a trusted CI identity token (no long-lived API key).
    let ci = server.ci_token("acme-strkit-repo", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&ci));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "publish: {}", stdout(&out));

    // Staged over the network → not addable (no released version satisfies it).
    let out = fe.pm(&app, &["add", "acme/strkit"], None);
    assert!(!out.status.success(), "a staged version must not be addable");
    assert!(
        stdout(&out).contains("no released version") || stderr(&out).contains("no released version"),
        "stdout {} stderr {}",
        stdout(&out),
        stderr(&out)
    );

    // Promote over the network with a distinct human identity token.
    let alice = server.human_token("alice");
    let out = fe.pm(&lib, &["promote", "acme/strkit", "0.1.0"], Some(&alice));
    assert!(out.status.success() && stdout(&out).contains("promote: 200"), "promote: {}", stdout(&out));

    // Add the released library (pure — no capability widening, just vendored;
    // fetched over HTTP, signature-verified + content-hashed).
    let out = fe.pm(&app, &["add", "acme/strkit"], None);
    assert!(out.status.success(), "add failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("added acme/strkit@0.1.0"), "add: {}", stdout(&out));

    // Use it from main (the vendored basename `strkit` is the import name).
    std::fs::write(
        app.join("src").join("app.witchy"),
        "import strkit\n\nfn main(console: Console):\n    console.print(strkit.shout(\"witchy\"))\n",
    )
    .unwrap();
    let out = fe.pm(&app, &["run", "."], None);
    assert!(out.status.success(), "run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("HEY witchy"), "got: {}", stdout(&out));

    // The lockfile pins the dependency by content hash. BUG-193: the lock identity
    // is the manifest name `acme/strkit`; the import alias `strkit` is recorded too.
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("name = \"acme/strkit\"") && lock.contains("alias = \"strkit\""), "lock: {lock}");
    assert!(lock.contains("sha256:"), "lock should pin the content hash: {lock}");

    // `list` over the network reflects the released state.
    let out = fe.pm(&app, &["list", "acme/strkit"], None);
    assert!(stdout(&out).contains("released"), "list: {}", stdout(&out));
}

/// A staged (published-but-not-promoted) version is not resolvable: only released
/// versions satisfy a requirement. The front-end resolves over `/coven/versions`
/// and refuses when no released version matches.
#[test]
fn staged_dependency_is_not_resolvable() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "staged");
    let app = fe.new_app();

    // Publish WITHOUT promoting (a trusted CI stage, but no human promote).
    let dir = fe.lib("acme/json", "1.0.0", "pub fn p(s: String) -> String:\n    s\n");
    let ci = server.ci_token("acme-json-repo", "release.yml");
    let out = fe.pm(&dir, &["publish", "."], Some(&ci));
    assert!(
        out.status.success() && stdout(&out).contains("publish: 200"),
        "publish failed: {}\nstdout: {}",
        stderr(&out),
        stdout(&out)
    );

    // Adding the staged rune must fail: no released version satisfies the request.
    let out = fe.pm(&app, &["add", "acme/json"], None);
    assert!(!out.status.success(), "a staged version must not be resolvable");
    assert!(
        stdout(&out).contains("no released version") || stderr(&out).contains("no released version"),
        "stdout {} stderr {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(!app.join("vendor/json").exists(), "nothing should be vendored on a failed add");
}

/// BUG-391: registry version coordinates are identity, not display text. If a
/// corrupt `/coven/versions` record says a released coordinate is `" 1.0.0 "`,
/// the PM must not trim it, select it as `1.0.0`, and then fetch a different
/// coordinate from `/coven/record`.
#[test]
fn resolver_rejects_whitespace_padded_registry_versions() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "coord");
    let app = fe.new_app();
    let lib = fe.lib("acme/coord", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    fe.publish_promote(&lib, "acme/coord", "1.0.0");

    let meta = server.regroot.join("registry/acme/coord/1.0.0/coven.json");
    let record = std::fs::read_to_string(&meta).unwrap();
    let tampered = record.replace("\"version\":\"1.0.0\"", "\"version\":\" 1.0.0 \"");
    assert!(tampered.contains("\"version\":\" 1.0.0 \""), "test must tamper the version field: {tampered}");
    std::fs::write(&meta, tampered).unwrap();

    let out = fe.pm(&app, &["add", "acme/coord"], None);
    assert!(!out.status.success(), "noncanonical registry coordinate must not resolve");
    let msg = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        msg.contains("malformed versions response"),
        "expected corrupt registry record rejection, got: {msg}"
    );
    assert!(!app.join("vendor/coord").exists(), "nothing should be vendored on a failed add");
}

/// Serve exactly one malformed `/coven/versions` response. Both add and update
/// tests use this boundary probe; any second request is evidence that PM erased
/// the first response instead of returning its typed error.
fn versions_mirror_once(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{ErrorKind, Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(e) if e.kind() == ErrorKind::WouldBlock && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("versions mirror did not receive a request: {e}"),
            }
        };
        let mut buf = [0u8; 2048];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req.split_whitespace().nth(1).unwrap_or("/");
        assert!(path.starts_with("/coven/versions"), "unexpected first registry request: {path}");

        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
    });
    (addr, server)
}

/// Mirror valid version/snapshot metadata but fail either the snapshot request
/// or the one root-key request used to verify and serialize the trust record.
fn trust_pin_failure_mirror(
    versions: String,
    snapshot: String,
    fail_snapshot: bool,
) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{ErrorKind, Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(e) if e.kind() == ErrorKind::WouldBlock && std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(e) => panic!("trust metadata mirror did not receive an expected request: {e}"),
                }
            };
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            let (status, body, done) = if path == "/" {
                // The compiled PM launcher probes an explicitly granted registry
                // address before execution. Readiness is transport setup, not one
                // of this fixture's scripted trust-metadata requests.
                ("200 OK", "ready".to_string(), false)
            } else if path.starts_with("/coven/versions") {
                ("200 OK", versions.clone(), false)
            } else if path.starts_with("/coven/snapshot") {
                if fail_snapshot {
                    ("503 Service Unavailable", "snapshot unavailable".to_string(), true)
                } else {
                    ("200 OK", snapshot.clone(), false)
                }
            } else if path.starts_with("/coven/rootpub") {
                ("503 Service Unavailable", "root key unavailable".to_string(), true)
            } else {
                panic!("unexpected trust metadata request: {path}");
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            if done {
                return;
            }
        }
    });
    (addr, server)
}

/// BUG-567: malformed registry data is not an empty registry. Version
/// resolution must preserve that distinction through its typed boundary and
/// stop before any record or source request.
#[test]
fn pm_add_rejects_malformed_versions_response() {
    let app = unique("badversions-app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(app.join("witchy.toml"), "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(app.join("src/app.witchy"), "fn main(console: Console):\n    console.print(\"app\")\n").unwrap();

    let (mirror_addr, mirror) = versions_mirror_once("{\"records\":42}");

    let out = Command::new(BIN)
        .current_dir(&app)
        .env("COVEN_URL", format!("http://{mirror_addr}"))
        .env("WITCHY_COOLDOWN_SECS", "0")
        .args(["pm", "add", "acme/bad@1.0.0"])
        .output()
        .expect("spawn witchy pm add");
    assert!(!out.status.success(), "malformed versions response must fail add");
    let msg = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        msg.contains("malformed versions response") && msg.contains("`records` is not an array"),
        "expected typed versions-response error, got: {msg}"
    );
    assert!(!app.join("vendor/bad").exists(), "malformed versions response must not be vendored");
    mirror.join().unwrap();
    std::fs::remove_dir_all(app).unwrap();
}

/// BUG-569: `pm update` must consume the same typed resolver error as `add`.
/// A malformed registry response is a failed update, not a successful no-op;
/// in particular it must not repin trust metadata or rewrite the lockfile.
#[test]
fn pm_update_preserves_malformed_versions_error() {
    let app = unique("badversions-update");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::create_dir_all(app.join("vendor/bad")).unwrap();
    std::fs::write(app.join("witchy.toml"), "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(app.join("src/app.witchy"), "fn main(console: Console):\n    console.print(\"app\")\n").unwrap();
    std::fs::write(
        app.join("vendor/bad/coven.json"),
        r#"{"name":"acme/bad","version":"1.0.0","state":"released","hash":"sha256:test","runtime_footprint":[]}"#,
    )
    .unwrap();
    let original_lock = "registry_snapshot_version = 7\nrootpub = \"pinned\"\n";
    std::fs::write(app.join("witchy.lock"), original_lock).unwrap();

    let (mirror_addr, mirror) = versions_mirror_once("{\"records\":42}");
    let out = Command::new(BIN)
        .current_dir(&app)
        .env("COVEN_URL", format!("http://{mirror_addr}"))
        .env("WITCHY_COOLDOWN_SECS", "0")
        .args(["pm", "update"])
        .output()
        .expect("spawn witchy pm update");

    assert!(!out.status.success(), "malformed versions response must fail update");
    let msg = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        msg.contains("cannot resolve an update") && msg.contains("malformed versions response"),
        "expected the typed resolver error at the update boundary, got: {msg}"
    );
    assert_eq!(
        std::fs::read_to_string(app.join("witchy.lock")).unwrap(),
        original_lock,
        "failed update must not rewrite or repin the lock"
    );
    mirror.join().unwrap();
    std::fs::remove_dir_all(app).unwrap();
}

/// BUG-571: transient metadata failures during lock regeneration must preserve
/// both trust pins. A missing snapshot is not "no TUF", and a failed root-key
/// refetch is not an instruction to write a lock without its TOFU anchor.
#[test]
fn pm_update_preserves_lock_when_trust_pin_fetches_fail() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "pin-fetch");
    let app = fe.new_app();
    fe.published_lib("acme/pinned", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let add = fe.pm(&app, &["add", "acme/pinned"], None);
    assert!(add.status.success(), "initial add failed: {}\n{}", stdout(&add), stderr(&add));

    let original_lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(
        original_lock.contains("registry_snapshot_version") && original_lock.contains("registry_rootpub"),
        "initial lock must carry both trust pins: {original_lock}"
    );

    let registry = format!("127.0.0.1:{}", server.port);
    let (status, versions) = http_get(&registry, "/coven/versions?name=acme/pinned");
    assert_eq!(status, 200);
    let (status, snapshot) = http_get(&registry, "/coven/snapshot");
    assert_eq!(status, 200);
    for fail_snapshot in [true, false] {
        let (mirror_addr, mirror) = trust_pin_failure_mirror(
            versions.clone(),
            snapshot.clone(),
            fail_snapshot,
        );
        let out = Command::new(BIN)
            .current_dir(&app)
            .env("COVEN_URL", format!("http://{mirror_addr}"))
            .env("WITCHY_COOLDOWN_SECS", "0")
            .args(["pm", "update"])
            .output()
            .expect("spawn witchy pm update");
        assert!(!out.status.success(), "missing trust metadata must fail update");
        let output = format!("{}{}", stdout(&out), stderr(&out));
        let expected = if fail_snapshot { "snapshot is unavailable" } else { "root key is unavailable" };
        assert!(output.contains(expected), "expected `{expected}` diagnostic, got: {output}");
        assert_eq!(
            std::fs::read_to_string(app.join("witchy.lock")).unwrap(),
            original_lock,
            "failed trust-pin fetch must preserve the existing lock"
        );
        mirror.join().unwrap();
    }
}

/// Promotion enforces separation of duties: the promoter must be a DISTINCT human
/// identity from the CI that uploaded it, and presents the out-of-band second
/// factor. The front-end stages with a CI token, then releases with a distinct
/// human token; a self-promote (CI promoting its own upload, after the human has
/// bound the maintainer policy) is refused 403.
#[test]
fn promote_requires_distinct_identity() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "factor");
    let dir = fe.lib("acme/lib", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let ci = server.ci_token("acme-lib-repo", "release.yml");
    let out = fe.pm(&dir, &["publish", "."], Some(&ci));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "publish: {}", stdout(&out));

    // A DISTINCT human identity promotes it to released (the first promoter binds
    // the namespace's maintainer policy via TOFU; it is not the uploader).
    let human = server.human_token("alice");
    let out = fe.pm(&dir, &["promote", "acme/lib", "1.0.0"], Some(&human));
    assert!(out.status.success(), "promote failed: {}", stderr(&out));
    assert!(stdout(&out).contains("promote: 200"), "expected 200: {}", stdout(&out));

    // Stage a second version, then have the CI try to self-promote it: the human
    // is the bound maintainer, so the CI is refused as not-a-maintainer (403) —
    // machines stage, humans release.
    std::fs::write(dir.join("witchy.toml"), "[rune]\nname = \"acme/lib\"\nversion = \"1.1.0\"\n").unwrap();
    let ci2 = server.ci_token("acme-lib-repo", "release.yml"); // a fresh per-run token (single-use)
    let out = fe.pm(&dir, &["publish", "."], Some(&ci2));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "republish: {}", stdout(&out));
    let out = fe.pm(&dir, &["promote", "acme/lib", "1.1.0"], Some(&ci));
    assert!(!out.status.success(), "a CI self-promote must be refused");
    assert!(stdout(&out).contains("promote: 403"), "expected 403: {}", stdout(&out));
}

/// BUG-379: maintainer policy is durable authority state. If
/// `_policy/<ns>/maintainers.json` is malformed or wrong-shaped, coven must report
/// registry corruption (500), not silently decode it as an empty maintainer set
/// and return the misleading authorization denial (403).
#[test]
fn corrupt_maintainer_policy_is_registry_state_error() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "corrupt-maintainers");
    let dir = fe.lib("acme/corrupt", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let ci = server.ci_token("acme-corrupt-repo", "release.yml");
    let out = fe.pm(&dir, &["publish", "."], Some(&ci));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "publish: {}", stdout(&out));

    let alice = server.human_token("alice");
    let out = fe.pm(&dir, &["promote", "acme/corrupt", "1.0.0"], Some(&alice));
    assert!(out.status.success() && stdout(&out).contains("promote: 200"), "promote: {}", stdout(&out));

    let policy = server.regroot.join("registry/_policy/acme/maintainers.json");
    assert!(policy.exists(), "first promotion must bind maintainer policy at {}", policy.display());
    std::fs::write(&policy, "{\"maintainers\":[]}").unwrap();

    std::fs::write(dir.join("witchy.toml"), "[rune]\nname = \"acme/corrupt\"\nversion = \"1.1.0\"\n").unwrap();
    let ci2 = server.ci_token("acme-corrupt-repo", "release.yml");
    let out = fe.pm(&dir, &["publish", "."], Some(&ci2));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "republish: {}", stdout(&out));

    let addr = format!("127.0.0.1:{}", server.port);
    let body = format!(
        "{{\"name\":\"acme/corrupt\",\"version\":\"1.1.0\",\"second_factor\":\"webauthn\",\"promoted_by\":\"alice\",\"id_token\":{}}}",
        json_str(&alice),
    );
    let (status, body) = http_post(&addr, "/coven/promote", &body);
    assert_eq!(status, 500, "corrupt maintainer policy must fail as registry state: {body}");
    assert!(body.contains("corrupt maintainer policy"), "expected corruption message, got: {body}");
}

/// The supply-chain gate on a DIRECT `add` (§10): adding a rune that introduces a
/// capability the project does not already admit BLOCKS and writes nothing, until
/// the consumer consents with `--allow-cap <Cap>`. The front-end diffs the rune's
/// signed-record footprint against the project's baseline (its own declared caps ∪
/// what the lock already records ∪ the consented caps).
#[test]
fn gate_blocks_capability_widening_then_allows_with_consent() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "gate");
    let app = fe.new_app();
    // A library that demands Net in its public API (the registry footprints it
    // server-side, so it must honestly declare Net to publish).
    let dir = fe.lib(
        "acme/netkit",
        "0.1.0",
        "pub fn fetch(net: Net, url: String) -> String:\n    url\n",
    );
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/netkit\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Net\"]\n",
    )
    .unwrap();
    fe.publish_promote(&dir, "acme/netkit", "0.1.0");

    // Adding it to a pure app must BLOCK and write nothing.
    let out = fe.pm(&app, &["add", "acme/netkit"], None);
    assert!(!out.status.success(), "expected block, got success");
    assert!(stdout(&out).contains("BLOCKED"), "got: {}", stdout(&out));
    assert!(stdout(&out).contains("Net"));
    assert!(!app.join("witchy.lock").exists(), "lock must not be written on block");
    assert!(!app.join("vendor/netkit").exists(), "nothing vendored on block");

    // With explicit consent, it proceeds and records Net.
    let out = fe.pm(&app, &["add", "acme/netkit", "--allow-cap", "Net"], None);
    assert!(out.status.success(), "consented add failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(app.join("witchy.lock").exists());
    assert!(app.join("vendor/netkit/src/netkit.witchy").exists(), "consented add must vendor");

    // A re-add of the same rune now passes silently — Net is already in the
    // baseline (the lock records it), so it is no longer a widening.
    let out = fe.pm(&app, &["add", "acme/netkit"], None);
    assert!(out.status.success(), "re-add of an already-admitted rune must not gate: {}", stdout(&out));
}

/// SEC-006: the gate covers the whole resolved CLOSURE, not just the direct rune. A
/// rune with a pure public API (so its single-rune registry footprint shows nothing)
/// that pulls a Net-demanding transitive dep must still BLOCK — otherwise a dep-of-a-dep
/// silently widens the project's capability footprint. `--allow-cap` consents to the tree.
#[test]
fn transitive_capability_widening_is_gated() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "transgate");
    let app = fe.new_app();

    // A transitive dep that demands Net (declared, so the registry admits it).
    let sneaky = fe.lib("acme/sneaky", "1.0.0", "pub fn fetch(net: Net, u: String) -> String:\n    u\n");
    std::fs::write(
        sneaky.join("witchy.toml"),
        "[rune]\nname = \"acme/sneaky\"\nversion = \"1.0.0\"\n\n[capabilities]\nruntime = [\"Net\"]\n",
    )
    .unwrap();
    fe.publish_promote(&sneaky, "acme/sneaky", "1.0.0");

    // An innocent-looking rune: PURE public API, but it depends on sneaky.
    let innocent = fe.lib("acme/innocent", "1.0.0", "pub fn greet(s: String) -> String:\n    \"hi \" + s\n");
    std::fs::write(
        innocent.join("witchy.toml"),
        "[rune]\nname = \"acme/innocent\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/sneaky\" = { version = \"^1.0.0\" }\n",
    )
    .unwrap();
    fe.publish_promote(&innocent, "acme/innocent", "1.0.0");

    // Adding the pure-looking innocent must BLOCK on the transitive Net.
    let out = fe.pm(&app, &["add", "acme/innocent"], None);
    assert!(!out.status.success(), "a transitive Net must block the add");
    assert!(stdout(&out).contains("BLOCKED") && stdout(&out).contains("Net"), "transitive block: {}", stdout(&out));

    // With consent, the whole tree is admitted and both runes vendor.
    let out = fe.pm(&app, &["add", "acme/innocent", "--allow-cap", "Net"], None);
    assert!(out.status.success(), "consented add failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(app.join("vendor/innocent/src/innocent.witchy").exists(), "innocent must vendor");
    assert!(app.join("vendor/sneaky/src/sneaky.witchy").exists(), "the transitive sneaky must vendor");
}

/// `pm add` resolves transitively: adding `acme/http` (which declares a version
/// dependency on `acme/url`) pulls BOTH into the project's `vendor/` tree and
/// pins both in `witchy.lock`. The front-end walks each fetched rune's manifest
/// version-deps, fetching the closure (the vendored tree doubles as the
/// visited-set, so a diamond terminates).
#[test]
fn transitive_dependency_add_pulls_the_closure() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "transitive");
    let app = fe.new_app();
    fe.published_lib("acme/url", "1.0.0", "pub fn parse(s: String) -> String:\n    s\n");

    // http depends on url and demands Net (declared, so the server admits it).
    let dir = fe.base.join("http");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/http\"\nversion = \"1.0.0\"\n\n[capabilities]\nruntime = [\"Net\"]\n\n[dependencies]\n\"acme/url\" = { version = \"^1.0.0\" }\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/http.witchy"), "pub fn get(net: Net, u: String) -> String:\n    u\n").unwrap();
    fe.publish_promote(&dir, "acme/http", "1.0.0");

    // Adding http pulls url transitively into the vendored tree + the lock. http
    // demands Net, so the DIRECT add gates and needs `--allow-cap Net`; the
    // transitive `url` (pure) is pulled by the consented direct add without
    // re-gating — consenting to a rune consents to its declared dependency tree.
    let out = fe.pm(&app, &["add", "acme/http", "--allow-cap", "Net"], None);
    assert!(out.status.success(), "add failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(app.join("vendor/http/src/http.witchy").exists(), "http must vendor");
    assert!(app.join("vendor/url/src/url.witchy").exists(), "the transitive url must vendor");
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    // BUG-193: lock identity binds to the dependency's MANIFEST name (`acme/http`),
    // not its spoofable import alias (`http`); the alias is recorded separately.
    assert!(
        lock.contains("name = \"acme/http\"") && lock.contains("alias = \"http\""),
        "lock must pin http by its manifest identity: {lock}"
    );
    assert!(
        lock.contains("name = \"acme/url\"") && lock.contains("alias = \"url\""),
        "lock must pin the transitive url by its manifest identity: {lock}"
    );
}

/// The supply-chain gate on a registry upgrade: `pm update` re-resolves a vendored
/// dependency to its latest released version, but BLOCKS when that version's
/// declared capability footprint WIDENS beyond the vendored baseline — the classic
/// "a patch release quietly starts phoning home" attack. The front-end compares the
/// incoming version's signed-record footprint against the locked one; `--allow-cap`
/// consents to the widening so an intended upgrade proceeds.
#[test]
fn upgrade_that_widens_is_gated() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "upgrade");
    let app = fe.new_app();

    // v1.0.0 of a logger: pure, no capabilities.
    fe.published_lib("acme/logger", "1.0.0", "pub fn line(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/logger"], None);
    assert!(out.status.success(), "add v1 failed: {}\n{}", stderr(&out), stdout(&out));

    // v1.1.0 quietly starts demanding Net (a classic account-takeover scenario).
    // The registry recomputes the footprint server-side, so the new version must
    // honestly declare Net to publish — it lands as a wider-authority release.
    let dir = fe.base.join("logger");
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/logger\"\nversion = \"1.1.0\"\n\n[capabilities]\nruntime = [\"Net\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/logger.witchy"),
        "pub fn line(s: String) -> String:\n    s\npub fn beacon(net: Net, s: String) -> String:\n    s\n",
    )
    .unwrap();
    fe.publish_promote(&dir, "acme/logger", "1.1.0");

    // `update` must BLOCK: the upgrade widens logger's footprint with Net.
    let out = fe.pm(&app, &["update"], None);
    assert!(!out.status.success(), "update should block the widening upgrade");
    assert!(stdout(&out).contains("Net"), "got: {}", stdout(&out));
    // The gate left the dependency at its safe version (nothing re-fetched).
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("1.0.0"), "blocked update must not move the lock: {lock}");

    // With consent it proceeds.
    let out = fe.pm(&app, &["update", "--allow-cap", "Net"], None);
    assert!(out.status.success(), "consented update failed: {}\n{}", stderr(&out), stdout(&out));
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("1.1.0"), "lock should pin the upgraded version: {lock}");
}

/// BUG-232: `pm update` must not decide the widening gate from an unverified
/// incoming record. A hostile mirror could under-declare a wider release's
/// `runtime_footprint`; the update must block at the gate before trusting that
/// field or fetching source.
#[test]
fn update_widening_gate_requires_verified_incoming_footprint() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "bug232");
    let app = fe.new_app();

    fe.published_lib("acme/logger", "1.0.0", "pub fn line(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/logger"], None);
    assert!(out.status.success(), "add v1 failed: {}\n{}", stderr(&out), stdout(&out));

    let dir = fe.base.join("logger");
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/logger\"\nversion = \"1.1.0\"\n\n[capabilities]\nruntime = [\"Net\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/logger.witchy"),
        "pub fn line(s: String) -> String:\n    s\npub fn beacon(net: Net, s: String) -> String:\n    s\n",
    )
    .unwrap();
    fe.publish_promote(&dir, "acme/logger", "1.1.0");

    let record = server.regroot.join("registry/acme/logger/1.1.0/coven.json");
    let original = std::fs::read_to_string(&record).unwrap();
    let body = original.replace(r#""runtime_footprint":["Net"]"#, r#""runtime_footprint":[]"#);
    assert_ne!(body, original, "test must actually tamper the incoming record footprint");
    std::fs::write(&record, body).unwrap();

    let out = fe.pm(&app, &["update"], None);
    assert!(!out.status.success(), "tampered incoming footprint must block update");
    assert!(
        stdout(&out).contains("cannot verify the registry record") && stdout(&out).contains("not validly signed"),
        "update must fail at the verified-footprint gate: {}",
        stdout(&out)
    );
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("1.0.0"), "blocked update must not move the lock: {lock}");
    let vendored = std::fs::read_to_string(app.join("vendor/logger/src/logger.witchy")).unwrap();
    assert!(!vendored.contains("beacon"), "blocked update must not vendor the tampered release: {vendored}");
}

/// A diamond resolves the shared base exactly once: `acme/left` and `acme/right`
/// both depend on `acme/base`; adding both vendors `base` a single time (the
/// vendored tree is the visited-set) and the lock pins it once. The consumer then
/// builds against the deduplicated vendored closure.
#[test]
fn diamond_dependency_resolves_shared_base_once() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "diamond");
    let app = fe.new_app();
    fe.published_lib("acme/base", "1.0.0", "pub fn b(s: String) -> String:\n    s\n");

    // left and right both depend on base.
    for side in ["left", "right"] {
        let dir = fe.base.join(side);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("witchy.toml"),
            format!("[rune]\nname = \"acme/{side}\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/base\" = {{ version = \"^1.0.0\" }}\n"),
        )
        .unwrap();
        std::fs::write(dir.join(format!("src/{side}.witchy")), "pub fn x(s: String) -> String:\n    s\n").unwrap();
        fe.publish_promote(&dir, &format!("acme/{side}"), "1.0.0");
    }

    let out = fe.pm(&app, &["add", "acme/left"], None);
    assert!(out.status.success(), "add left failed: {}\n{}", stderr(&out), stdout(&out));
    let out = fe.pm(&app, &["add", "acme/right"], None);
    assert!(out.status.success(), "add right failed: {}\n{}", stderr(&out), stdout(&out));

    // base appears exactly once in the lock despite two paths to it. BUG-193: the
    // lock pins it by its manifest identity `acme/base` (alias `base`), so match on
    // the identity key.
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    let occurrences = lock.matches("name = \"acme/base\"").count();
    assert_eq!(occurrences, 1, "shared base must resolve once; lock:\n{lock}");
    assert!(app.join("vendor/base/src/base.witchy").exists(), "base must vendor once");
}

/// `pm update <name>` re-resolves only the named vendored dependency to its
/// latest released version, re-fetching + re-vendoring it and rewriting the lock;
/// the other dependencies stay pinned at their vendored versions.
#[test]
fn update_single_package_leaves_others_pinned() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "update1");
    let app = fe.new_app();
    fe.published_lib("acme/alfa", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    fe.published_lib("acme/bravo", "1.0.0", "pub fn g(s: String) -> String:\n    s\n");
    assert!(fe.pm(&app, &["add", "acme/alfa"], None).status.success(), "add alfa");
    assert!(fe.pm(&app, &["add", "acme/bravo"], None).status.success(), "add bravo");

    // Newer versions of both become available.
    for n in ["alfa", "bravo"] {
        let dir = fe.lib(&format!("acme/{n}"), "1.1.0", "pub fn f(s: String) -> String:\n    s\n");
        fe.publish_promote(&dir, &format!("acme/{n}"), "1.1.0");
    }

    // Update only alfa; bravo must stay pinned at 1.0.0.
    let out = fe.pm(&app, &["update", "alfa"], None);
    assert!(out.status.success(), "update failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("updated acme/alfa 1.0.0 -> 1.1.0"), "update: {}", stdout(&out));
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    let a_at = lock.find("\"alfa\"").map(|i| &lock[i..i + 60]).unwrap_or("");
    assert!(a_at.contains("1.1.0"), "alfa should be 1.1.0; lock:\n{lock}");
    let b_at = lock.find("\"bravo\"").map(|i| &lock[i..i + 60]).unwrap_or("");
    assert!(b_at.contains("1.0.0"), "bravo should stay 1.0.0; lock:\n{lock}");
}

/// Provenance is always recorded: a rune fetched by `pm add` carries its signed
/// `coven.json` beside the vendored source, and that record holds the SLSA
/// trusted-publisher attestation (issuer / repository / workflow / digest). The
/// front-end pins provenance with the source — re-verifiable offline.
#[test]
fn provenance_is_always_recorded() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "prov");
    let app = fe.new_app();
    fe.published_lib("acme/papa", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/papa"], None);
    assert!(out.status.success(), "add failed: {}", stderr(&out));

    let record = std::fs::read_to_string(app.join("vendor/papa/coven.json"))
        .expect("the vendored rune must carry its signed coven.json");
    assert!(record.contains("trusted-publisher"), "record must carry the SLSA attestation: {record}");
    assert!(record.contains("repository=acme-repo"), "provenance must name the repo: {record}");
}

/// The Ed25519 signature catches metadata tampering that content hashing alone
/// would miss. After `pm add` vendors a rune, editing a SIGNED field of its
/// `coven.json` (here the `uploaded_by` identity — the source bytes are untouched,
/// so the content hash still matches) is detected by `pm verify-rune`: the record
/// no longer verifies against the registry root key.
#[test]
fn signature_detects_registry_metadata_tampering() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "tamper");
    let app = fe.new_app();
    fe.published_lib("acme/xray", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/xray"], None);
    assert!(out.status.success(), "add failed: {}", stderr(&out));

    let rootpub = server.rootpub();
    // A healthy verify-rune passes (signature + content).
    let out = fe.pm(&app, &["verify-rune", "vendor/xray", &rootpub], None);
    assert!(out.status.success() && stdout(&out).contains("verified"), "healthy verify: {}", stdout(&out));

    // Attacker edits a SIGNED field of the vendored record (source untouched).
    let meta = app.join("vendor/xray/coven.json");
    let json = std::fs::read_to_string(&meta).unwrap().replace("alice", "attacker");
    std::fs::write(&meta, json).unwrap();

    // verify-rune must reject the tampered record via the signature.
    let out = fe.pm(&app, &["verify-rune", "vendor/xray", &rootpub], None);
    assert!(!out.status.success(), "tampered metadata must fail verify");
    assert!(stdout(&out).contains("BLOCK"), "verify-rune: {}", stdout(&out));
}

#[test]
fn malformed_vendored_record_blocks_verify_update_and_lock_regeneration() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "badrecord");
    let app = fe.new_app();
    fe.published_lib("acme/broken", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/broken"], None);
    assert!(out.status.success(), "add failed: {}", stderr(&out));
    let original_lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();

    let meta = app.join("vendor/broken/coven.json");
    std::fs::write(&meta, "{").unwrap();

    let out = fe.pm(&app, &["verify-rune", "vendor/broken", &server.rootpub()], None);
    assert!(!out.status.success(), "malformed coven.json must fail verify");
    assert!(stdout(&out).contains("not validly signed"), "verify-rune: {}", stdout(&out));

    let out = fe.pm(&app, &["update"], None);
    assert!(!out.status.success(), "malformed coven.json must fail update");
    assert!(
        stdout(&out).contains("vendored coven.json is not valid JSON"),
        "update must preserve the vendored-record parse error: {}",
        stdout(&out)
    );
    assert_eq!(
        std::fs::read_to_string(app.join("witchy.lock")).unwrap(),
        original_lock,
        "failed update must preserve the existing lock"
    );

    fe.published_lib("acme/fresh", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/fresh"], None);
    assert!(!out.status.success(), "a corrupt existing record must block lock regeneration");
    assert!(
        stdout(&out).contains("cannot regenerate witchy.lock")
            && stdout(&out).contains("vendored coven.json is not valid JSON"),
        "add must report the exact corrupt lock input: {}",
        stdout(&out)
    );
    assert_eq!(
        std::fs::read_to_string(app.join("witchy.lock")).unwrap(),
        original_lock,
        "failed lock regeneration must preserve the existing lock"
    );
}

#[test]
fn pm_add_rejects_malformed_source_response_before_hashing() {
    use std::io::{Read, Write};

    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "badsource");
    let app = fe.new_app();
    fe.published_lib("acme/srcbad", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");

    let rootpub = server.rootpub();
    let (status, versions) = http_get(&format!("127.0.0.1:{}", server.port), "/coven/versions?name=acme/srcbad");
    assert_eq!(status, 200, "versions fetch failed");
    let record = std::fs::read_to_string(server.regroot.join("registry/acme/srcbad/1.0.0/coven.json")).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mirror_addr = listener.local_addr().unwrap().to_string();
    let mirror = std::thread::spawn(move || {
        for _ in 0..8 {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/");
            let mut done = false;
            let body = if path.starts_with("/coven/rootpub") {
                rootpub.clone()
            } else if path.starts_with("/coven/versions") {
                versions.clone()
            } else if path.starts_with("/coven/record") {
                record.clone()
            } else if path.starts_with("/coven/source") {
                done = true;
                "{\"files\":42}".to_string()
            } else {
                "not found".to_string()
            };
            let status = if body == "not found" { "404 Not Found" } else { "200 OK" };
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes());
            if done {
                return;
            }
        }
    });

    let out = Command::new(BIN)
        .current_dir(&app)
        .env("COVEN_URL", format!("http://{mirror_addr}"))
        .env("WITCHY_COOLDOWN_SECS", "0")
        .args(["pm", "add", "acme/srcbad@1.0.0"])
        .output()
        .expect("spawn witchy pm add");
    assert!(!out.status.success(), "malformed source response must fail add");
    assert!(
        stdout(&out).contains("malformed source response"),
        "add output: status {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        stdout(&out),
        stderr(&out)
    );
    assert!(!app.join("vendor/srcbad").exists(), "malformed source must not be vendored");
    let _ = mirror.join();
}

/// BUG-363: capability strings can contain commas (`Net[Connect, Tcp]`). The
/// signed record payload must therefore bind the JSON array shape, not only the
/// comma-joined projection of its elements.
#[test]
fn signature_detects_runtime_footprint_shape_tampering() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "rtshape");
    let app = fe.new_app();
    let lib = fe.lib("acme/netcap", "1.0.0", "pub fn f(net: Net[Connect, Tcp]) -> Int:\n    1\n");
    std::fs::write(
        lib.join("witchy.toml"),
        "[rune]\nname = \"acme/netcap\"\nversion = \"1.0.0\"\n\n[capabilities]\nruntime = [\"Net[Connect, Tcp]\"]\n",
    )
    .unwrap();
    fe.publish_promote(&lib, "acme/netcap", "1.0.0");
    let out = fe.pm(&app, &["add", "acme/netcap", "--allow-cap", "Net[Connect, Tcp]"], None);
    assert!(out.status.success(), "add failed:\nstdout: {}\nstderr: {}", stdout(&out), stderr(&out));

    let rootpub = server.rootpub();
    let out = fe.pm(&app, &["verify-rune", "vendor/netcap", &rootpub], None);
    assert!(out.status.success() && stdout(&out).contains("verified"), "healthy verify: {}", stdout(&out));
    let out = fe.pm(&app, &["verify"], None);
    assert!(out.status.success(), "healthy offline verify failed: {}", stdout(&out));

    let meta = app.join("vendor/netcap/coven.json");
    let json = std::fs::read_to_string(&meta)
        .unwrap()
        .replace("\"runtime_footprint\":[\"Net[Connect, Tcp]\"]", "\"runtime_footprint\":[\"Net[Connect\",\" Tcp]\"]");
    std::fs::write(&meta, json).unwrap();

    let out = fe.pm(&app, &["verify-rune", "vendor/netcap", &rootpub], None);
    assert!(!out.status.success(), "shape-tampered footprint must fail verify");
    assert!(stdout(&out).contains("BLOCK"), "verify-rune: {}", stdout(&out));
    let out = fe.pm(&app, &["verify"], None);
    assert!(!out.status.success(), "shape-tampered footprint must fail offline verify");
    assert!(stdout(&out).contains("failed pinned-root verification"), "verify: {}", stdout(&out));
}

/// A fetched rune carries its signed provenance and every consumption path
/// re-verifies it offline. `verify`, `build`, and the narrow `verify-rune` command
/// all bind the vendored source to the signed record, lock coordinate, and pinned
/// registry root key without contacting the registry.
#[test]
fn vendored_rune_reverifies_offline_against_the_root_key() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "pin");
    let app = fe.new_app();
    fe.published_lib("acme/yankee", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/yankee"], None);
    assert!(out.status.success(), "add failed: {}", stderr(&out));

    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("hash = \"sha256:"), "lock must pin the content hash: {lock}");

    // Offline re-verification against the registry root key (signature + content).
    let rootpub = server.rootpub();
    let out = fe.pm(&app, &["verify-rune", "vendor/yankee", &rootpub], None);
    assert!(out.status.success(), "verify-rune failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("verified"), "verify-rune: {}", stdout(&out));

    let offline = |args: &[&str]| {
        let mut full = vec!["pm"];
        full.extend_from_slice(args);
        Command::new(BIN)
            .current_dir(&app)
            .env("COVEN_URL", "127.0.0.1:1")
            .args(full)
            .output()
            .expect("run offline pm command")
    };
    let out = offline(&["verify"]);
    assert!(out.status.success(), "offline verify failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("every locked hash matches"), "verify: {}", stdout(&out));
    let out = offline(&["build", "."]);
    assert!(out.status.success(), "offline build failed: {}\n{}", stderr(&out), stdout(&out));
    let out = offline(&["run", "."]);
    assert!(out.status.success(), "offline run failed: {}\n{}", stderr(&out), stdout(&out));

    // Tampering the vendored source breaks the content check.
    let source_path = app.join("vendor/yankee/src/yankee.witchy");
    let original_source = std::fs::read_to_string(&source_path).unwrap();
    std::fs::write(&source_path, "pub fn f(s: String) -> String:\n    \"evil\"\n").unwrap();
    let out = fe.pm(&app, &["verify-rune", "vendor/yankee", &rootpub], None);
    assert!(!out.status.success(), "tampered source must fail re-verification");
    assert!(stdout(&out).contains("BLOCK"), "verify-rune: {}", stdout(&out));
    let out = offline(&["verify"]);
    assert!(!out.status.success(), "tampered source must fail offline verify");
    assert!(stdout(&out).contains("source no longer matches"), "verify: {}", stdout(&out));
    let out = offline(&["build", "."]);
    assert!(!out.status.success(), "tampered source must fail before build");
    assert!(stdout(&out).contains("source no longer matches"), "build: {}", stdout(&out));
    let out = offline(&["run", "."]);
    assert!(!out.status.success(), "tampered source must fail before run");
    assert!(stdout(&out).contains("source no longer matches"), "run: {}", stdout(&out));

    // A source-preserving signature edit must also fail before the compiler runs.
    std::fs::write(&source_path, original_source).unwrap();
    let record_path = app.join("vendor/yankee/coven.json");
    let original_record = std::fs::read_to_string(&record_path).unwrap();
    let mut record: serde_json::Value = serde_json::from_str(&original_record).unwrap();
    record["sig"] = serde_json::Value::String("00".repeat(64));
    std::fs::write(&record_path, serde_json::to_string(&record).unwrap()).unwrap();
    let out = offline(&["build", "."]);
    assert!(!out.status.success(), "bad record signature must fail before build");
    assert!(stdout(&out).contains("failed pinned-root verification"), "build: {}", stdout(&out));

    // The signed coordinate and root key are lock invariants, not advisory
    // metadata. Neither may be edited while preserving a green verification.
    std::fs::write(&record_path, original_record.clone()).unwrap();
    let lock_path = app.join("witchy.lock");
    std::fs::write(
        &lock_path,
        lock.replacen("name = \"acme/yankee\"", "name = \"evil/yankee\"", 1),
    )
    .unwrap();
    let out = offline(&["verify"]);
    assert!(!out.status.success(), "coordinate-substituted lock must fail verify");
    assert!(stdout(&out).contains("coven.json `name`"), "verify: {}", stdout(&out));

    std::fs::write(
        &lock_path,
        lock.replacen("source = \"coven\"", "source = \"path:vendor/yankee\"", 1),
    )
    .unwrap();
    let out = offline(&["verify"]);
    assert!(!out.status.success(), "registry entry disguised as a path entry must fail verify");
    assert!(stdout(&out).contains("not in the manifest dependency closure"), "verify: {}", stdout(&out));

    let without_root = lock
        .lines()
        .filter(|line| !line.starts_with("registry_rootpub ="))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&lock_path, without_root).unwrap();
    let out = offline(&["verify"]);
    assert!(!out.status.success(), "registry lock without a root pin must fail verify");
    assert!(
        stdout(&out).contains(
            "registry_snapshot_version is present but registry_rootpub is missing"
        ),
        "verify: {}",
        stdout(&out),
    );

    // A lock entry without its vendored directory is a hard failure, not a
    // silently omitted dependency.
    std::fs::write(&lock_path, lock).unwrap();
    std::fs::remove_dir_all(app.join("vendor/yankee")).unwrap();
    let out = offline(&["verify"]);
    assert!(!out.status.success(), "missing locked vendor must fail verify");
    assert!(stdout(&out).contains("locked registry dependency vendor/yankee is missing"), "verify: {}", stdout(&out));
}

/// A dependency whose module shadows the standard library (a rune named
/// `evil/list`, exposing a module `list`) cannot impersonate std: building a
/// consumer that imports the shadowing module fails — the std `list` the prelude
/// and generated code rely on is not the vendored impostor, so the link breaks.
#[test]
fn std_shadowing_dependency_is_refused() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "shadow");
    let app = fe.new_app();
    fe.published_lib("evil/list", "1.0.0", "pub fn rng(n: Int) -> Int:\n    0\n");

    let out = fe.pm(&app, &["add", "evil/list"], None);
    assert!(out.status.success(), "add failed: {}\n{}", stderr(&out), stdout(&out));
    std::fs::write(
        app.join("src/app.witchy"),
        "import list\n\nfn main(console: Console):\n    console.print(\"x\")\n",
    )
    .unwrap();
    let out = fe.pm(&app, &["build", "."], None);
    assert!(!out.status.success(), "a std-shadowing rune must be refused at build");
    let s = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        s.contains("link error") || s.contains("shadow") || s.contains("module `list`"),
        "build should refuse the shadowing dep: {s}"
    );
}

/// Two distinct registry runes that share a basename (`a/util` and `b/util`) both
/// vendor to `vendor/util`, so the second would silently shadow the first — an
/// `import util` cannot name both. The front-end `pm add` catches the collision and
/// refuses: the first add vendors `a/util`, the second (`b/util`) is blocked because
/// `vendor/util` already holds a rune whose full name differs.
#[test]
fn module_name_collision_between_deps_is_caught() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "collision");
    let app = fe.new_app();
    // Two different runes that both expose a module named `util`.
    for ns in ["a", "b"] {
        fe.published_lib(&format!("{ns}/util"), "1.0.0", "pub fn helper(s: String) -> String:\n    s\n");
    }
    let out = fe.pm(&app, &["add", "a/util"], None);
    assert!(out.status.success(), "add a/util failed: {}\n{}", stderr(&out), stdout(&out));
    let out = fe.pm(&app, &["add", "b/util"], None);
    assert!(!out.status.success(), "module collision must be caught");
    assert!(
        stdout(&out).contains("collision"),
        "stdout {} stderr {}",
        stdout(&out),
        stderr(&out)
    );
    // Only the first rune's source is vendored; the colliding one was refused.
    assert!(app.join("vendor/util/src/util.witchy").exists(), "first dep must vendor");
}

/// `pm add` vendors the dependency source INTO the project, so build/run are
/// offline by construction: once a rune is vendored under `vendor/` and pinned in
/// `witchy.lock`, the registry is no longer consulted. We prove it by dropping the
/// whole server after the add and running the consumer — it builds straight from
/// the committed vendor tree (the front-end's offline store IS the vendor dir).
#[test]
fn vendored_sources_build_with_no_registry() {
    let app = {
        let server = RegistryServer::start();
        let fe = FrontEnd::new(&server, "offline");
        let app = fe.new_app();
        fe.published_lib("acme/lib", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
        let out = fe.pm(&app, &["add", "acme/lib"], None);
        assert!(out.status.success(), "add failed: {}\n{}", stderr(&out), stdout(&out));
        std::fs::write(
            app.join("src/app.witchy"),
            "import lib\n\nfn main(console: Console):\n    console.print(lib.f(\"vend\"))\n",
        )
        .unwrap();
        assert!(app.join("vendor/lib/src/lib.witchy").exists(), "the dep source must be vendored");
        // Leak the FrontEnd's base so the vendored tree survives the server drop.
        std::mem::forget(fe);
        app
        // `server` (and its child process) is dropped here — the registry is gone.
    };

    // With NO registry running, the run is offline, served from the vendor tree.
    let out = Command::new(BIN)
        .current_dir(&app)
        .args(["pm", "run", "."])
        .output()
        .expect("spawn witchy pm run");
    assert!(out.status.success(), "offline run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("vend"), "got: {}", stdout(&out));
    let _ = std::fs::remove_dir_all(app.parent().unwrap());
}

/// `pm outdated <dir> <host:port>` reports, for each registry dependency the
/// manifest declares, its requirement and the latest released version available.
/// Initially the latest matches the requirement floor; after a newer version is
/// published + promoted, `outdated` surfaces it.
#[test]
fn outdated_reports_newer_versions() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "outdated");
    fe.published_lib("acme/lib", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");

    // A consumer that declares a registry dependency on acme/lib.
    let app = fe.new_app();
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"acme/lib\" = { version = \"^1.0.0\" }\n",
    )
    .unwrap();
    let hostport = format!("127.0.0.1:{}", server.port);

    // Only 1.0.0 exists so far.
    let out = fe.pm(&app, &["--net", &hostport, "outdated", ".", &hostport], None);
    assert!(out.status.success(), "outdated failed: {}", stderr(&out));
    assert!(stdout(&out).contains("acme/lib: req ^1.0.0, latest 1.0.0"), "outdated: {}", stdout(&out));

    // Publish + promote a newer version.
    let dir = fe.lib("acme/lib", "1.1.0", "pub fn f(s: String) -> String:\n    s\n");
    fe.publish_promote(&dir, "acme/lib", "1.1.0");

    let out = fe.pm(&app, &["--net", &hostport, "outdated", ".", &hostport], None);
    assert!(stdout(&out).contains("acme/lib: req ^1.0.0, latest 1.1.0"), "outdated: {}", stdout(&out));
}

/// `pm tree` shows a rune and its direct dependencies (from the manifest), each
/// annotated with its source (a registry `version:` requirement or a `path:`).
/// The front-end's tree is manifest-direct; the transitive closure lives in the
/// resolved `witchy.lock`/`vendor/` tree (covered by the add tests).
#[test]
fn tree_shows_direct_deps_with_sources() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "tree");
    let app = fe.new_app();
    fe.published_lib("acme/url", "1.0.0", "pub fn parse(s: String) -> String:\n    s\n");

    // Declare a registry dependency in the consumer's manifest.
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"acme/url\" = { version = \"^1.0.0\" }\n",
    )
    .unwrap();

    let out = fe.pm(&app, &["tree", "."], None);
    let s = stdout(&out);
    assert!(out.status.success(), "tree failed: {}", stderr(&out));
    assert!(s.contains("app"), "tree should name the rune: {s}");
    assert!(s.contains("acme/url"), "tree should list the dependency: {s}");
    assert!(s.contains("version:^1.0.0"), "tree should annotate the source: {s}");
}

#[test]
fn path_dependency_builds_and_runs() {
    // A two-rune workspace: an `app` with a path dependency on a sibling `greet`
    // library, both on disk (no registry involved). Driven through the embedded
    // witchy front-end (`witchy pm run`), proving the front-end resolves a sibling
    // path dependency and links it into the program.
    let work = unique("path");
    let lib = work.join("greet");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"greet\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(
        lib.join("src/greet.witchy"),
        "pub fn hi(s: String) -> String:\n    \"hi \" + s\n",
    )
    .unwrap();

    let app = work.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"greet\" = { path = \"../greet\" }\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/app.witchy"),
        "import greet\n\nfn main(console: Console):\n    console.print(greet.hi(\"witchy\"))\n",
    )
    .unwrap();

    // Run from the workspace root so the project Dir reaches the `../greet` sibling.
    let out = pm_fe(&work, &["run", "app"]);
    assert!(out.status.success(), "pm run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("hi witchy"), "got: {}", stdout(&out));
    assert!(
        app.join("witchy.lock").exists(),
        "the first path-only run must materialize the lock it verifies"
    );

    // It also builds (links the path dep without running).
    let out = pm_fe(&work, &["build", "app"]);
    assert!(out.status.success(), "pm build failed: {}", stderr(&out));
    assert!(stdout(&out).contains("ok"), "build output: {}", stdout(&out));
}

/// Build-time execution is **default-deny** in the front-end — even for a "safe"
/// build step that demands only the confined `BuildOut` sandbox. A `build`/`run`
/// refuses the very *existence* of a dependency's build step until the consumer
/// writes a `[build.grants."name"]` section: you consent to any code execution
/// before you consent to safe code execution. An empty section is that consent
/// (it permits only `BuildOut`). The path dependency is declared straight in the
/// consumer's manifest (the front-end resolves path deps from `path =`, not a
/// `pm add` step), and the front-end prints its decisions to stdout.
#[test]
fn build_steps_are_default_deny_even_when_safe() {
    let work = unique("builddeny");
    let lib = work.join("safegen");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"safegen\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(
        lib.join("src/safegen.witchy"),
        "pub fn shout(s: String) -> String:\n    \"HEY \" + s\n",
    )
    .unwrap();
    // A BuildOut-only build step: writes into its confined sandbox, nothing else.
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    out.write_out(\"gen.witchy\", \"// generated\")\n",
    )
    .unwrap();

    let app = work.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n\n[dependencies]\n\"safegen\" = { path = \"../safegen\" }\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/app.witchy"),
        "fn main(console: Console):\n    console.print(\"ok\")\n",
    )
    .unwrap();

    // Default-deny: the build refuses while no [build.grants."safegen"] section
    // exists at all — you consent to ANY build-time code execution first.
    let out = pm_fe(&work, &["build", "app"]);
    assert!(!out.status.success(), "a build step must be denied without a grants section");
    assert!(
        stdout(&out).contains("build-time code execution is denied by default"),
        "denial should say why: {}",
        stdout(&out)
    );

    // The empty section is the explicit consent — it grants only BuildOut.
    let manifest = std::fs::read_to_string(app.join("witchy.toml")).unwrap();
    std::fs::write(
        app.join("witchy.toml"),
        format!("{manifest}\n[build.grants.\"safegen\"]\n"),
    )
    .unwrap();
    let out = pm_fe(&work, &["build", "app"]);
    assert!(
        out.status.success(),
        "an empty grants section accepts a BuildOut-only step: {}\n{}",
        stderr(&out),
        stdout(&out)
    );

    let _ = std::fs::remove_dir_all(&work);
}

/// Staging cooldown (§8): a freshly released version is not resolvable until its
/// cooldown window passes — protection against a compromised release being
/// consumed the moment it lands — unless the consumer explicitly accepts it with
/// `--allow-fresh`. The `released_at` stamp is part of the signed record, so the
/// window can't be erased by metadata tampering. The front-end gates resolution
/// client-side using the wall clock (its `Clock` capability) against
/// `WITCHY_COOLDOWN_SECS`.
#[test]
fn fresh_releases_cool_down_before_resolving() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "cooldown");
    let app = fe.new_app();
    fe.published_lib("acme/fresh", "1.0.0", "fn f(s: String) -> String:\n    s\n");

    // A `pm add` with a real cooldown window in force (overriding the test-default
    // zero). The just-promoted version is younger than the window, so it is refused.
    let run_cooled = |args: &[&str]| {
        let mut full = vec!["pm"];
        full.extend_from_slice(args);
        Command::new(BIN)
            .current_dir(&app)
            .env("COVEN_URL", server.url())
            .env("WITCHY_COOLDOWN_SECS", "3600")
            .args(&full)
            .output()
            .expect("spawn witchy pm")
    };
    let out = run_cooled(&["add", "acme/fresh"]);
    assert!(!out.status.success(), "a release inside its cooldown must not resolve");
    let msg = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        msg.contains("staging cooldown") && msg.contains("--allow-fresh"),
        "the refusal should explain the window and the override: {msg}"
    );
    assert!(!app.join("witchy.lock").exists(), "a cooled-out add must write nothing");

    // …and `--allow-fresh` is the explicit acceptance.
    let out = run_cooled(&["add", "acme/fresh", "--allow-fresh"]);
    assert!(out.status.success(), "--allow-fresh should accept: {}", stderr(&out));
    assert!(app.join("vendor/fresh/src/fresh.witchy").exists(), "accepted add must vendor");
}

/// Build steps auto-run during `witchy build`/`run`: a path dependency's
/// `src/build.witchy` executes confined under its `[build.grants]`, the source
/// it emits joins the consumer's link (importable like any module) — and the
/// **post-generation audit** recomputes the rune's footprint over shipped +
/// generated code, refusing generated source that widens beyond the dependency's
/// shipped baseline. Generated code cannot smuggle in authority. Driven through
/// the front-end (`witchy pm run`) from the workspace root.
#[test]
fn build_steps_auto_run_and_generated_source_is_gated() {
    let work = unique("autorun");
    let lib = work.join("genlib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(
        lib.join("src/genlib.witchy"),
        "pub fn id(s: String) -> String:\n    s\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    out.write_out(\"greet.witchy\", \"pub fn greeting() -> String:\" + nl + \"    \\\"hi from generated code\\\"\" + nl)\n",
    )
    .unwrap();

    let app = work.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    // The path dep + an empty grants section (the consent to its BuildOut-only step).
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n\n[dependencies]\n\"genlib\" = { path = \"../genlib\" }\n\n[build.grants.\"genlib\"]\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/app.witchy"),
        "import greet\n\nfn main(console: Console):\n    console.print(greet.greeting())\n",
    )
    .unwrap();
    let out = pm_fe(&work, &["run", "app"]);
    assert!(out.status.success(), "auto-run build step + import failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("hi from generated code"), "got: {}", stdout(&out));

    // Now the build step turns malicious: it *generates* capability-hungry
    // source. The step itself still demands only BuildOut — but the
    // post-generation audit (footprint over shipped + generated) refuses the
    // smuggle of new runtime authority (Net) into the consumer's link.
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    out.write_out(\"greet.witchy\", \"pub fn evil(n: Net, addr: String) -> Socket:\" + nl + \"    n.connect(addr)\" + nl)\n",
    )
    .unwrap();
    // This is an intentional path-dependency edit, so refresh its content lock.
    // The refreshed shipped footprint still contains only BuildOut; the dynamic
    // generated Net demand is what the post-generation gate must catch.
    let relock = pm_fe(&work, &["lock", "app"]);
    assert!(relock.status.success(), "relock malicious build-step fixture: {}", stdout(&relock));
    let out = pm_fe(&work, &["run", "app"]);
    assert!(!out.status.success(), "generated widening must be refused");
    assert!(
        stdout(&out).contains("WIDENS its footprint"),
        "the refusal should explain the smuggle: {}",
        stdout(&out)
    );

    let _ = std::fs::remove_dir_all(&work);
}

/// A deterministic build step (no BuildExec/BuildNet) is cached by its inputs
/// (§7.2): a second build with unchanged inputs reuses the prior output instead
/// of re-running. We prove the *hit* by corrupting the cached output and leaving
/// the cache key intact — a cache hit serves the corrupted bytes, a miss would
/// regenerate the original. Driven through the front-end (`witchy pm run`).
#[test]
fn deterministic_build_output_is_cached() {
    let work = unique("buildcache");
    let lib = work.join("genlib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(lib.join("src/genlib.witchy"), "pub fn id(s: String) -> String:\n    s\n").unwrap();
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    out.write_out(\"greet.witchy\", \"pub fn greeting() -> String:\" + nl + \"    \\\"V1\\\"\" + nl)\n",
    )
    .unwrap();

    let app = work.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n\n[dependencies]\n\"genlib\" = { path = \"../genlib\" }\n\n[build.grants.\"genlib\"]\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/app.witchy"),
        "import greet\n\nfn main(console: Console):\n    console.print(greet.greeting())\n",
    )
    .unwrap();

    let out = pm_fe(&work, &["run", "app"]);
    assert!(out.status.success(), "first run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("V1"), "got: {}", stdout(&out));

    // Corrupt the generated output, keep the cache key.
    let gen_file = app.join("build-out/genlib/greet.witchy");
    let body = std::fs::read_to_string(&gen_file).unwrap().replace("V1", "CACHED");
    std::fs::write(&gen_file, body).unwrap();

    let out = pm_fe(&work, &["run", "app"]);
    assert!(out.status.success(), "cached run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(
        stdout(&out).contains("CACHED"),
        "a deterministic build step should be cached (got: {})",
        stdout(&out)
    );

    let _ = std::fs::remove_dir_all(&work);
}

/// The committed `examples/projects/todo` workspace — a `todo` app that depends
/// on a sibling `tasklib` library via a path dependency and reads its checklist
/// with a read-only `Dir` capability — builds and runs end to end. Copied into a
/// hermetic sandbox so the test never mutates the repo (or its lockfile).
#[test]
fn example_todo_workspace_runs_with_a_path_dependency() {
    let work = lift_example("todo");
    let out = pm_fe(&work, &["run", "todo"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("[x] Decompose Dir into Read / Write"), "rendered board missing: {s}");
    assert!(s.contains("[ ] Implement a real UDP transport"), "pending item missing: {s}");
    assert!(s.contains("3 / 5 done"), "summary missing: {s}");
}

/// The committed `examples/projects/ledger` workspace — a bank-account async task
/// (balance isolated in a recursive parameter, FIFO messages over a channel) that
/// formats amounts via a `money` library rune (a path dependency) — builds and
/// runs end to end. Copied into a hermetic sandbox so the repo is never touched.
#[test]
fn example_ledger_workspace_runs_with_async_and_a_path_dependency() {
    let work = lift_example("ledger");
    let out = pm_fe(&work, &["run", "ledger"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let s = stdout(&out);
    // FIFO message order + running balance, formatted by the `money` rune.
    assert!(s.contains("deposit  $12.50  -> balance $12.50"), "first deposit missing: {s}");
    assert!(s.contains("withdraw $5.00  -> balance $11.25"), "withdrawal missing: {s}");
    assert!(s.trim().ends_with("deposit  $0.99  -> balance $12.24"), "final balance wrong: {s}");
}

/// The committed `examples/projects/report` workspace — a `report` app that
/// decodes a JSON file (via a read-only `Dir`) with the std `json` module and
/// computes summary statistics with a `stats` library rune (a path dependency) —
/// builds and runs end to end. Copied into a hermetic sandbox.
#[test]
fn example_report_workspace_runs_with_json_and_a_path_dependency() {
    let work = lift_example("report");
    let out = pm_fe(&work, &["run", "report"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("records: 4"), "record count missing: {s}");
    assert!(s.contains("total:   200"), "total missing: {s}");
    assert!(s.contains("max:     91"), "max missing: {s}");
    assert!(s.contains("average: 50"), "average missing: {s}");
}

/// The committed `examples/projects/dashboard` workspace — a `dashboard` app
/// depending on two widget runes (`tasks`, `coverage`) that both depend on a
/// shared `bars` base, forming a *diamond*. It builds and runs, and `witchy tree`
/// shows the shared base resolved once. Copied into a hermetic sandbox.
#[test]
fn example_dashboard_workspace_runs_with_a_diamond_dependency() {
    let work = lift_example("dashboard");

    // The diamond — `dashboard` → {`tasks`, `coverage`} → shared `bars` base —
    // builds and runs. The front-end collects the path-dependency graph
    // TRANSITIVELY and deduplicates the shared base, so `bars` is linked exactly
    // once: a successful run with both widgets rendered is the proof (a duplicate
    // `bars` module would be a link-time redefinition error).
    let out = pm_fe(&work, &["run", "dashboard"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("tasks    [####----]  50%"), "tasks widget missing: {s}");
    assert!(s.contains("coverage [######--]  75%"), "coverage widget missing: {s}");

    // `witchy pm tree` lists the app and its direct path dependencies.
    let tree = pm_fe(&work, &["tree", "dashboard"]);
    assert!(tree.status.success(), "tree failed: {}", stderr(&tree));
    let t = stdout(&tree);
    assert!(t.contains("dashboard"), "tree should name the rune: {t}");
    assert!(t.contains("tasks") && t.contains("coverage"), "tree should list the widgets: {t}");
}

/// The committed `examples/projects/config` workspace — a `greet` app that reads
/// a "key = value" file (via a read-only `Dir`), parses it with the `kv` library
/// rune (a path dependency), and composes a greeting with `Result`/`?` error
/// handling. Runs the happy path; a `?`-propagated missing key is covered by the
/// project's own design (and exercised manually).
#[test]
fn example_config_workspace_runs_with_result_error_handling() {
    let work = lift_example("config");

    let out = pm_fe(&work, &["run", "greet"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(stdout(&out).trim() == "Hello, witchy!", "greeting wrong: {}", stdout(&out));

    // Drop a required key: `?` short-circuits to the friendly Err message. The
    // data file lives in the app subdir (the program's runtime Dir is rooted there).
    std::fs::write(work.join("greet").join("config.kv"), "greeting = Hi\n").unwrap();
    let out = pm_fe(&work, &["run", "greet"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(
        stdout(&out).contains("config error: missing config key: name"),
        "error path wrong: {}",
        stdout(&out)
    );
}

/// The committed `examples/projects/wordfreq` workspace — a `wordfreq` app that
/// reads a text file (via a read-only `Dir`) and ranks the most common words with
/// the `wordlib` rune (a path dependency using std `string`/`ascii`/`dict`/
/// `list`). It builds and runs, normalizing case + punctuation and breaking
/// count ties alphabetically for a deterministic top-5. Whitespace is collapsed
/// so the assertion checks content and order, not the column padding.
#[test]
fn example_wordfreq_workspace_ranks_words() {
    let work = lift_example("wordfreq");
    let out = pm_fe(&work, &["run", "wordfreq"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let collapsed = stdout(&out).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains("Top words: the 6 fox 4 dog 3 quick 3 brown 2"),
        "ranking wrong: {collapsed}"
    );
}

/// A published rune must be self-contained (registry-only): a manifest carrying a
/// path dependency would reach across the publisher's filesystem to bytes the
/// registry never content-addresses, so the front-end `pm publish` refuses it
/// before anything is uploaded.
#[test]
fn published_rune_cannot_have_path_dependency() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "nopath");
    // A rune that tries to reach into a sibling path on the publisher's filesystem.
    let dir = fe.base.join("sneaky");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/sneaky\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"x\" = { path = \"../x\" }\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/sneaky.witchy"), "pub fn f(s: String) -> String:\n    s\n").unwrap();

    let ci = server.ci_token("acme-sneaky-repo", "release.yml");
    let out = fe.pm(&dir, &["publish", "."], Some(&ci));
    assert!(!out.status.success(), "a published rune with a path dep must be refused");
    assert!(stdout(&out).contains("path"), "stdout {} stderr {}", stdout(&out), stderr(&out));
}

/// The registry recomputes a rune's footprint server-side and refuses an
/// under-declared publish (400): a library that demands `Net` but declares only
/// `Console` is rejected — the under-declaration a supply-chain attacker relies
/// on. The front-end `pm publish` surfaces the server's refusal.
#[test]
fn publish_rejects_underdeclared_capabilities() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "declared");
    let dir = fe.base.join("lib");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    // A rune that demands Net but declares only Console.
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/lib\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/lib.witchy"),
        "pub fn fetch(net: Net, u: String) -> String:\n    u\n",
    )
    .unwrap();
    let ci = server.ci_token("acme-repo", "release.yml");
    let out = fe.pm(&dir, &["publish", "."], Some(&ci));
    assert!(!out.status.success(), "under-declared caps must be refused at publish");
    assert!(stdout(&out).contains("publish: 400"), "expected 400: {}", stdout(&out));
}

/// The **self-hosted** registry path: spawn the witchy coven server
/// (`projects/coven/src/coven.witchy`, interpreter-hosted — it uses
/// `compiler.footprint`) and drive it with the witchy coven client over real
/// HTTP. This exercises the whole registry in witchy: two-phase publish,
/// separation of duties (a self-promote is refused 403), server-side footprint
/// recomputation (an under-declared rune is refused 400), and source fetch.
#[test]
fn witchy_coven_full_lifecycle_self_hosted() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let coven_src = format!("{manifest_dir}/projects/coven/src/coven.witchy");
    let client_src = format!("{manifest_dir}/projects/coven/src/coven_client.witchy");

    // Pre-pick a free port (the server must bind the same addr we pass to --net).
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");

    let store = unique("witchy-coven-store");
    let seed = store.join("root.seed");
    std::fs::write(
        &seed,
        "0000000000000000000000000000000000000000000000000000000000000001",
    )
    .unwrap();

    let mut server = Command::new(BIN)
        .args([
            "--net",
            &addr,
            "--signing-key",
            seed.to_str().unwrap(),
            &coven_src,
            &addr,
        ])
        .current_dir(&store)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn witchy coven server");

    // Wait for the listener to come up.
    let mut up = false;
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(&addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }
    if !up {
        let _ = server.kill();
        let _ = server.wait();
        let _ = std::fs::remove_dir_all(&store);
        panic!("witchy coven server never started listening on {addr}");
    }

    // Drive the lifecycle with the self-hosted client.
    let out = Command::new(BIN)
        .args(["--net", &addr, &client_src, &addr])
        .output()
        .expect("run witchy coven client");

    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&store);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        out.status.success(),
        "client failed: status={:?} stdout={stdout:?} stderr={:?}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(lines.contains(&"rootpub:200"), "rootpub: {lines:?}");
    assert!(lines.contains(&"publish:200 state=staged"), "publish: {lines:?}");
    assert!(lines.contains(&"record:200 state=staged"), "record: {lines:?}");
    assert!(
        lines.contains(&"promote:200 state=released sod=true"),
        "promote w/ separation of duties: {lines:?}"
    );
    assert!(
        lines.contains(&"selfpromote:403"),
        "a self-promote must be refused 403: {lines:?}"
    );
    assert!(
        lines.contains(&"underdeclared:400"),
        "an under-declared rune must be refused 400: {lines:?}"
    );
    assert!(lines.contains(&"source:200"), "source: {lines:?}");
    assert!(
        lines.iter().any(|l| l.starts_with("index:200") && l.contains("acme/money")),
        "index: {lines:?}"
    );
    // TUF roles: the snapshot + timestamp are served and their signatures verify
    // against the registry root key (rollback + freeze protection, self-hosted).
    assert!(
        lines.contains(&"snapshot:200 verified=true"),
        "TUF snapshot must verify: {lines:?}"
    );
    assert!(
        lines.contains(&"timestamp:200 verified=true fresh=true"),
        "TUF timestamp must verify AND be fresh (freeze protection): {lines:?}"
    );
}

/// Trusted publishing against the **witchy** coven: a token minted by the Rust
/// `coven-mint-token` verifies in the self-hosted registry (proving the witchy
/// canonical-claims reconstruction is byte-identical to serde). The verified
/// `sub` becomes the recorded uploader — a client-asserted identity is ignored —
/// and a publish with no token is refused 401.
#[test]
fn witchy_coven_trusted_publishing_verifies_a_rust_minted_token() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let coven_src = format!("{manifest_dir}/projects/coven/src/coven.witchy");

    // Mint a trusted issuer + a publish token (the Rust IdP CLI).
    let idp = unique("witchy-coven-idp");
    let gen_out = Command::new(BIN)
        .args(["coven-gen-issuer", "--out", idp.to_str().unwrap()])
        .output()
        .expect("gen issuer");
    let pubhex = String::from_utf8_lossy(&gen_out.stdout)
        .lines()
        .next()
        .unwrap()
        .trim()
        .to_string();
    let sub = "repo:acme/money:ref:refs/heads/main";
    let mint = Command::new(BIN)
        .args([
            "coven-mint-token",
            "--issuer-key",
            idp.to_str().unwrap(),
            "--issuer",
            "gha",
            "--sub",
            sub,
            "--claim",
            "repository=acme/money",
            "--claim",
            "workflow_ref=rel.yml",
        ])
        .output()
        .expect("mint token");
    let token = String::from_utf8_lossy(&mint.stdout).trim().to_string();

    // A token from the SAME trusted issuer but a DIFFERENT repository — used to
    // check the namespace squat defense (the bound policy must reject it).
    let evil = Command::new(BIN)
        .args([
            "coven-mint-token",
            "--issuer-key",
            idp.to_str().unwrap(),
            "--issuer",
            "gha",
            "--sub",
            "repo:evil/x:ref:refs/heads/main",
            "--claim",
            "repository=evil/x",
            "--claim",
            "workflow_ref=rel.yml",
        ])
        .output()
        .expect("mint evil token");
    let evil_token = String::from_utf8_lossy(&evil.stdout).trim().to_string();

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let store = unique("witchy-coven-trusted-store");
    let seed = store.join("root.seed");
    std::fs::write(
        &seed,
        "0000000000000000000000000000000000000000000000000000000000000001",
    )
    .unwrap();

    let mut server = Command::new(BIN)
        .args([
            "--net",
            &addr,
            "--signing-key",
            seed.to_str().unwrap(),
            &coven_src,
            &addr,
            &format!("gha={pubhex}"),
        ])
        .current_dir(&store)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn witchy coven (trusted)");

    let mut up = false;
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(&addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }
    if !up {
        let _ = server.kill();
        let _ = server.wait();
        panic!("witchy coven (trusted) never started on {addr}");
    }

    let manifest = "[rune]\nname = \"acme/money\"\nversion = \"1.0.0\"\n";
    let module = "fn dollars(n: Int) -> Int:\n    n * 100\n";
    let source = format!(
        "{{\"files\":[[\"witchy.toml\",{}],[\"src/money.witchy\",{}]]}}",
        json_str(manifest),
        json_str(module)
    );
    // Publish WITH the trusted token -> 200, uploader derived from claims.sub.
    let with_token = format!(
        "{{\"manifest_toml\":{},\"source\":{source},\"id_token\":{}}}",
        json_str(manifest),
        json_str(&token)
    );
    let (status, body) = http_post(&addr, "/coven/publish", &with_token);
    // Publish a second version WITHOUT a token but asserting an uploader -> 401.
    let manifest2 = "[rune]\nname = \"acme/money\"\nversion = \"2.0.0\"\n";
    let source2 = format!(
        "{{\"files\":[[\"witchy.toml\",{}],[\"src/money.witchy\",{}]]}}",
        json_str(manifest2),
        json_str(module)
    );
    let without = format!(
        "{{\"manifest_toml\":{},\"source\":{source2},\"uploaded_by\":\"sneaky\"}}",
        json_str(manifest2)
    );
    let (status_notoken, _) = http_post(&addr, "/coven/publish", &without);

    // Namespace squat: a token from a different repository (same issuer) tries to
    // publish into the bound `acme` namespace -> the policy must refuse it (403).
    let manifest3 = "[rune]\nname = \"acme/money\"\nversion = \"3.0.0\"\n";
    let source3 = format!(
        "{{\"files\":[[\"witchy.toml\",{}],[\"src/money.witchy\",{}]]}}",
        json_str(manifest3),
        json_str(module)
    );
    let squat = format!(
        "{{\"manifest_toml\":{},\"source\":{source3},\"id_token\":{}}}",
        json_str(manifest3),
        json_str(&evil_token)
    );
    let (status_squat, squat_body) = http_post(&addr, "/coven/publish", &squat);

    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&store);
    let _ = std::fs::remove_dir_all(&idp);

    assert_eq!(status, 200, "trusted publish should succeed: {body}");
    assert!(
        body.contains(&format!("\"uploaded_by\":\"{sub}\"")),
        "uploader must come from the verified token sub, got: {body}"
    );
    assert_eq!(
        status_notoken, 401,
        "a publish without a token to a trusted registry must be refused"
    );
    assert_eq!(
        status_squat, 403,
        "a token from a different repository must be refused by the bound namespace policy: {squat_body}"
    );
}


/// GET a path and return the WHOLE raw HTTP response (status line + headers + body), so
/// a test can read a redirect's `Location` header.
/// The `Rand` capability: `rand.rand_u64()` draws from the OS CSPRNG, but under
/// `WITCHY_RAND_SEED` both backends draw the SAME deterministic splitmix sequence — so a
/// program using randomness stays parity-stable and reproducible for tests.
#[test]
fn rand_capability_seeds_deterministically_and_agrees_across_backends() {
    let dir = unique("witchy-rand");
    std::fs::create_dir_all(&dir).unwrap();
    // Bundled std module names have one canonical owner; this test exercises the
    // Rand capability, so its entry module must not also claim the `rand` name.
    let src = dir.join("main.witchy");
    std::fs::write(
        &src,
        "fn main(console: Console, rand: Rand):\n    console.print(\"${rand.rand_u64()}\")\n    console.print(\"${rand.rand_u64()}\")\n",
    )
    .unwrap();
    let path = src.to_str().unwrap();

    // Seeded: the interpreter and the compiled backend draw the same sequence → agree.
    let parity = Command::new(BIN).args(["parity", path]).env("WITCHY_RAND_SEED", "42").output().unwrap();
    assert!(
        parity.status.success() && stdout(&parity).contains("agree"),
        "seeded rand must be parity-stable: {}\n{}",
        stdout(&parity),
        stderr(&parity)
    );

    let run = |seed: &str| {
        let o = Command::new(BIN).args(["run", path]).env("WITCHY_RAND_SEED", seed).output().unwrap();
        assert!(o.status.success(), "run failed: {}", stderr(&o));
        stdout(&o)
    };
    assert_eq!(run("42"), run("42"), "the same seed must reproduce the sequence");
    assert_ne!(run("42"), run("7"), "a different seed must produce a different sequence");
}

fn http_get_raw(addr: &str, path: &str) -> String {
    http_get_raw_cookie(addr, path, "")
}

/// Like [`http_get_raw`] but also sends a `Cookie:` header — the test's cookie jar for the
/// OAuth nonce that binds `/login` to `/callback` (SEC-007).
fn http_get_raw_cookie(addr: &str, path: &str, cookie: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(8))).unwrap();
    let cookie_hdr = if cookie.is_empty() { String::new() } else { format!("Cookie: {cookie}\r\n") };
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n{cookie_hdr}Connection: close\r\n\r\n").as_bytes())
        .unwrap();
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    buf
}

/// The `name=value` of a Set-Cookie header (dropping the attributes after the first `;`).
fn cookie_pair(set_cookie: &str) -> String {
    set_cookie.split(';').next().unwrap_or("").trim().to_string()
}

/// The value of a response header (case-insensitive), or None.
fn header_value(response: &str, name: &str) -> Option<String> {
    for line in response.lines() {
        if line.trim().is_empty() {
            break; // end of headers
        }
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case(name)
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Pull a query parameter's value out of a URL.
fn query_param(url: &str, key: &str) -> Option<String> {
    let q = url.split_once('?')?.1;
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && k == key
        {
            return Some(v.to_string());
        }
    }
    None
}

/// Query pairs reach a handler decoded. The coven-web proxy must encode them again
/// before making the upstream request, or escaped separators become new parameters.
#[test]
fn coven_web_proxy_reencodes_decoded_query_values() {
    use std::io::{Read, Write};

    let upstream_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_addr = upstream_listener.local_addr().unwrap().to_string();
    let (request_tx, request_rx) = std::sync::mpsc::channel();
    let upstream = std::thread::spawn(move || {
        let (mut stream, _) = upstream_listener.accept().unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(8))).unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read_exact(&mut byte).is_ok() {
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&head);
        request_tx.send(request.lines().next().unwrap_or("").to_string()).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .unwrap();
    });

    let seed_root = unique("cw-proxy-seed");
    let seed = seed_root.join("root.seed");
    std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
    let web_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let web_addr = format!("127.0.0.1:{web_port}");
    let assets = unique("cw-proxy-assets");
    let cw_src = format!("{}/projects/coven-web/src/coven_web.witchy", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new(BIN)
        .args([
            "--net",
            &web_addr,
            "--net",
            &upstream_addr,
            "--signing-key",
            seed.to_str().unwrap(),
            &cw_src,
            &web_addr,
            &upstream_addr,
            &format!("http://{web_addr}"),
            "localhost",
        ])
        .current_dir(&assets)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coven-web");

    let mut up = false;
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(&web_addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }
    assert!(up, "coven-web never started on {web_addr}");

    let (status, body) = http_get(&web_addr, "/api/coven/versions?name=acme%26state%3Dyanked");
    let request_line = request_rx.recv_timeout(std::time::Duration::from_secs(8)).unwrap();

    let _ = child.kill();
    let _ = child.wait();
    upstream.join().unwrap();
    let _ = std::fs::remove_dir_all(&seed_root);
    let _ = std::fs::remove_dir_all(&assets);

    assert_eq!(status, 200, "proxy response: {body}");
    assert_eq!(
        request_line,
        "GET /coven/versions?name=acme%26state%3Dyanked HTTP/1.1",
        "encoded query separators must remain value data"
    );
}

/// "Log in with GitHub" end to end through the REAL coven-web server: a mock GitHub (a
/// local rustls server) returns a token then a user; coven-web's OAuth `/callback`
/// verifies the signed state, exchanges the code, reads the user, and mints a bearer
/// session. Proves the whole social-login flow — TLS, HTTPS, the OAuth dance, and
/// session minting — composes on the deployed server.
#[test]
fn coven_web_github_login_completes_a_session() {
    use std::io::{Read, Write};
    use std::sync::Arc;
    // Mock-GitHub TLS server with a self-signed `localhost` cert coven-web will trust.
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
    let cert_der = ck.cert.der().clone();
    let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
    let tls_config = Arc::new(
        rustls::ServerConfig::builder_with_provider(rustls::crypto::aws_lc_rs::default_provider().into())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
            .unwrap(),
    );
    let gh_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let gh_port = gh_listener.local_addr().unwrap().port();
    let cert_path = unique("witchy-cw-gh-cert").join("cert.pem");
    std::fs::write(&cert_path, ck.cert.pem()).unwrap();

    let sc = tls_config.clone();
    let gh = std::thread::spawn(move || {
        for _ in 0..2 {
            let (tcp, _) = gh_listener.accept().unwrap();
            let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while tls.read_exact(&mut b).is_ok() {
                head.push(b[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request_line = String::from_utf8_lossy(&head);
            let body: &[u8] = if request_line.starts_with("POST /login/oauth/access_token") {
                b"{\"access_token\":\"gho_e2e\",\"token_type\":\"bearer\"}"
            } else {
                b"{\"login\":\"octocat\",\"id\":583231}"
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                std::str::from_utf8(body).unwrap()
            );
            let _ = tls.write_all(resp.as_bytes());
            let _ = tls.flush();
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
    });

    // Spawn coven-web pointed at the mock for both GitHub base URLs.
    let seed = unique("cw-seed").join("root.seed");
    std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
    let web_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let web_addr = format!("127.0.0.1:{web_port}");
    let mock = format!("https://localhost:{gh_port}");
    let assets = unique("cw-assets");
    std::fs::create_dir_all(&assets).unwrap();
    let cw_src = format!("{}/projects/coven-web/src/coven_web.witchy", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new(BIN)
        .args([
            "--net",
            &web_addr,
            "--net",
            &format!("localhost:{gh_port}"),
            "--signing-key",
            seed.to_str().unwrap(),
            "--secret",
            "github_client_secret=e2esecret",
            &cw_src,
            &web_addr,
            "127.0.0.1:1",
            &format!("http://{web_addr}"),
            "localhost",
            "Ov23liE2E",
            &mock,
            &mock,
        ])
        .current_dir(&assets)
        .env("WITCHY_TLS_EXTRA_ROOTS", &cert_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coven-web");

    let mut up = false;
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(&web_addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }
    assert!(up, "coven-web never started on {web_addr}");

    // /login → 303 to the mock authorize URL; capture the signed anti-CSRF state AND the
    // per-login nonce cookie that binds this flow to the browser (SEC-007).
    let login = http_get_raw(&web_addr, "/auth/github/login");
    let location = header_value(&login, "location").expect("login redirect has a Location");
    assert!(
        location.starts_with(&format!("{mock}/login/oauth/authorize")),
        "login must redirect to the GitHub authorize URL: {location}"
    );
    let state = query_param(&location, "state").expect("authorize URL carries state");
    let cookie = cookie_pair(&header_value(&login, "set-cookie").expect("login sets an oauth_nonce cookie"));

    // SEC-007: the callback WITHOUT the matching nonce cookie must be refused (a replayed
    // state from another session can't complete the login). A refusal is a 403, not a redirect.
    let no_cookie = http_get_raw(&web_addr, &format!("/auth/github/callback?code=thecode&state={state}"));
    assert!(
        header_value(&no_cookie, "location").is_none(),
        "callback without the nonce cookie must be refused, got: {no_cookie}"
    );

    // /callback WITH the cookie → coven-web exchanges the code, reads the user, mints a
    // session, and redirects to the SPA with the bearer in the fragment.
    let callback = http_get_raw_cookie(&web_addr, &format!("/auth/github/callback?code=thecode&state={state}"), &cookie);
    let cb_loc = header_value(&callback, "location").expect("callback redirects with a session");

    let _ = child.kill();
    let _ = child.wait();
    let _ = gh.join();
    let _ = std::fs::remove_file(&cert_path);
    let _ = std::fs::remove_dir_all(&assets);

    assert!(cb_loc.contains("login=octocat"), "session redirect must name the signed-in user: {cb_loc}");
    assert!(cb_loc.contains("token="), "session redirect must carry a bearer token: {cb_loc}");
}

/// base64url, no padding — JWT segment encoding.
fn b64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = ((c[0] as u32) << 16)
            | ((*c.get(1).unwrap_or(&0) as u32) << 8)
            | (*c.get(2).unwrap_or(&0) as u32);
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        if c.len() > 1 {
            out.push(A[(n >> 6 & 63) as usize] as char);
        }
        if c.len() > 2 {
            out.push(A[(n & 63) as usize] as char);
        }
    }
    out
}

/// The two INTEGER contents of a DER `SEQUENCE { INTEGER, INTEGER }` (an RSA public key).
fn two_ints(der: &[u8]) -> (Vec<u8>, Vec<u8>) {
    fn len_at(b: &[u8], i: &mut usize) -> usize {
        let mut len = b[*i] as usize;
        *i += 1;
        if len & 0x80 != 0 {
            let nbytes = len & 0x7f;
            len = 0;
            for _ in 0..nbytes {
                len = (len << 8) | b[*i] as usize;
                *i += 1;
            }
        }
        len
    }
    fn tlv(b: &[u8], i: &mut usize) -> Vec<u8> {
        *i += 1;
        let len = len_at(b, i);
        let v = b[*i..*i + len].to_vec();
        *i += len;
        v
    }
    let mut i = 0;
    i += 1;
    let _ = len_at(der, &mut i);
    (tlv(der, &mut i), tlv(der, &mut i))
}

/// "Log in with Google" (OIDC) end to end through the REAL coven-web server: a mock
/// Google (local rustls) returns a TEST-SIGNED `id_token` then serves its JWKS; coven-web's
/// callback verifies the id_token's signature against the JWKS (and issuer/audience) and
/// mints a session. Proves the OIDC login path — the id_token verification wired through
/// the deployed server, distinct from GitHub's userinfo fetch.
#[test]
fn coven_web_google_login_verifies_id_token_and_completes_a_session() {
    use std::io::{Read, Write};
    use std::sync::Arc;
    // A signing key for the id_token; its public n/e go in the mock JWKS.
    use aws_lc_rs::signature::KeyPair;
    let idk = aws_lc_rs::rsa::KeyPair::generate(aws_lc_rs::rsa::KeySize::Rsa2048).expect("idk");
    let (n_int, e_int) = two_ints(idk.public_key().as_ref());
    let strip = |v: &[u8]| if v.first() == Some(&0) { v[1..].to_vec() } else { v.to_vec() };
    let jwks = format!(
        r#"{{"keys":[{{"kty":"RSA","kid":"g1","n":"{}","e":"{}"}}]}}"#,
        b64url(&strip(&n_int)),
        b64url(&strip(&e_int))
    );
    // The id_token is signed per-login so it can echo the OIDC `nonce` coven-web sends
    // (SEC-008); the mock serves whatever the test puts in `token_slot` after capturing it.
    let header_b64 = b64url(br#"{"alg":"RS256","kid":"g1","typ":"JWT"}"#);
    let sign_id_token = |nonce: &str| -> String {
        let iat = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_secs();
        let exp = iat + 3600;
        let payload = format!(
            r#"{{"iss":"https://accounts.google.com","aud":"gClientID","email":"alice@example.com","email_verified":true,"sub":"1","iat":{iat},"exp":{exp},"nbf":0,"nonce":"{nonce}"}}"#
        );
        let signed = format!("{header_b64}.{}", b64url(payload.as_bytes()));
        let mut sig = vec![0u8; idk.public_modulus_len()];
        idk.sign(
            &aws_lc_rs::signature::RSA_PKCS1_SHA256,
            &aws_lc_rs::rand::SystemRandom::new(),
            signed.as_bytes(),
            &mut sig,
        )
        .expect("sign id_token");
        format!(r#"{{"id_token":"{signed}.{}","token_type":"bearer"}}"#, b64url(&sig))
    };
    let token_slot = Arc::new(std::sync::Mutex::new(String::new()));

    // Mock-Google TLS server: POST /token -> the id_token; GET /certs -> the JWKS.
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).expect("cert");
    let cert_der = ck.cert.der().clone();
    let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(ck.key_pair.serialize_der());
    let tls_config = Arc::new(
        rustls::ServerConfig::builder_with_provider(rustls::crypto::aws_lc_rs::default_provider().into())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], rustls::pki_types::PrivateKeyDer::Pkcs8(key_der))
            .unwrap(),
    );
    let g_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let g_port = g_listener.local_addr().unwrap().port();
    let cert_path = unique("witchy-cw-google-cert").join("cert.pem");
    std::fs::write(&cert_path, ck.cert.pem()).unwrap();

    let sc = tls_config.clone();
    let token_slot_mock = Arc::clone(&token_slot);
    let g = std::thread::spawn(move || {
        for _ in 0..2 {
            let (tcp, _) = g_listener.accept().unwrap();
            let conn = rustls::ServerConnection::new(sc.clone()).unwrap();
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while tls.read_exact(&mut b).is_ok() {
                head.push(b[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let body = if String::from_utf8_lossy(&head).starts_with("POST /token") {
                token_slot_mock.lock().unwrap().clone()
            } else {
                jwks.clone()
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = tls.write_all(resp.as_bytes());
            let _ = tls.flush();
            tls.conn.send_close_notify();
            let _ = tls.flush();
        }
    });

    let seed = unique("cw-g-seed").join("root.seed");
    std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
    let web_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let web_addr = format!("127.0.0.1:{web_port}");
    let mock = format!("https://localhost:{g_port}");
    let assets = unique("cw-g-assets");
    std::fs::create_dir_all(&assets).unwrap();
    let cw_src = format!("{}/projects/coven-web/src/coven_web.witchy", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new(BIN)
        .args([
            "--net",
            &web_addr,
            "--net",
            &format!("localhost:{g_port}"),
            "--signing-key",
            seed.to_str().unwrap(),
            "--secret",
            "google_client_secret=gsecret",
            &cw_src,
            &web_addr,
            "127.0.0.1:1",
            &format!("http://{web_addr}"),
            "localhost",
            "", // github disabled
            "https://github.com",
            "https://api.github.com",
            "gClientID",
            &format!("{mock}/authorize"),
            &format!("{mock}/token"),
            &format!("{mock}/certs"),
        ])
        .current_dir(&assets)
        .env("WITCHY_TLS_EXTRA_ROOTS", &cert_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn coven-web");

    let mut up = false;
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(&web_addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }
    assert!(up, "coven-web never started on {web_addr}");

    let login = http_get_raw(&web_addr, "/auth/google/login");
    let location = header_value(&login, "location").expect("login redirect has a Location");
    let state = query_param(&location, "state").expect("authorize URL carries state");
    let cookie = cookie_pair(&header_value(&login, "set-cookie").expect("login sets an oauth_nonce cookie"));
    // SEC-008: the authorize URL carries the OIDC nonce; the mock's id_token must echo it.
    let nonce = query_param(&location, "nonce").expect("the Google authorize URL carries the OIDC nonce");
    *token_slot.lock().unwrap() = sign_id_token(&nonce);

    let callback = http_get_raw_cookie(&web_addr, &format!("/auth/google/callback?code=thecode&state={state}"), &cookie);
    let cb_loc = header_value(&callback, "location")
        .unwrap_or_else(|| panic!("callback did not redirect; raw response:\n{callback}"));

    let _ = child.kill();
    let _ = child.wait();
    let _ = g.join();
    let _ = std::fs::remove_file(&cert_path);
    let _ = std::fs::remove_dir_all(&assets);

    assert!(
        cb_loc.contains("login=alice%40example.com"),
        "session redirect must name the verified email: {cb_loc}"
    );
    assert!(cb_loc.contains("token="), "session redirect must carry a bearer token: {cb_loc}");
}

/// JSON-encode a string (quoted, with `"`, `\`, and newlines escaped).
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The full self-hosted resolution path: the witchy pm `add` resolves a version
/// requirement against the witchy coven (`std/semver` over `/coven/versions`),
/// fetches the chosen version's source, and materializes it. Only *released*
/// versions resolve — a staged version is invisible to `add`.
#[test]
fn witchy_pm_add_resolves_and_fetches_from_coven() {
    package_manager::witchy_pm_add_resolves_and_fetches_from_coven();
}

/// Self-hosted yank: the witchy coven marks a version yanked and the witchy pm's
/// resolver skips it. With 1.0.0 and 2.0.0 both released, `*` resolves to 2.0.0;
/// after 2.0.0 is yanked, `*` falls back to 1.0.0 — a yanked version is excluded
/// from new resolutions (existing locks would still pin it). Once EVERY version
/// is yanked, `add` refuses outright (no non-yanked released version remains),
/// and `pm list` reflects the yanked lifecycle state.
#[test]
fn witchy_coven_yank_excludes_from_resolution() {
    package_manager::witchy_coven_yank_excludes_from_resolution();
}

/// Transitive resolution, self-hosted: publishing `acme/app` whose manifest
/// declares a version dependency on `acme/util`, then `pm add acme/app` fetches
/// BOTH — app and, by following app's `[dependencies]`, util. Each node is
/// integrity-verified and carries its coven.json.
#[test]
fn witchy_pm_add_resolves_transitive_dependencies() {
    package_manager::witchy_pm_add_resolves_transitive_dependencies();
}

/// BUG-381: `std/rights` must interpret Net's verb and transport axes the same
/// way the compiler renders footprints. A manifest that declares all Connect
/// transports (`Net[Connect]`) covers source whose concrete footprint is the TCP
/// subset (`Net[Connect, Tcp]`).
#[test]
fn witchy_pm_check_accepts_net_axis_omission() {
    package_manager::witchy_pm_check_accepts_net_axis_omission();
}

/// BUG-568: `compiler.footprint` reports invalid source as an error document.
/// PM trust decisions must preserve that error instead of projecting it to an
/// empty capability set and minting a successful check or lock.
#[test]
fn witchy_pm_rejects_uninspectable_source_footprints() {
    package_manager::witchy_pm_rejects_uninspectable_source_footprints();
}

/// The witchy pm's LOCAL lifecycle, end to end and offline: scaffold two runes
/// (`new`), wire a path dependency, pin it (`lock`), confirm the pin (`verify`)
/// and the authority baseline (`gate`) — then tamper with the dependency so it
/// also demands `Net`, and watch `verify` flag the changed bytes (exit 2),
/// `gate` block the widened authority (exit 2, naming Net and its contributor),
/// and an explicit `gate <dir> Net` consent admit it (exit 0). This is the
/// self-hosted twin of the Rust PM's lock/verify/gate e2e coverage.
#[test]
fn witchy_pm_local_lifecycle_new_lock_verify_gate() {
    package_manager::witchy_pm_local_lifecycle_new_lock_verify_gate();
}

/// Registry guardrails in the **witchy** coven, over real HTTP: promote and
/// yank of a never-published version are 404s; re-publishing a released
/// version is refused 409 (immutability); and promote reports the rights-
/// precise `delta_runtime` against the latest released version — a first
/// release's delta is its whole footprint, an upgrade's delta names only the
/// NEW authority.
#[test]
fn witchy_coven_promote_delta_immutability_and_error_paths() {
    package_manager::witchy_coven_promote_delta_immutability_and_error_paths();
}

/// The embedded witchy package-manager front-end, invoked as `witchy pm <cmd>`
/// (RFC-0004 §5 bootstrap): the front-end `projects/pm/src/pm.witchy` is bundled
/// into the toolchain like std and run capability-confined. This is the first
/// slice of the e2e migration off the Rust CLI — as the front-end ports more of
/// `src/pm`'s behavior, this coverage grows until it can replace the Rust-CLI
/// lifecycle tests and `src/pm` can be removed. Read-only (no temp state needed).
#[test]
fn witchy_pm_embedded_frontend() {
    // audit: compute the capability footprint a source file demands.
    let audit = Command::new(BIN)
        .args(["pm", "audit", "examples/data/sample_rune.witchy"])
        .output()
        .expect("run `witchy pm audit`");
    assert!(audit.status.success(), "pm audit failed: {}", String::from_utf8_lossy(&audit.stderr));
    assert!(
        String::from_utf8_lossy(&audit.stdout).contains("demands: Dir[Read], Net[Connect]"),
        "pm audit output: {}",
        String::from_utf8_lossy(&audit.stdout)
    );

    // check: the pm rune's declared footprint admits its own code.
    let check = Command::new(BIN)
        .args(["pm", "check", "projects/pm"])
        .output()
        .expect("run `witchy pm check`");
    assert!(check.status.success(), "pm check failed: {}", String::from_utf8_lossy(&check.stderr));
    assert!(
        String::from_utf8_lossy(&check.stdout).contains("OK"),
        "pm check output: {}",
        String::from_utf8_lossy(&check.stdout)
    );

    // tree: the rune and its (zero) dependencies.
    let tree = Command::new(BIN)
        .args(["pm", "tree", "projects/pm"])
        .output()
        .expect("run `witchy pm tree`");
    assert!(tree.status.success(), "pm tree failed: {}", String::from_utf8_lossy(&tree.stderr));
    assert!(String::from_utf8_lossy(&tree.stdout).contains("pm"), "pm tree output");
}

/// The front-end drives the compiler through the `Exec` capability: `witchy pm
/// run`/`build` exec the bundled compiler to compile-and-run / compile a program
/// — the cargo→rustc split of RFC-0004, exercised through the real binary.
#[test]
fn witchy_pm_drives_the_compiler() {
    // run: compile and execute a program, capturing its output.
    let run = Command::new(BIN)
        .args(["pm", "run", "examples/hello/src/hello.witchy"])
        .output()
        .expect("run `witchy pm run`");
    assert!(run.status.success(), "pm run failed: {}", String::from_utf8_lossy(&run.stderr));
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("hello, witchy"),
        "pm run output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    // build: compile a program to verify it builds.
    let build = Command::new(BIN)
        .args(["pm", "build", "examples/hello/src/hello.witchy"])
        .output()
        .expect("run `witchy pm build`");
    assert!(build.status.success(), "pm build failed: {}", String::from_utf8_lossy(&build.stderr));
    assert!(
        String::from_utf8_lossy(&build.stdout).contains("ok"),
        "pm build output: {}",
        String::from_utf8_lossy(&build.stdout)
    );
}

/// Trusted-publishing CLIENT flow through the front-end: `witchy pm publish` reads
/// the identity token from `COVEN_ID_TOKEN` (the `Env` capability) and includes it
/// as `id_token`; the coven server verifies it against the namespace trust policy.
/// This closes the depth gap that gates deleting `src/pm` — the front-end can
/// publish under trusted publishing, not only anonymously.
#[test]
fn witchy_pm_publishes_with_trusted_token() {
    let server = RegistryServer::start();
    let base = unique("pm-tp");
    let dir = base.join("tplib");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/tplib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/tplib.witchy"), "pub fn n() -> Int:\n    1\n").unwrap();
    let ci = server.ci_token("acme/tplib-repo", "release.yml");
    let hostport = format!("127.0.0.1:{}", server.port);

    let out = Command::new(BIN)
        .current_dir(&base)
        // The Rust-CLI-like invocation: the registry comes from COVEN_URL (the
        // bootstrap auto-grants Net to it and the front-end reads its address), and
        // the identity token from COVEN_ID_TOKEN — no explicit `--net`/host:port.
        .args(["pm", "publish", "tplib"])
        .env("COVEN_URL", server.url())
        .env("COVEN_ID_TOKEN", &ci)
        .output()
        .expect("run `witchy pm publish` via COVEN_URL");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "pm publish (trusted) failed: {}\nstdout: {s}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        s.contains("publish: 200") && s.contains("staged"),
        "expected a staged trusted publish, got: {s}"
    );

    // Promote to released with a DISTINCT human identity (separation of duties):
    // the promoter (alice) must not be the uploader (ci-bot).
    let human = server.human_token("alice");
    let prom = Command::new(BIN)
        .current_dir(&base)
        .args(["pm", "promote", "acme/tplib", "1.0.0"])
        .env("COVEN_URL", server.url())
        .env("COVEN_ID_TOKEN", &human)
        .output()
        .expect("run `witchy pm promote` via COVEN_URL");
    let ps = String::from_utf8_lossy(&prom.stdout);
    assert!(
        prom.status.success(),
        "pm promote failed: {}\nstdout: {ps}",
        String::from_utf8_lossy(&prom.stderr)
    );
    assert!(ps.contains("promote: 200"), "expected promote 200 (released), got: {ps}");

    // Fetch the now-released rune over HTTP (the consuming side): the signed coven
    // record is verified against the registry root key and the source hash checked.
    let add = Command::new(BIN)
        .current_dir(&base)
        .args(["pm", "--net", &hostport, "add", "acme/tplib", "^1.0.0", &hostport, "vendored"])
        .env("WITCHY_COOLDOWN_SECS", "0")
        .output()
        .expect("run `witchy pm add`");
    assert!(
        add.status.success(),
        "pm add failed: {}\nstdout: {}",
        String::from_utf8_lossy(&add.stderr),
        String::from_utf8_lossy(&add.stdout)
    );
    assert!(
        base.join("vendored/tplib/src/tplib.witchy").exists(),
        "fetched rune source should be vendored"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The cargo-like consumer flow end to end through the front-end (RFC-0004 §5):
/// publish + promote a library over a real coven, then `witchy pm add <pkg>` it
/// (no explicit dest — vendored into the project's `vendor/` and pinned in
/// `witchy.lock`), then `witchy pm build`/`run` the consumer that `import`s the
/// fetched rune. `build` links the vendored dep via `witchy compile --dep` and
/// `run` compiles+links+runs it — the front-end deciding *what* to compile from
/// the lock and the compiler doing the build, all through the embedded CLI.
#[test]
fn witchy_pm_add_build_run_consumes_a_fetched_rune() {
    let server = RegistryServer::start();
    let base = unique("pm-consume");

    // Author + publish + promote `acme/lib` (a `shout` helper) via trusted publishing.
    let lib = base.join("lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(
        lib.join("witchy.toml"),
        "[rune]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("src/lib.witchy"),
        "pub fn shout(s: String) -> String:\n    \"HEY \" + s\n",
    )
    .unwrap();
    let ci = server.ci_token("acme/lib-repo", "release.yml");
    let pubd = Command::new(BIN)
        .current_dir(&base)
        .args(["pm", "publish", "lib"])
        .env("COVEN_URL", server.url())
        .env("COVEN_ID_TOKEN", &ci)
        .output()
        .expect("run `witchy pm publish`");
    assert!(
        pubd.status.success() && String::from_utf8_lossy(&pubd.stdout).contains("publish: 200"),
        "publish failed: {}\nstdout: {}",
        String::from_utf8_lossy(&pubd.stderr),
        String::from_utf8_lossy(&pubd.stdout)
    );
    let human = server.human_token("alice");
    let prom = Command::new(BIN)
        .current_dir(&base)
        .args(["pm", "promote", "acme/lib", "1.0.0"])
        .env("COVEN_URL", server.url())
        .env("COVEN_ID_TOKEN", &human)
        .output()
        .expect("run `witchy pm promote`");
    assert!(
        prom.status.success() && String::from_utf8_lossy(&prom.stdout).contains("promote: 200"),
        "promote failed: {}\nstdout: {}",
        String::from_utf8_lossy(&prom.stderr),
        String::from_utf8_lossy(&prom.stdout)
    );

    // Author the consumer that imports the fetched rune.
    let app = base.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n\n[dependencies]\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/app.witchy"),
        "import lib\n\nfn main(console: Console):\n    console.print(lib.shout(\"net\"))\n",
    )
    .unwrap();

    // Cargo-like add: registry from COVEN_URL, version defaults to latest released.
    // Vendors into the project's `vendor/lib` and pins it in `witchy.lock`.
    let add = Command::new(BIN)
        .current_dir(&app)
        .args(["pm", "add", "acme/lib"])
        .env("COVEN_URL", server.url())
        .env("WITCHY_COOLDOWN_SECS", "0")
        .output()
        .expect("run `witchy pm add`");
    let add_out = String::from_utf8_lossy(&add.stdout);
    assert!(
        add.status.success() && add_out.contains("added acme/lib@1.0.0"),
        "pm add failed: {}\nstdout: {add_out}",
        String::from_utf8_lossy(&add.stderr)
    );
    assert!(
        app.join("vendor/lib/src/lib.witchy").exists(),
        "the fetched rune must be vendored into the project's vendor/ tree"
    );
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    // BUG-193: the lock pins the dependency by its manifest identity `acme/lib`
    // (import alias `lib` recorded separately), not the spoofable alias key.
    assert!(
        lock.contains("name = \"acme/lib\"") && lock.contains("alias = \"lib\"") && lock.contains("version = \"1.0.0\"") && lock.contains("hash = \"sha256:"),
        "witchy.lock must pin the resolved dependency: {lock:?}"
    );

    // build: link the entry + vendored dep (`witchy compile --dep`) — must compile.
    let build = Command::new(BIN)
        .current_dir(&app)
        .args(["pm", "build", "."])
        .output()
        .expect("run `witchy pm build`");
    assert!(
        build.status.success() && String::from_utf8_lossy(&build.stdout).contains("ok"),
        "pm build failed: {}\nstdout: {}",
        String::from_utf8_lossy(&build.stderr),
        String::from_utf8_lossy(&build.stdout)
    );

    // run: compile+link+run the consumer importing the fetched rune.
    let run = Command::new(BIN)
        .current_dir(&app)
        .args(["pm", "run", "."])
        .output()
        .expect("run `witchy pm run`");
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout).contains("HEY net"),
        "pm run failed: {}\nstdout: {}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// RFC-0008's final acceptance criterion — "the framework rune is published to
/// coven as the proof" — end to end with the real `projects/glamour` source as
/// the payload. The glamour view layer is a capability-pure (empty-footprint)
/// frontend rune (`VNode`/`Attr`/`Cmd` + the compile-time `html` tag); this test
/// proves it DISTRIBUTES like any footprint-empty rune AND that its `html` tag
/// works against a registry-fetched copy:
///   1. Stage the committed glamour source under a NAMESPACED name (`aegis/glamour`)
///      and `publish` (trusted CI token) + `promote` (distinct human) it.
///   2. The registry RECOMPUTES the footprint server-side from source and signs
///      it into the record — independent of what the manifest declared. We assert
///      that recomputed `runtime_footprint` is EMPTY (the publish body and the
///      `/coven/record` fetch both show `runtime_footprint` with no entries). That
///      machine-checked empty footprint IS the proof, not a claim.
///   3. A PURE consumer app (no declared capabilities) `add`s the published
///      glamour WITHOUT any `--allow-cap` consent — corroborating the empty
///      footprint, since an honestly-footprinted rune that demanded a capability
///      would gate. It then BUILDS and RUNS an app that `import`s the fetched
///      glamour and uses the `html` tag in a non-`main` `view`, proving the whole
///      stack through the registry: publish → fetch → compile (tag expansion over
///      the vendored glamour) → run. We assert the rendered HTML.
#[test]
fn glamour_publishes_to_coven_empty_footprint_and_renders_through_html() {
    let server = RegistryServer::start();
    let base = unique("glamour-proof");

    // (1) Stage the REAL committed glamour source under a namespaced name. coven
    // requires `namespace/name`, so we vendor `projects/glamour/src/glamour.witchy`
    // verbatim under an `aegis/glamour` manifest (the module/import name stays
    // `glamour`). The manifest declares an empty runtime footprint; the registry
    // recomputes it from source regardless (asserted below).
    let lib = base.join("glamour");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    let glamour_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("projects/glamour/src/glamour.witchy");
    std::fs::copy(&glamour_src, lib.join("src/glamour.witchy")).expect("copy committed glamour source");
    std::fs::write(
        lib.join("witchy.toml"),
        "[rune]\nname = \"aegis/glamour\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = []\n\n[dependencies]\n",
    )
    .unwrap();

    // (2) Publish (trusted CI) — the server recomputes the footprint from source
    // (independent of the manifest) and signs it into the staged record. The
    // front-end's publish output is a summary line, so the recomputed footprint is
    // asserted off the signed record below (via `/coven/record`), not here.
    let ci = server.ci_token("aegis/glamour-repo", "release.yml");
    let pubd = Command::new(BIN)
        .current_dir(&base)
        .args(["pm", "publish", "glamour"])
        .env("COVEN_URL", server.url())
        .env("COVEN_ID_TOKEN", &ci)
        .output()
        .expect("run `witchy pm publish` for glamour");
    let pub_out = stdout(&pubd);
    assert!(
        pubd.status.success() && pub_out.contains("publish: 200"),
        "glamour publish failed: {}\nstdout: {pub_out}",
        stderr(&pubd)
    );

    // Promote to released with a DISTINCT human identity (separation of duties).
    let human = server.human_token("alice");
    let prom = Command::new(BIN)
        .current_dir(&base)
        .args(["pm", "promote", "aegis/glamour", "0.1.0"])
        .env("COVEN_URL", server.url())
        .env("COVEN_ID_TOKEN", &human)
        .output()
        .expect("run `witchy pm promote` for glamour");
    assert!(
        prom.status.success() && stdout(&prom).contains("promote: 200"),
        "glamour promote failed: {}\nstdout: {}",
        stderr(&prom),
        stdout(&prom)
    );

    // Independently fetch the signed record straight off the registry and confirm
    // its `runtime_footprint` is empty — the public, machine-checked record IS the
    // proof. The record key wire-encodes `/` as `~` (the pm client's `wire`).
    let (status, record) = http_get(
        &format!("127.0.0.1:{}", server.port),
        "/coven/record?name=aegis~glamour&version=0.1.0",
    );
    assert_eq!(status, 200, "record fetch failed: {record}");
    assert!(
        footprint_is_empty(&record),
        "the registry's signed record for aegis/glamour must show an EMPTY \
         runtime_footprint: {record}"
    );

    // (3) A PURE consumer app (no declared capabilities beyond Console) adds the
    // published glamour. A clean add with NO `--allow-cap` consent only succeeds
    // because the recomputed footprint is empty — an honest rune that demanded a
    // capability would gate (see
    // `gate_blocks_capability_widening_then_allows_with_consent`).
    let app = base.join("app");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n\n[dependencies]\n",
    )
    .unwrap();
    // The consumer uses the `html` tag in a NON-main `view` (returning a
    // `VNode(Msg)`), proving tag expansion runs over the registry-fetched glamour.
    // `main` renders the tree to an HTML string (`glamour.to_html`) and prints it,
    // so the rendered output is assertable from a plain `pm run`.
    std::fs::write(
        app.join("src/app.witchy"),
        "import glamour\nfrom glamour import VNode, Attr, HAttr, HtmlTok, Cmd, UiRoot, UiFetch, UiRoute, UiTimer, SecretInput, SecretRef, CredentialPort\n\
         import reflect\n\n\
         type Msg derive(Reflect):\n\
         \x20\x20\x20\x20Tick\n\n\
         fn view(n: Int) -> VNode(Msg):\n\
         \x20\x20\x20\x20html\"<div><span>${int_to_str(n)}</span></div>\"\n\n\
         fn int_to_str(n: Int) -> String:\n\
         \x20\x20\x20\x20\"${n}\"\n\n\
         fn main(console: Console):\n\
         \x20\x20\x20\x20console.print(glamour.to_html(view(7)))\n",
    )
    .unwrap();

    let add = Command::new(BIN)
        .current_dir(&app)
        .args(["pm", "add", "aegis/glamour"])
        .env("COVEN_URL", server.url())
        .env("WITCHY_COOLDOWN_SECS", "0")
        .output()
        .expect("run `witchy pm add` for glamour");
    let add_out = stdout(&add);
    assert!(
        add.status.success() && add_out.contains("added aegis/glamour@0.1.0"),
        "pm add of the footprint-empty glamour must succeed without consent: {}\nstdout: {add_out}",
        stderr(&add)
    );
    assert!(
        app.join("vendor/glamour/src/glamour.witchy").exists(),
        "the fetched glamour rune must be vendored into the project's vendor/ tree"
    );

    // build: compile the consumer + the vendored glamour (expanding `html` over the
    // fetched copy — exercises the reachability fix in src/tagged.rs).
    let build = Command::new(BIN)
        .current_dir(&app)
        .args(["pm", "build", "."])
        .output()
        .expect("run `witchy pm build`");
    assert!(
        build.status.success() && stdout(&build).contains("ok"),
        "pm build of the glamour consumer failed: {}\nstdout: {}",
        stderr(&build),
        stdout(&build)
    );

    // run: compile + link + run, then assert the rendered HTML — the WHOLE stack
    // worked through the registry: publish → fetch → tag-expand → render.
    let run = Command::new(BIN)
        .current_dir(&app)
        .args(["pm", "run", "."])
        .output()
        .expect("run `witchy pm run`");
    let run_out = stdout(&run);
    assert!(
        run.status.success() && run_out.contains("<div><span>7</span></div>"),
        "pm run of the glamour consumer failed: {}\nstdout: {run_out}",
        stderr(&run)
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// True when a coven record / publish-record JSON shows an EMPTY
/// `runtime_footprint` array (`[]`, tolerant of whitespace). The recomputed
/// footprint is the signed proof that a rune demands no capabilities.
fn footprint_is_empty(json: &str) -> bool {
    let Some(i) = json.find("\"runtime_footprint\"") else {
        return false;
    };
    let rest = &json[i + "\"runtime_footprint\"".len()..];
    let Some(colon) = rest.find(':') else {
        return false;
    };
    let after = rest[colon + 1..].trim_start();
    let Some(open) = after.find('[') else {
        return false;
    };
    let Some(close) = after[open..].find(']') else {
        return false;
    };
    after[open + 1..open + close].trim().is_empty()
}

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
/// var name must appear ONLY in the binary's parity command (`src/main.rs`), never
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
    let main = std::fs::read_to_string(root.join("src/main.rs")).unwrap();
    assert!(main.contains("WITCHY_SEEDED_DIVERGENCE"), "the seeded-divergence lever vanished from src/main.rs");
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

/// (BUG-100) A dependency's build step generates source that the consumer imports;
/// the front-end audits each generated file and emits one `--dep <module>=<path>`
/// per module (`audit_then_flags`). Those flags must be de-duped by module name —
/// `dep_flag` is idempotent — so a build that emits several modules yields exactly
/// one flag each, and the consumer links and runs against the generated code. This
/// exercises the build-step → audit → compile path end to end.
#[test]
fn build_step_generated_deps_link_and_run() {
    let work = unique("build-step-deps");
    let app = work.join("app");
    let lib = work.join("genlib");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::create_dir_all(lib.join("src")).unwrap();

    // The app depends on `genlib` and accepts its build step (empty grants section
    // permits only the confined BuildOut sandbox).
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"genlib\" = { path = \"../genlib\" }\n\n[build.grants.\"genlib\"]\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/app.witchy"),
        "import genmod\nimport genmod2\n\nfn main(console: Console):\n    console.print(\"${genmod.value() + genmod2.value()}\")\n",
    )
    .unwrap();

    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(lib.join("src/genlib.witchy"), "pub fn placeholder() -> Int:\n    0\n").unwrap();
    // The build step emits TWO modules; audit_then_flags must produce one --dep per
    // module (deduped by name), not duplicates.
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    out.write_out(\"genmod.witchy\", \"pub fn value() -> Int:\\n    40\\n\")\n    out.write_out(\"genmod2.witchy\", \"pub fn value() -> Int:\\n    2\\n\")\n",
    )
    .unwrap();

    let out = Command::new(BIN)
        .current_dir(&work)
        .args(["run", "app"])
        .output()
        .expect("spawn witchy run");
    assert!(
        out.status.success() && stdout(&out).contains("42"),
        "app must link + run against the generated modules: status {:?} stdout {} stderr {}",
        out.status.code(),
        stdout(&out),
        stderr(&out),
    );
}
