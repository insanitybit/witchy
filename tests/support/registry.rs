use super::support::coven::*;

use std::process::Command;

pub(crate) fn trusted_publishing_binds_repo_single_use_and_first_bind() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "auth");
    let lib = fe.lib("acme/secure", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");

    // SEC-023: a token from a DIFFERENT org cannot first-claim the `acme` namespace.
    let evil_org = server.ci_token("evilcorp/fork", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&evil_org));
    assert!(!out.status.success(), "a cross-org first-bind must be refused");
    assert!(stdout(&out).contains("publish: 403"), "cross-org bind: {}", stdout(&out));

    // A repository whose org IS the namespace may claim it: the first trusted
    // publish from acme/secure-repo / release.yml binds namespace `acme`.
    let good = server.ci_token("acme/secure-repo", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&good));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "matching-org bind: {}", stdout(&out));

    // A token from a DIFFERENT repository cannot publish to `acme` (namespace hijack).
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/secure\"\nversion = \"1.1.0\"\n").unwrap();
    let evil = server.ci_token("evil-fork", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&evil));
    assert!(!out.status.success(), "publish from wrong repo must be refused");
    assert!(stdout(&out).contains("publish: 403"), "wrong-repo: {}", stdout(&out));

    // A token from the right repo but a NON-release workflow is also refused.
    let wrong_wf = server.ci_token("acme/secure-repo", "ci.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&wrong_wf));
    assert!(!out.status.success(), "publish from wrong workflow must be refused");
    assert!(stdout(&out).contains("publish: 403"), "wrong-workflow: {}", stdout(&out));

    // SEC-022: replaying the token that ALREADY published 1.0.0 — right repo and
    // workflow, so only the consumed `jti` can refuse it — is rejected (single-use).
    let out = fe.pm(&lib, &["publish", "."], Some(&good));
    assert!(!out.status.success(), "a replayed publish token must be refused");
    assert!(stdout(&out).contains("publish: 403"), "replayed token: {}", stdout(&out));

    // The legitimate CI identity may publish the new version — with a FRESH token,
    // as a real workflow mints one per run.
    let good2 = server.ci_token("acme/secure-repo", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&good2));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "legit re-publish: {}", stdout(&out));

    // BUG-219: a client-controlled marker plus a valid identity token is not a
    // second-factor proof. The trusted IdP must attest MFA/WebAuthn in `amr`.
    let marker_only = server.mint("mallory", &[]);
    let out = fe.pm(&lib, &["promote", "acme/secure", "1.1.0"], Some(&marker_only));
    assert!(!out.status.success(), "a marker-only promote must be refused");
    assert!(stdout(&out).contains("promote: 403"), "marker-only promote: {}", stdout(&out));

    // A distinct human with IdP-attested WebAuthn promotes it to released.
    let alice = server.human_token("alice");
    let out = fe.pm(&lib, &["promote", "acme/secure", "1.1.0"], Some(&alice));
    assert!(out.status.success() && stdout(&out).contains("promote: 200"), "human promote: {}", stdout(&out));

    // The attested token is single-use. It cannot release a second staged
    // version; a freshly minted token for the same maintainer can.
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/secure\"\nversion = \"1.2.0\"\n").unwrap();
    let good3 = server.ci_token("acme/secure-repo", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&good3));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "third publish: {}", stdout(&out));
    let out = fe.pm(&lib, &["promote", "acme/secure", "1.2.0"], Some(&alice));
    assert!(!out.status.success(), "a replayed MFA token must be refused");
    assert!(stdout(&out).contains("promote: 403"), "replayed promote token: {}", stdout(&out));

    let alice2 = server.human_token("alice");
    let out = fe.pm(&lib, &["promote", "acme/secure", "1.2.0"], Some(&alice2));
    assert!(out.status.success() && stdout(&out).contains("promote: 200"), "fresh human promote: {}", stdout(&out));
    let (status, record) = http_get(
        &format!("127.0.0.1:{}", server.port),
        "/coven/record?name=acme~secure&version=1.2.0",
    );
    assert_eq!(status, 200, "promoted record fetch failed: {record}");
    assert!(
        record.contains("\"second_factor\":\"oidc-amr:webauthn\""),
        "the signed record must contain the verified authentication method, not the request marker: {record}"
    );
}

