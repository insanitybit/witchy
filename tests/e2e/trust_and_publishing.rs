//! e2e: trust and publishing tests (extracted from tests/e2e.rs).

use std::process::Command;

use super::json_str;
use super::registry;

use super::support::coven::*;

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

/// RFC-0116 track 2, live JWKS-over-HTTPS issuer trust end to end: coven-serve
/// started with `--trust-issuer-oidc <issuer-url>` fetches the loopback IdP's
/// OIDC discovery document and its same-origin JWKS over TLS at startup
/// (self-signed cert trusted via `WITCHY_TLS_EXTRA_ROOTS`), and installs the
/// discovered keys for that issuer. A token signed by the issuer's key (under
/// the published `kid`) publishes; a token signed by an UNKNOWN key — same
/// issuer URL, same `kid`, different RSA key — is refused 401.
#[test]
fn coven_serve_trusts_a_live_oidc_issuer_via_https_discovery() {
    let server = RegistryServer::start_oidc("kid-live");
    let fe = FrontEnd::new(&server, "oidc");
    let lib = fe.lib("acme/live", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");

    let good = server.ci_token_kid("acme-live-repo", "release.yml", "kid-live");
    let out = fe.pm(&lib, &["publish", "."], Some(&good));
    assert!(
        out.status.success() && stdout(&out).contains("publish: 200"),
        "a token verified by the DISCOVERED JWKS should publish: {}",
        stdout(&out)
    );

    // The rogue key: a second issuer keypair minting under the trusted issuer's
    // URL and kid. Only the discovered JWKS may verify — refused 401.
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/live\"\nversion = \"1.1.0\"\n").unwrap();
    let rogue_dir = unique("oidc-rogue-issuer");
    let gen_out = Command::new(BIN).args(["coven-gen-issuer", "--out", rogue_dir.to_str().unwrap()]).output().unwrap();
    assert!(gen_out.status.success(), "rogue gen-issuer failed");
    let mint = Command::new(BIN)
        .args([
            "coven-mint-token",
            "--issuer-key",
            rogue_dir.to_str().unwrap(),
            "--issuer",
            server.issuer(),
            "--sub",
            "repo:acme-live-repo:ref:refs/heads/main",
            "--kid",
            "kid-live",
            "--claim",
            "repository=acme-live-repo",
            "--claim",
            "workflow_ref=release.yml",
        ])
        .output()
        .unwrap();
    assert!(mint.status.success(), "rogue mint failed: {}", String::from_utf8_lossy(&mint.stderr));
    let rogue = String::from_utf8_lossy(&mint.stdout).trim().to_string();
    let out = fe.pm(&lib, &["publish", "."], Some(&rogue));
    assert!(!out.status.success(), "a token signed by an unknown key must be refused");
    assert!(stdout(&out).contains("publish: 401"), "unknown-key publish: {}", stdout(&out));
    let _ = std::fs::remove_dir_all(&rogue_dir);
}

/// The fail-loud startup contract: with `--trust-issuer-oidc` pointing at an
/// unreachable issuer, coven-serve exits nonzero BEFORE binding its listen
/// address — a registry must never come up with silently-empty trust. The
/// Rust dispatcher additionally refuses a plaintext-http issuer outright.
#[test]
fn coven_serve_refuses_to_start_when_oidc_discovery_fails() {
    // A port with no listener: the discovery fetch fails immediately.
    let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let regroot = unique("coven-oidc-dead");
    std::fs::create_dir_all(&regroot).unwrap();
    let seed = regroot.join("root.seed");
    std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
    let addr_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{addr_port}");
    let home = unique("coven-oidc-dead-home");
    let out = Command::new(BIN)
        .args([
            "coven-serve",
            "--addr",
            &addr,
            "--root",
            regroot.to_str().unwrap(),
            "--trust-issuer-oidc",
            &format!("https://localhost:{dead}"),
            "--signing-key",
            seed.to_str().unwrap(),
        ])
        .env("WITCHY_HOME", &home)
        .output()
        .expect("run coven-serve");
    assert!(!out.status.success(), "an unreachable issuer must abort startup");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot trust OIDC issuer"), "stderr should name the failure: {err}");
    // It aborted before ever binding the listen address.
    assert!(std::net::TcpStream::connect(&addr).is_err(), "the server must not have come up");

    // A plaintext-http issuer never reaches the program at all.
    let out = Command::new(BIN)
        .args([
            "coven-serve",
            "--addr",
            &addr,
            "--root",
            regroot.to_str().unwrap(),
            "--trust-issuer-oidc",
            "http://localhost:1",
            "--signing-key",
            seed.to_str().unwrap(),
        ])
        .env("WITCHY_HOME", &home)
        .output()
        .expect("run coven-serve");
    assert!(!out.status.success(), "an http issuer must be refused");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("must be an https:// URL"),
        "http issuer refusal: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&regroot);
    let _ = std::fs::remove_dir_all(&home);
}
