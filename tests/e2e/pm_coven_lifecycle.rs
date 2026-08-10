//! e2e: pm coven lifecycle tests (extracted from tests/e2e.rs).

use std::path::Path;
use std::process::{Command, Stdio};

use super::json_str;
use super::package_manager;

use super::support::coven::*;

/// RFC-0116 track 1: the pm client speaks HTTPS to a registry. A loopback TLS
/// server (rustls, rcgen self-signed `localhost` cert — the coven_web mock
/// pattern) plays the registry's `/coven/versions` endpoint; `COVEN_URL` is an
/// `https://localhost:<port>` origin, so the scheme must survive `coven_addr` →
/// `parse_origin` → `registry_origin` into a rustls dial, and the bootstrap's
/// auto-grant must admit the host:port. The self-signed cert is trusted via
/// `WITCHY_TLS_EXTRA_ROOTS`, exactly like the coven-web OAuth e2e.
#[test]
fn pm_lists_versions_from_an_https_registry() {
    use std::io::{Read, Write};
    use std::sync::Arc;
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let cert_dir = unique("pm-https-cert");
    let cert_path = cert_dir.join("cert.pem");
    std::fs::write(&cert_path, ck.cert.pem()).unwrap();

    let (request_tx, request_rx) = std::sync::mpsc::channel::<String>();
    let server = std::thread::spawn(move || {
        let (tcp, _) = listener.accept().unwrap();
        let conn = rustls::ServerConnection::new(tls_config).unwrap();
        let mut tls = rustls::StreamOwned::new(conn, tcp);
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while tls.read_exact(&mut b).is_ok() {
            head.push(b[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let request_line = String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_string();
        let _ = request_tx.send(request_line);
        let body = r#"{"records":[{"version":"0.1.0","state":"released"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = tls.write_all(resp.as_bytes());
        let _ = tls.flush();
        tls.conn.send_close_notify();
        let _ = tls.flush();
    });

    let work = unique("pm-https-list");
    let out = Command::new(BIN)
        .current_dir(&work)
        .env("COVEN_URL", format!("https://localhost:{port}"))
        .env("WITCHY_TLS_EXTRA_ROOTS", &cert_path)
        .args(["pm", "list", "demo/x"])
        .output()
        .expect("spawn witchy pm list");
    server.join().unwrap();
    let _ = std::fs::remove_dir_all(&cert_dir);
    let _ = std::fs::remove_dir_all(&work);

    let request_line = request_rx.recv_timeout(std::time::Duration::from_secs(8)).unwrap();
    assert_eq!(
        request_line, "GET /coven/versions?name=demo~x HTTP/1.1",
        "the registry request must arrive over the TLS session"
    );
    assert!(
        out.status.success(),
        "pm list over https failed: stdout={} stderr={}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("demo/x@0.1.0 released"),
        "pm list must print the registry's released version: {}",
        stdout(&out)
    );
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
    assert!(
        app.join("vendor/strkit/src/strkit.witchy").exists(),
        "the fetched rune source must be vendored",
    );

    // RFC-0117 Lane A: `add` also records the dependency in the manifest's
    // `[dependencies]` (import alias keyed, a caret requirement of the resolved
    // version — the manifest holds the requirement, the lock pins the exact
    // version), in the same inline shape a hand-written registry dep uses — so the
    // whole-tree authority commands are no longer blind to registry deps.
    let manifest = std::fs::read_to_string(app.join("witchy.toml")).unwrap();
    assert!(
        manifest.contains("\"strkit\" = { version = \"^0.1.0\" }"),
        "add must record the registry dep in [dependencies]: {manifest}"
    );
    // Re-adding must not duplicate or corrupt the entry (idempotency).
    let out = fe.pm(&app, &["add", "acme/strkit"], None);
    assert!(out.status.success(), "re-add failed: {}\n{}", stderr(&out), stdout(&out));
    let manifest = std::fs::read_to_string(app.join("witchy.toml")).unwrap();
    assert_eq!(
        manifest.matches("\"strkit\" = { version =").count(),
        1,
        "re-add must not duplicate the [dependencies] entry: {manifest}"
    );
    // `tree` and `why` now see the registry dep (previously "(no dependencies)").
    let out = fe.pm(&app, &["tree", "."], None);
    assert!(
        stdout(&out).contains("strkit") && !stdout(&out).contains("(no dependencies)"),
        "tree must show the added registry dep: {}",
        stdout(&out)
    );
    let out = fe.pm(&app, &["why", ".", "strkit"], None);
    assert!(
        stdout(&out).contains("strkit — direct dependency"),
        "why must report the registry dep as direct: {}",
        stdout(&out)
    );

    let out = fe.pm(&app, &["build", "."], None);
    assert!(out.status.success() && stdout(&out).contains("ok"), "build: {}", stderr(&out));

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

/// SEC-048: `pm update` re-resolves and re-fetches versions; a new version can pull
/// a NEW transitive dependency that widens the project's capability footprint past
/// the gate. A registry record's footprint is single-rune, so the per-record gate
/// alone misses it. `update` must re-gate the WHOLE re-resolved closure — blocking a
/// transitive widening (exit 2) without `--allow-cap` and leaving `witchy.lock`
/// untouched, exactly as `add` does — and proceed once the widening is consented.
#[test]
fn witchy_pm_update_regates_transitive_widening() {
    package_manager::witchy_pm_update_regates_transitive_widening();
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

#[test]
fn coven_registry_serves_a_human_landing_page_at_root() {
    // A browser hitting the registry root must learn what it is — not a 404.
    let server = RegistryServer::start();
    let (status, body) = http_get(&format!("127.0.0.1:{}", server.port), "/");
    assert_eq!(status, 200, "landing page fetch failed: {body}");
    for expectation in ["coven", "/coven/index", "/coven/rootpub", "COVEN_URL"] {
        assert!(body.contains(expectation), "landing page is missing `{expectation}`: {body}");
    }
}

/// RFC-0095: an application release ships an `artifact.json` manifest beside its
/// `witchy.toml`. `pm publish` attaches it; coven validates + freezes it, the
/// signed record commits to its digest (coven-v2), and `/coven/artifact` serves
/// the manifest verbatim for the installer. A source-only sibling stays coven-v1
/// and its `/coven/artifact` is a 404.
#[test]
fn publish_with_artifact_manifest_produces_coven_v2_and_serves_it() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "artifact");
    let app = fe.lib("acme/wrg", "0.1.0", "pub fn go() -> String:\n    \"hi\"\n");
    // A minimal, valid one-target artifact manifest (coven_artifact shape).
    let manifest = "{\"version\":1,\"artifacts\":{\"aarch64-apple-darwin\":{\"kind\":\"trusted-exe\",\"command\":\"wrg\",\"sha256\":\"416e71b2cf6ce3fb316381f0eec039aa5b586e931f925f4d1f0be1d2d5009b5d\",\"size\":128,\"binding_plan_sha256\":\"\",\"authority\":[]}}}";
    std::fs::write(app.join("artifact.json"), manifest).unwrap();

    let ci = server.ci_token("acme-wrg-repo", "release.yml");
    let out = fe.pm(&app, &["publish", "."], Some(&ci));
    assert!(
        out.status.success() && stdout(&out).contains("publish: 200"),
        "publish with artifact: {}\n{}",
        stdout(&out),
        stderr(&out)
    );

    let addr = format!("127.0.0.1:{}", server.port);
    // The signed record commits to the manifest digest (coven-v2).
    let (rstatus, record) = http_get(&addr, "/coven/record?name=acme~wrg&version=0.1.0");
    assert_eq!(rstatus, 200, "record fetch: {record}");
    assert!(
        record.contains("artifact_digest") && record.contains("sha256:"),
        "record must carry a sha256 artifact_digest (coven-v2): {record}"
    );
    // The manifest is served back verbatim for the installer to read.
    let (astatus, served) = http_get(&addr, "/coven/artifact?name=acme~wrg&version=0.1.0");
    assert_eq!(astatus, 200, "artifact manifest fetch: {served}");
    assert!(
        served.contains("aarch64-apple-darwin") && served.contains("wrg"),
        "served manifest is the one published: {served}"
    );

    // A source-only sibling has no artifact manifest → 404, and stays coven-v1.
    // Same namespace + repo (so the binding matches), but a fresh single-use token.
    let plain = fe.lib("acme/plain", "0.1.0", "pub fn go() -> String:\n    \"hi\"\n");
    let ci_plain = server.ci_token("acme-wrg-repo", "release.yml");
    let out2 = fe.pm(&plain, &["publish", "."], Some(&ci_plain));
    assert!(out2.status.success() && stdout(&out2).contains("publish: 200"), "publish plain: {}", stdout(&out2));
    let (n404, _) = http_get(&addr, "/coven/artifact?name=acme~plain&version=0.1.0");
    assert_eq!(n404, 404, "a source-only release has no artifact manifest");
    let (_, plain_rec) = http_get(&addr, "/coven/record?name=acme~plain&version=0.1.0");
    assert!(
        !plain_rec.contains("\"artifact_digest\":\"sha256:"),
        "a source-only record must not commit to an artifact digest: {plain_rec}"
    );
}

/// RFC-0095: a publisher ships built trusted-exe bytes in `<dir>/artifacts/<target>`.
/// `pm publish` uploads each after the release; coven re-hashes the decoded bytes
/// against the SIGNED manifest (crypto.sha256_bytes), so only the attested bytes are
/// accepted — and `/coven/artifact/bytes` serves them back for the installer. A
/// direct upload of mismatched bytes is refused by the sha256 gate.
#[test]
fn publish_uploads_artifact_bytes_then_serves_them() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "artbytes");
    let app = fe.lib("acme/wrg", "0.2.0", "pub fn go() -> String:\n    \"hi\"\n");
    // The manifest commits to sha256("hello"); the uploaded bytes must match it.
    let sha_hello = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    let manifest = format!(
        "{{\"version\":1,\"artifacts\":{{\"aarch64-apple-darwin\":{{\"kind\":\"trusted-exe\",\"command\":\"wrg\",\"sha256\":\"{sha_hello}\",\"size\":5,\"binding_plan_sha256\":\"\",\"authority\":[]}}}}}}"
    );
    std::fs::write(app.join("artifact.json"), &manifest).unwrap();
    std::fs::create_dir_all(app.join("artifacts")).unwrap();
    std::fs::write(app.join("artifacts").join("aarch64-apple-darwin"), b"hello").unwrap();

    let ci = server.ci_token("acme-wrg-repo", "release.yml");
    let out = fe.pm(&app, &["publish", "."], Some(&ci));
    assert!(
        out.status.success() && stdout(&out).contains("publish: 200"),
        "publish: {}\n{}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("artifact aarch64-apple-darwin: 200"),
        "pm must upload the built artifact bytes after publish: {}",
        stdout(&out)
    );

    let addr = format!("127.0.0.1:{}", server.port);
    // Fetch the bytes back: base64("hello") == "aGVsbG8=".
    let (bstatus, bytes_body) = http_get(
        &addr,
        "/coven/artifact/bytes?name=acme~wrg&version=0.2.0&target=aarch64-apple-darwin",
    );
    assert_eq!(bstatus, 200, "artifact bytes fetch: {bytes_body}");
    assert!(
        bytes_body.contains("aGVsbG8="),
        "served bytes are the base64 of the uploaded blob: {bytes_body}"
    );

    // A direct upload of MISMATCHED bytes (base64("world")) is refused by the sha256 gate.
    let bad = r#"{"name":"acme/wrg","version":"0.2.0","target":"aarch64-apple-darwin","bytes":"d29ybGQ="}"#;
    let (mstatus, mbody) = http_post(&addr, "/coven/artifact/bytes", bad);
    assert_eq!(mstatus, 400, "mismatched bytes must be refused: {mbody}");
    assert!(
        mbody.contains("does not match"),
        "the rejection explains the sha256 mismatch: {mbody}"
    );
}

/// RFC-0095 Cut 3: `pm install` runs the full trust chain end to end — resolve the
/// released version, verify the signed record against the registry root key, read
/// the coven-v2 artifact_digest, digest-verify the manifest, select the target,
/// fetch the bytes and check their sha256 — then writes the trusted-exe into the
/// project's `.witchy/bin/`. A source-only package is refused.
#[test]
fn install_fetches_verifies_and_writes_a_trusted_exe() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "install");
    let appsrc = fe.lib("acme/wrg", "0.3.0", "pub fn go() -> String:\n    \"hi\"\n");
    let sha_hello = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    let manifest = format!(
        "{{\"version\":1,\"artifacts\":{{\"aarch64-apple-darwin\":{{\"kind\":\"trusted-exe\",\"command\":\"wrg\",\"sha256\":\"{sha_hello}\",\"size\":5,\"binding_plan_sha256\":\"\",\"authority\":[]}}}}}}"
    );
    std::fs::write(appsrc.join("artifact.json"), &manifest).unwrap();
    std::fs::create_dir_all(appsrc.join("artifacts")).unwrap();
    std::fs::write(appsrc.join("artifacts").join("aarch64-apple-darwin"), b"hello").unwrap();

    let ci = server.ci_token("acme-wrg-repo", "release.yml");
    let out = fe.pm(&appsrc, &["publish", "."], Some(&ci));
    assert!(out.status.success() && stdout(&out).contains("publish: 200"), "publish: {}\n{}", stdout(&out), stderr(&out));
    // Install requires a RELEASED version — promote with a distinct human identity.
    let alice = server.human_token("alice");
    let out = fe.pm(&appsrc, &["promote", "acme/wrg", "0.3.0"], Some(&alice));
    assert!(out.status.success() && stdout(&out).contains("promote: 200"), "promote: {}", stdout(&out));

    // Install into a fresh consumer project.
    let consumer = fe.new_app();
    let out = fe.pm(&consumer, &["install", "acme/wrg", "--target", "aarch64-apple-darwin"], None);
    assert!(out.status.success(), "install failed: {}\n{}", stdout(&out), stderr(&out));
    assert!(stdout(&out).contains("installed wrg"), "install receipt: {}", stdout(&out));
    let installed = consumer.join(".witchy/bin/wrg");
    assert!(installed.exists(), "the trusted-exe must be installed into .witchy/bin");
    assert_eq!(std::fs::read(&installed).unwrap(), b"hello", "installed bytes match the published, signed artifact");

    // A source-only package has nothing to install → refused.
    let libsrc = fe.lib("acme/plain", "0.1.0", "pub fn go() -> String:\n    \"hi\"\n");
    let ci2 = server.ci_token("acme-wrg-repo", "release.yml");
    let out2 = fe.pm(&libsrc, &["publish", "."], Some(&ci2));
    assert!(out2.status.success(), "publish plain: {}", stdout(&out2));
    // alice is now the bound maintainer of namespace `acme`, so she promotes here too.
    let alice2 = server.human_token("alice");
    let out3 = fe.pm(&libsrc, &["promote", "acme/plain", "0.1.0"], Some(&alice2));
    assert!(out3.status.success(), "promote plain: {}", stdout(&out3));
    let out4 = fe.pm(&consumer, &["install", "acme/plain", "--target", "aarch64-apple-darwin"], None);
    assert!(!out4.status.success(), "installing a source-only package must fail");
    assert!(stdout(&out4).contains("source-only"), "explains source-only: {}", stdout(&out4));
}