pub(crate) fn trusted_yank_requires_a_maintainer() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "yauth");
    let lib = fe.lib("acme/y", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");

    let ci = server.ci_token("acme-y-repo", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&ci));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "publish: {}", stdout(&out));
    let alice = server.human_token("alice");
    let out = fe.pm(&lib, &["promote", "acme/y", "1.0.0"], Some(&alice));
    assert!(out.status.success() && stdout(&out).contains("promote: 200"), "promote: {}", stdout(&out));

    // A non-maintainer token cannot yank.
    let mallory = server.human_token("mallory");
    let out = fe.pm(&lib, &["yank", "acme/y", "1.0.0"], Some(&mallory));
    assert!(!out.status.success(), "a non-maintainer must not yank");
    assert!(stdout(&out).contains("yank: 403"), "non-maintainer yank: {}", stdout(&out));

    // The maintainer (alice) may yank.
    let out = fe.pm(&lib, &["yank", "acme/y", "1.0.0"], Some(&alice));
    assert!(out.status.success() && stdout(&out).contains("yank: 200"), "maintainer yank: {}", stdout(&out));
}

pub(crate) fn trusted_publishing_verifies_a_jwks_issuer_by_kid() {
    let server = RegistryServer::start_jwks("kid-1");
    let fe = FrontEnd::new(&server, "jwks");
    let lib = fe.lib("acme/widget", "0.1.0", "pub fn f(s: String) -> String:\n    s\n");

    // A token signed under the JWKS's published `kid` verifies and publishes.
    let good = server.ci_token_kid("acme-widget-repo", "release.yml", "kid-1");
    let out = fe.pm(&lib, &["publish", "."], Some(&good));
    assert!(
        out.status.success() && stdout(&out).contains("publish: 200"),
        "a token whose kid is in the JWKS should publish: {}",
        stdout(&out)
    );

    // A token whose `kid` is not in the JWKS cannot be matched to a key → refused (401).
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/widget\"\nversion = \"0.2.0\"\n").unwrap();
    let unknown = server.ci_token_kid("acme-widget-repo", "release.yml", "kid-rotated-away");
    let out = fe.pm(&lib, &["publish", "."], Some(&unknown));
    assert!(!out.status.success(), "a token with an unknown kid must be refused");
    assert!(stdout(&out).contains("publish: 401"), "unknown-kid: {}", stdout(&out));
}

pub(crate) fn coven_serves_generated_api_docs() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "docs");
    let lib = fe.lib(
        "acme/greeter",
        "1.0.0",
        "// Greets a name warmly.\npub fn greet(name: String) -> String:\n    \"hi \" + name\n",
    );
    let ci = server.ci_token("acme-greeter-repo", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&ci));
    assert!(stdout(&out).contains("publish: 200"), "publish: {}", stdout(&out));

    let (status, body) =
        http_get(&format!("127.0.0.1:{}", server.port), "/coven/doc?name=acme~greeter&version=1.0.0");
    assert_eq!(status, 200, "doc status: {body}");
    // It is RENDERED markdown (the `####` heading the doc renderer emits — not the raw
    // `pub fn …:` source), naming the public function and carrying its doc-comment.
    assert!(body.contains("greet"), "docs should name the public fn: {body}");
    assert!(body.contains("#### "), "docs should be rendered markdown headings: {body}");
    assert!(
        body.contains("fn greet(name: String) -> String"),
        "docs should render the function signature: {body}"
    );
    assert!(body.contains("Greets a name warmly"), "docs should include the doc-comment: {body}");
}

pub(crate) fn coven_audits_embedded_compartments() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "audit");
    // Both runes are namespace `acme`, so both publish from the one TOFU-bound repo.
    let ci = server.ci_token("acme-repo", "release.yml");

    // A rune that embeds a foreign-code compartment (a d3 chart).
    let viz = fe.lib(
        "acme/viz",
        "1.0.0",
        "// renders with glamour.compartment(\"d3-runes-chart\")\npub fn f(s: String) -> String:\n    s\n",
    );
    let out = fe.pm(&viz, &["publish", "."], Some(&ci));
    assert!(stdout(&out).contains("publish: 200"), "viz publish: {}", stdout(&out));
    let (status, body) =
        http_get(&format!("127.0.0.1:{}", server.port), "/coven/compartments?name=acme~viz&version=1.0.0");
    assert_eq!(status, 200, "compartments status: {body}");
    assert!(body.contains("d3-runes-chart"), "the audit should flag the embedded compartment: {body}");

    // A rune with no compartment embed reports none. Mint a FRESH CI token: tokens are
    // single-use (see `trusted_publishing_binds_repo_single_use_and_first_bind`), and the
    // first publish already consumed `ci`. Same repo, so it publishes into the same
    // TOFU-bound namespace.
    let ci2 = server.ci_token("acme-repo", "release.yml");
    let plain = fe.lib("acme/plain", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out2 = fe.pm(&plain, &["publish", "."], Some(&ci2));
    assert!(stdout(&out2).contains("publish: 200"), "plain publish: {}", stdout(&out2));
    let (status2, body2) =
        http_get(&format!("127.0.0.1:{}", server.port), "/coven/compartments?name=acme~plain&version=1.0.0");
    assert_eq!(status2, 200, "compartments2 status: {body2}");
    assert!(!body2.contains("d3-runes-chart"), "a package with no compartments must not flag one: {body2}");
}

pub(crate) fn token_required_and_untrusted_issuer_refused() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "nobearer");
    let lib = fe.lib("acme/xray", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");

    // No identity token at all → publish is refused outright (401).
    let out = fe.pm(&lib, &["publish", "."], None);
    assert!(!out.status.success(), "publish without an identity token must be refused");
    assert!(stdout(&out).contains("publish: 401"), "no-token: {}", stdout(&out));

    // A token from an UNTRUSTED issuer is also refused (401).
    let other_issuer = unique("rogue-idp");
    let gen_out = Command::new(BIN).args(["coven-gen-issuer", "--out", other_issuer.to_str().unwrap()]).output().unwrap();
    assert!(gen_out.status.success());
    let mint = Command::new(BIN)
        .args(["coven-mint-token", "--issuer-key", other_issuer.to_str().unwrap(), "--issuer", "rogue", "--sub", "x", "--claim", "repository=acme-xray-repo"])
        .output()
        .unwrap();
    let rogue = String::from_utf8_lossy(&mint.stdout).trim().to_string();
    let out = fe.pm(&lib, &["publish", "."], Some(&rogue));
    assert!(!out.status.success(), "token from untrusted issuer must be refused");
    assert!(stdout(&out).contains("publish: 401"), "untrusted-issuer: {}", stdout(&out));
    let _ = std::fs::remove_dir_all(&other_issuer);
}

pub(crate) fn tuf_chain_verified_and_snapshot_tamper_rejected() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "tuf");
    let app = fe.new_app();
    fe.published_lib("acme/tango", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/tango"], None);
    assert!(out.status.success(), "add failed: {}\n{}", stderr(&out), stdout(&out));

    // The lock pinned a TUF snapshot version, and verify confirms the chain.
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("registry_snapshot_version"), "lock should pin snapshot version: {lock}");
    // SEC-002: the lock also pins the registry's Ed25519 root public key (trust-on-first-use),
    // so `verify` refuses a chain rooted in a different (MITM/hostile-mirror) key.
    assert!(lock.contains("registry_rootpub = \""), "lock should pin the root key: {lock}");
    let out = fe.pm(&app, &["verify", ".", "--online"], None);
    assert!(out.status.success(), "verify failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("TUF chain"), "verify out: {}", stdout(&out));

    // Tamper the SERVER's signed snapshot (changing a signed field breaks the
    // snapshot-role signature). The front-end vendors, so there is no client cache
    // to clear — `verify` re-fetches the role and must reject the broken signature.
    let snap = server.regroot.join("registry/snapshot.json");
    let body = std::fs::read_to_string(&snap).unwrap().replace("1.0.0", "1.0.1");
    std::fs::write(&snap, body).unwrap();

    let out = fe.pm(&app, &["verify", ".", "--online"], None);
    assert!(!out.status.success(), "tampered snapshot must fail verify");
    assert!(stdout(&out).contains("FAIL"), "verify out: {}", stdout(&out));
}

pub(crate) fn witchy_pm_verify_rejects_malformed_registry_trust_pins() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "badpin");
    let app = fe.new_app();
    fe.published_lib("acme/badpin", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/badpin"], None);
    assert!(out.status.success(), "add failed: {}\n{}", stderr(&out), stdout(&out));

    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    let cases = [
        (
            lock.lines()
                .map(|line| {
                    if line.trim_start().starts_with("registry_snapshot_version") {
                        "registry_snapshot_version = nope".to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            "registry_snapshot_version `nope` (not an integer)",
        ),
        (
            lock.lines()
                .filter(|line| !line.trim_start().starts_with("registry_rootpub"))
                .collect::<Vec<_>>()
                .join("\n"),
            "registry_snapshot_version is present but registry_rootpub is missing",
        ),
        (
            lock.lines()
                .filter(|line| !line.trim_start().starts_with("registry_snapshot_version"))
                .collect::<Vec<_>>()
                .join("\n"),
            "registry_rootpub is present but registry_snapshot_version is missing",
        ),
        (
            lock.lines()
                .map(|line| {
                    if line.trim_start().starts_with("registry_rootpub") {
                        "registry_rootpub = \"not-hex\"".to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            "invalid witchy.lock registry_rootpub",
        ),
    ];

    for (corrupted, expected) in cases {
        std::fs::write(app.join("witchy.lock"), corrupted).unwrap();
        let out = fe.pm(&app, &["verify", ".", "--online"], None);
        assert!(!out.status.success(), "malformed trust record must fail verify");
        assert!(stdout(&out).contains(expected), "expected `{expected}`, got: {}", stdout(&out));
    }
}

pub(crate) fn tuf_chain_rejects_validly_signed_malformed_roles() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "tufschema");
    let app = fe.new_app();
    fe.published_lib("acme/schema", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/schema"], None);
    assert!(out.status.success(), "add failed: {}\n{}", stderr(&out), stdout(&out));

    let snap_path = server.regroot.join("registry/snapshot.json");
    let ts_path = server.regroot.join("registry/timestamp.json");
    let original_snapshot = std::fs::read_to_string(&snap_path).unwrap();
    let original_timestamp = std::fs::read_to_string(&ts_path).unwrap();
    let original_snapshot_json: serde_json::Value = serde_json::from_str(&original_snapshot).unwrap();
    let original_timestamp_json: serde_json::Value = serde_json::from_str(&original_timestamp).unwrap();
    let version = original_snapshot_json["signed"]["version"].as_i64().unwrap();

    let malformed_snapshot = format!(r#"{{"version":{version},"created":0}}"#);
    let malformed_timestamp = format!(
        r#"{{"snapshot_version":{version},"snapshot_hash":"sha256:{}","expires":9999999999}}"#,
        sha256_hex(&malformed_snapshot)
    );
    std::fs::write(&snap_path, signed_test_root_role(&malformed_snapshot)).unwrap();
    std::fs::write(&ts_path, signed_test_root_role(&malformed_timestamp)).unwrap();

    let out = fe.pm(&app, &["verify", ".", "--online"], None);
    assert!(!out.status.success(), "malformed snapshot must fail verify");
    assert!(
        stdout(&out).contains("snapshot role is structurally malformed"),
        "out: {}",
        stdout(&out)
    );

    std::fs::write(&snap_path, original_snapshot).unwrap();
    let malformed_timestamp = format!(
        r#"{{"snapshot_version":{version},"snapshot_hash":"{}"}}"#,
        original_timestamp_json["signed"]["snapshot_hash"].as_str().unwrap()
    );
    std::fs::write(&ts_path, signed_test_root_role(&malformed_timestamp)).unwrap();

    let out = fe.pm(&app, &["verify", ".", "--online"], None);
    assert!(!out.status.success(), "malformed timestamp must fail verify");
    assert!(
        stdout(&out).contains("timestamp role is structurally malformed"),
        "out: {}",
        stdout(&out)
    );

    std::fs::write(&ts_path, original_timestamp).unwrap();
}

pub(crate) fn tuf_rollback_is_rejected() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "rollback");
    let app = fe.new_app();
    fe.published_lib("acme/romeo", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/romeo"], None);
    assert!(out.status.success(), "add failed: {}\n{}", stderr(&out), stdout(&out));

    // Simulate having previously seen a much newer snapshot: bump the pinned
    // version in the lock. The server still presents an older snapshot version —
    // a rollback — which must be refused.
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    let bumped = lock
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("registry_snapshot_version") {
                "registry_snapshot_version = 9999".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(app.join("witchy.lock"), bumped).unwrap();

    let out = fe.pm(&app, &["verify", ".", "--online"], None);
    assert!(!out.status.success(), "rollback must be refused");
    assert!(
        stdout(&out).contains("rolled back") || stdout(&out).contains("rollback"),
        "out: {}",
        stdout(&out)
    );
}

pub(crate) fn networked_registry_signature_detects_tampering() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "nettamper");
    let lib = fe.lib("acme/xray", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    let ci = server.ci_token("acme-xray-repo", "release.yml");
    let out = fe.pm(&lib, &["publish", "."], Some(&ci));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "publish: {}", stdout(&out));
    let alice = server.human_token("alice");
    let out = fe.pm(&lib, &["promote", "acme/xray", "1.0.0"], Some(&alice));
    assert!(out.status.success() && stdout(&out).contains("promote: 200"), "promote: {}", stdout(&out));

    // Tamper a signed field of the record in the SERVER's storage (the provenance
    // attestation is signed, so editing it breaks the root-key signature).
    let meta = server.regroot.join("registry/acme/xray/1.0.0/coven.json");
    let json = std::fs::read_to_string(&meta).unwrap().replace("trusted-publisher", "evil-publisher");
    std::fs::write(&meta, json).unwrap();

    // A fresh `add` fetches the tampered record and must refuse it via the signature.
    let app = fe.new_app();
    let out = fe.pm(&app, &["add", "acme/xray"], None);
    assert!(!out.status.success(), "tampered remote record must be refused");
    assert!(
        stdout(&out).contains("invalid signature") || stdout(&out).contains("BLOCK"),
        "stdout {} stderr {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(!app.join("vendor/xray").exists(), "nothing should be vendored on a rejected add");
}
