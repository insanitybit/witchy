use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

pub(crate) const BIN: &str = env!("CARGO_BIN_EXE_witchy");
pub(crate) const SERVER_START_ATTEMPTS: usize = 2400;
pub(crate) const SERVER_START_POLL_MS: u64 = 50;
const TEST_ROOT_SEED: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
];

fn panic_coven_startup(addr: &str, detail: &str, errlog: &Path) -> ! {
    let stderr = std::fs::read_to_string(errlog).unwrap_or_default();
    let stderr = stderr.trim();
    panic!(
        "witchy coven-serve never started listening on {addr}: {detail}{}",
        if stderr.is_empty() {
            " (no stderr captured)".to_string()
        } else {
            format!("\n--- coven-serve stderr ---\n{stderr}")
        }
    );
}

fn wait_for_coven_server(server: &mut Child, addr: &str, errlog: &Path) {
    for _ in 0..SERVER_START_ATTEMPTS {
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }
        match server.try_wait() {
            Ok(Some(status)) => panic_coven_startup(addr, &format!("child exited with {status} before binding"), errlog),
            Ok(None) => {}
            Err(e) => {
                let _ = server.kill();
                let _ = server.wait();
                panic_coven_startup(addr, &format!("cannot poll child status: {e}"), errlog);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
    }

    let _ = server.kill();
    let status = server.wait().ok();
    panic_coven_startup(
        addr,
        &format!("child did not bind within the readiness window; terminated with {status:?}"),
        errlog,
    );
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn sign_test_root(msg: &str) -> String {
    let kp = aws_lc_rs::signature::Ed25519KeyPair::from_seed_unchecked(&TEST_ROOT_SEED).unwrap();
    hex(kp.sign(msg.as_bytes()).as_ref())
}

pub(crate) fn sha256_hex(msg: &str) -> String {
    hex(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, msg.as_bytes()).as_ref())
}

pub(crate) fn signed_test_root_role(signed: &str) -> String {
    format!(r#"{{"signed":{signed},"sig":"{}"}}"#, sign_test_root(signed))
}

/// A coven registry server (the real `witchy coven-serve` binary) on a free
/// local port, for end-to-end testing over HTTP. Trusted publishing is enabled
/// with a generated issuer (the "IdP"); tests mint short-lived identity tokens.
/// Killed on drop.
pub(crate) struct RegistryServer {
    child: Child,
    pub(crate) port: u16,
    pub(crate) regroot: PathBuf,
    home: PathBuf,
    issuer_dir: PathBuf,
    /// The issuer id minted tokens carry (`iss`) — the local-IdP name for the
    /// pinned/JWKS forms, the full https issuer URL for the OIDC-discovery form.
    issuer: String,
}

const ISSUER: &str = "local-idp";

impl RegistryServer {
    /// Spawn the EMBEDDED witchy registry server (`witchy coven-serve`, running
    /// projects/coven on the interpreter — rfcs/0004-self-hosted-cli.md Phase 5).
    /// The witchy coven buffers its startup line until exit (a server never exits),
    /// so we pre-pick a free port, pass a concrete `127.0.0.1:<port>`, and poll the
    /// listener — the zero-risk pattern (no `:0`-discovery std/runtime gap).
    pub(crate) fn start() -> RegistryServer {
        let regroot = unique("coven-regroot");
        let home = unique("coven-srv-home");
        // The root signing key (a fixed test seed) — coven mints its `signing` secret.
        std::fs::create_dir_all(&regroot).unwrap();
        let seed = regroot.join("root.seed");
        std::fs::write(
            &seed,
            "0000000000000000000000000000000000000000000000000000000000000001",
        )
        .unwrap();
        // Generate the IdP signing key and capture its public key (the JWKS).
        let issuer_dir = unique("coven-issuer");
        let gen_out = Command::new(BIN)
            .args(["coven-gen-issuer", "--out", issuer_dir.to_str().unwrap()])
            .output()
            .expect("gen issuer");
        let pubhex = String::from_utf8_lossy(&gen_out.stdout).trim().to_string();

        // Pre-pick a free port (the server binds the same addr we pass).
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let addr = format!("127.0.0.1:{port}");

        // Capture stderr so the readiness helper can report a crash immediately
        // and preserve diagnostics if a still-running child times out.
        let errlog = regroot.join("coven-serve.stderr");
        let errfile = std::fs::File::create(&errlog).expect("create server stderr log");
        let mut child = Command::new(BIN)
            .args([
                "coven-serve",
                "--addr",
                &addr,
                "--root",
                regroot.to_str().unwrap(),
                "--trust-issuer",
                &format!("{ISSUER}={pubhex}"),
                "--signing-key",
                seed.to_str().unwrap(),
            ])
            .env("WITCHY_HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::from(errfile))
            .spawn()
            .expect("spawn coven-serve");

        wait_for_coven_server(&mut child, &addr, &errlog);

        RegistryServer {
            child,
            port,
            regroot,
            home,
            issuer_dir,
            issuer: ISSUER.to_string(),
        }
    }

    pub(crate) fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The registry's root public key (hex) — what an offline `pm verify-rune`
    /// pins and re-verifies a vendored rune's signed record against.
    pub(crate) fn rootpub(&self) -> String {
        let (status, body) = http_get(&format!("127.0.0.1:{}", self.port), "/coven/rootpub");
        assert_eq!(status, 200, "rootpub fetch failed");
        body.trim().to_string()
    }

    /// Mint a short-lived identity token (JSON) via the IdP key, with arbitrary
    /// claims (`key=value`).
    pub(crate) fn mint(&self, sub: &str, claims: &[(&str, &str)]) -> String {
        let mut args: Vec<String> = vec![
            "coven-mint-token".into(),
            "--issuer-key".into(),
            self.issuer_dir.to_string_lossy().into_owned(),
            "--issuer".into(),
            self.issuer.clone(),
            "--sub".into(),
            sub.into(),
        ];
        for (k, v) in claims {
            args.push("--claim".into());
            args.push(format!("{k}={v}"));
        }
        let out = Command::new(BIN).args(&args).output().expect("mint token");
        assert!(out.status.success(), "mint failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A CI (machine) identity token bound to a repository + workflow.
    pub(crate) fn ci_token(&self, repository: &str, workflow: &str) -> String {
        self.mint(
            &format!("repo:{repository}:ref:refs/heads/main"),
            &[("repository", repository), ("workflow_ref", workflow), ("ref", "refs/heads/main")],
        )
    }

    /// A human maintainer identity token (for promotion).
    pub(crate) fn human_token(&self, name: &str) -> String {
        self.mint(name, &[("amr", "webauthn")])
    }

    /// Like [`start`], but trusts the issuer via a **JWKS** document (the rotating form a
    /// real OIDC provider publishes) instead of a single pinned key. The issuer's public
    /// key is emitted as a JWKS under `kid`, written to a file `coven-serve
    /// --trust-issuer-jwks` reads; the verifier then selects the key by each token's `kid`.
    pub(crate) fn start_jwks(kid: &str) -> RegistryServer {
        let regroot = unique("coven-jwks-regroot");
        let home = unique("coven-jwks-home");
        std::fs::create_dir_all(&regroot).unwrap();
        let seed = regroot.join("root.seed");
        std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
        let issuer_dir = unique("coven-jwks-issuer");
        Command::new(BIN)
            .args(["coven-gen-issuer", "--out", issuer_dir.to_str().unwrap()])
            .output()
            .expect("gen issuer");
        // Publish the issuer's one key as a JWKS under `kid`, into a file the server reads.
        let jwks = Command::new(BIN)
            .args(["coven-issuer-jwks", "--issuer-key", issuer_dir.to_str().unwrap(), "--kid", kid])
            .output()
            .expect("issuer jwks");
        assert!(jwks.status.success(), "issuer-jwks failed: {}", String::from_utf8_lossy(&jwks.stderr));
        let jwks_path = regroot.join("jwks.json");
        std::fs::write(&jwks_path, &jwks.stdout).unwrap();

        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let child = Command::new(BIN)
            .args([
                "coven-serve",
                "--addr",
                &addr,
                "--root",
                regroot.to_str().unwrap(),
                "--trust-issuer-jwks",
                &format!("{ISSUER}={}", jwks_path.to_str().unwrap()),
                "--signing-key",
                seed.to_str().unwrap(),
            ])
            .env("WITCHY_HOME", &home)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn coven-serve");
        let mut up = false;
        for _ in 0..SERVER_START_ATTEMPTS {
            if std::net::TcpStream::connect(&addr).is_ok() {
                up = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
        }
        assert!(up, "witchy coven-serve (jwks) never started listening on {addr}");
        RegistryServer { child, port, regroot, home, issuer_dir, issuer: ISSUER.to_string() }
    }

    /// Like [`start_jwks`], but the registry trusts the issuer via **live OIDC
    /// discovery** (`--trust-issuer-oidc <issuer-url>`, RFC-0116 track 2): a
    /// loopback rustls "IdP" (self-signed `localhost` cert, trusted through
    /// `WITCHY_TLS_EXTRA_ROOTS`) serves `/.well-known/openid-configuration`
    /// pointing at its same-origin `/jwks`, and coven-serve fetches both over
    /// HTTPS at startup — before binding — installing the JWKS for the issuer
    /// URL. Minted tokens carry that URL as `iss`.
    pub(crate) fn start_oidc(kid: &str) -> RegistryServer {
        use std::io::{Read, Write};
        use std::sync::Arc;
        let regroot = unique("coven-oidc-regroot");
        let home = unique("coven-oidc-home");
        std::fs::create_dir_all(&regroot).unwrap();
        let seed = regroot.join("root.seed");
        std::fs::write(&seed, "0000000000000000000000000000000000000000000000000000000000000001").unwrap();
        let issuer_dir = unique("coven-oidc-issuer");
        Command::new(BIN)
            .args(["coven-gen-issuer", "--out", issuer_dir.to_str().unwrap()])
            .output()
            .expect("gen issuer");
        let jwks = Command::new(BIN)
            .args(["coven-issuer-jwks", "--issuer-key", issuer_dir.to_str().unwrap(), "--kid", kid])
            .output()
            .expect("issuer jwks");
        assert!(jwks.status.success(), "issuer-jwks failed: {}", String::from_utf8_lossy(&jwks.stderr));
        let jwks_body = String::from_utf8(jwks.stdout).expect("jwks utf8");

        // The loopback HTTPS IdP (the coven_web mock-GitHub pattern).
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
        let idp_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let idp_port = idp_listener.local_addr().unwrap().port();
        let issuer_url = format!("https://localhost:{idp_port}");
        let cert_path = regroot.join("idp-cert.pem");
        std::fs::write(&cert_path, ck.cert.pem()).unwrap();
        let discovery_body = format!("{{\"jwks_uri\":\"{issuer_url}/jwks\"}}");
        let idp = std::thread::spawn(move || {
            // Exactly two startup fetches: the discovery document, then the JWKS.
            for _ in 0..2 {
                let (tcp, _) = idp_listener.accept().unwrap();
                let conn = rustls::ServerConnection::new(tls_config.clone()).unwrap();
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
                let body = if request_line.starts_with("GET /.well-known/openid-configuration") {
                    discovery_body.clone()
                } else {
                    jwks_body.clone()
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

        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{port}");
        let errlog = regroot.join("coven-serve.stderr");
        let errfile = std::fs::File::create(&errlog).expect("create server stderr log");
        let mut child = Command::new(BIN)
            .args([
                "coven-serve",
                "--addr",
                &addr,
                "--root",
                regroot.to_str().unwrap(),
                "--trust-issuer-oidc",
                &issuer_url,
                "--signing-key",
                seed.to_str().unwrap(),
            ])
            .env("WITCHY_HOME", &home)
            .env("WITCHY_TLS_EXTRA_ROOTS", &cert_path)
            .stdout(Stdio::null())
            .stderr(Stdio::from(errfile))
            .spawn()
            .expect("spawn coven-serve (oidc)");
        wait_for_coven_server(&mut child, &addr, &errlog);
        // The server is up, so discovery completed — both IdP requests were served.
        idp.join().expect("idp thread");
        RegistryServer { child, port, regroot, home, issuer_dir, issuer: issuer_url }
    }

    pub(crate) fn issuer(&self) -> &str {
        &self.issuer
    }

    /// A CI identity token whose JWT header carries `kid` — naming which JWKS key signed
    /// it, so a rotating-key verifier can select the matching public key.
    pub(crate) fn ci_token_kid(&self, repository: &str, workflow: &str, kid: &str) -> String {
        let args: Vec<String> = vec![
            "coven-mint-token".into(),
            "--issuer-key".into(),
            self.issuer_dir.to_string_lossy().into_owned(),
            "--issuer".into(),
            self.issuer.clone(),
            "--sub".into(),
            format!("repo:{repository}:ref:refs/heads/main"),
            "--kid".into(),
            kid.into(),
            "--claim".into(),
            format!("repository={repository}"),
            "--claim".into(),
            format!("workflow_ref={workflow}"),
            "--claim".into(),
            "ref=refs/heads/main".into(),
        ];
        let out = Command::new(BIN).args(&args).output().expect("mint kid token");
        assert!(out.status.success(), "mint failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

impl Drop for RegistryServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.regroot);
        let _ = std::fs::remove_dir_all(&self.home);
        let _ = std::fs::remove_dir_all(&self.issuer_dir);
    }
}

/// Drives the embedded **witchy front-end** (`witchy pm <cmd>`, the self-hosted
/// CLI of rfcs/0004-self-hosted-cli.md) against a `RegistryServer`. The front-end
/// is the canonical client: a project-local `vendor/<name>/` + content-hash
/// `witchy.lock` (no global `WITCHY_HOME` store), and `COVEN_URL`/`COVEN_ID_TOKEN`
/// for the registry address + trusted-publishing identity. Each `FrontEnd` owns a
/// hermetic working tree under which projects/libraries are authored.
pub(crate) struct FrontEnd<'a> {
    server: &'a RegistryServer,
    pub(crate) base: PathBuf,
}

impl<'a> FrontEnd<'a> {
    pub(crate) fn new(server: &'a RegistryServer, tag: &str) -> FrontEnd<'a> {
        FrontEnd { server, base: unique(tag) }
    }

    /// Author a library rune `<dir_name>` (named `<name>`@`<version>`) with one
    /// source module; returns its directory.
    pub(crate) fn lib(&self, name: &str, version: &str, module_body: &str) -> PathBuf {
        let dir_name = name.rsplit('/').next().unwrap();
        let dir = self.base.join(dir_name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("witchy.toml"),
            format!("[rune]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
        let module = dir_name.replace('-', "_");
        std::fs::write(dir.join("src").join(format!("{module}.witchy")), module_body).unwrap();
        dir
    }

    /// Scaffold a fresh consumer app via `witchy pm new app`; returns its dir.
    pub(crate) fn new_app(&self) -> PathBuf {
        let out = self.pm(&self.base, &["new", "app"], None);
        assert!(out.status.success(), "pm new failed: {}", stderr(&out));
        self.base.join("app")
    }

    /// Run `witchy pm <args>` from `dir`, with `COVEN_URL` pointed at the server
    /// and (optionally) a `COVEN_ID_TOKEN` identity for trusted publish/promote.
    pub(crate) fn pm(&self, dir: &Path, args: &[&str], id_token: Option<&str>) -> Output {
        let mut full = vec!["pm"];
        full.extend_from_slice(args);
        let mut cmd = Command::new(BIN);
        cmd.current_dir(dir)
            .env("COVEN_URL", self.server.url())
            // Most tests publish and immediately consume; zero the staging cooldown
            // so they exercise their own subject. The cooldown has its own test that
            // overrides this (fresh_releases_cool_down_before_resolving).
            .env("WITCHY_COOLDOWN_SECS", "0")
            .args(&full);
        if let Some(t) = id_token {
            cmd.env("COVEN_ID_TOKEN", t);
        }
        cmd.output().expect("spawn witchy pm")
    }

    /// Publish + promote a library to the registry in one shot (the common case):
    /// a CI identity bound to `<namespace>-repo`/`release.yml` stages it, then a
    /// human identity (distinct, for separation of duties) promotes it to
    /// released. The repository is keyed by NAMESPACE (not the full name) so every
    /// rune under a namespace publishes from the one TOFU-bound repository.
    pub(crate) fn publish_promote(&self, dir: &Path, name: &str, version: &str) {
        let ns = name.split('/').next().unwrap();
        let repo = format!("{ns}-repo");
        let ci = self.server.ci_token(&repo, "release.yml");
        let out = self.pm(dir, &["publish", "."], Some(&ci));
        assert!(
            out.status.success() && stdout(&out).contains("publish: 200"),
            "publish failed: {}\nstdout: {}",
            stderr(&out),
            stdout(&out)
        );
        let human = self.server.human_token("alice");
        let out = self.pm(dir, &["promote", name, version], Some(&human));
        assert!(
            out.status.success() && stdout(&out).contains("promote: 200"),
            "promote failed: {}\nstdout: {}",
            stderr(&out),
            stdout(&out)
        );
    }

    /// Author + publish + promote a library in one shot; returns its dir.
    pub(crate) fn published_lib(&self, name: &str, version: &str, module_body: &str) -> PathBuf {
        let dir = self.lib(name, version, module_body);
        self.publish_promote(&dir, name, version);
        dir
    }
}

impl Drop for FrontEnd<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

pub(crate) fn unique(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("coven-e2e-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub(crate) fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
pub(crate) fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

pub(crate) fn start_basic_coven(tag: &str) -> (Child, String, PathBuf) {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let store = unique(tag);
    let seed = store.join("root.seed");
    std::fs::write(
        &seed,
        "0000000000000000000000000000000000000000000000000000000000000001",
    )
    .unwrap();

    let errlog = store.join("coven-serve.stderr");
    let errfile = std::fs::File::create(&errlog).expect("create server stderr log");
    let mut server = Command::new(BIN)
        .args([
            "coven-serve",
            "--addr",
            &addr,
            "--root",
            store.to_str().unwrap(),
            "--signing-key",
            seed.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::from(errfile))
        .spawn()
        .expect("spawn witchy coven-serve");

    wait_for_coven_server(&mut server, &addr, &errlog);

    (server, addr, store)
}

#[test]
fn coven_startup_reports_an_exited_child_without_waiting_for_timeout() {
    let dir = unique("coven-startup-exit");
    let errlog = dir.join("coven-serve.stderr");
    let errfile = std::fs::File::create(&errlog).unwrap();
    let missing = dir.join("missing.witchy");
    let mut child = Command::new(BIN)
        .arg(missing.to_str().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::from(errfile))
        .spawn()
        .unwrap();
    child.wait().unwrap();

    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for_coven_server(&mut child, "127.0.0.1:0", &errlog);
    }))
    .expect_err("an exited child must fail readiness immediately");
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic");
    assert!(message.contains("child exited with") && message.contains("missing.witchy"), "{message}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Recursively copy the contents of `src` into `dst` (used to lift a committed
/// example workspace into a hermetic sandbox without mutating the repo).
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// Lift a committed `examples/projects/<name>` workspace into a fresh, hermetic
/// temp dir (so the test never mutates the repo or its lockfiles) and return the
/// workspace root. The workspace holds the app rune and its sibling library runes;
/// `witchy pm run/build <app>` is driven from this root so the project `Dir` (the
/// front-end's handle 0) reaches both the app and its `../sibling` path deps,
/// while the program's own runtime `Dir` is rooted at the app subdir.
pub(crate) fn lift_example(name: &str) -> PathBuf {
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects").join(name);
    let work = unique(&format!("ex-{name}"));
    copy_tree(&srcroot, &work);
    work
}

/// Drive the embedded **witchy front-end** (`witchy pm <args>`) from `dir`, the
/// self-hosted CLI of rfcs/0004-self-hosted-cli.md. Capability-confined: the
/// project `Dir` is `dir`, a `Dir` to the toolchain bin lets it drive the compiler
/// via `Exec`. No registry server is needed for a path-dependency workspace — the
/// sources resolve straight from the manifests' `path =`.
pub(crate) fn pm_fe(dir: &Path, args: &[&str]) -> Output {
    let mut full = vec!["pm"];
    full.extend_from_slice(args);
    Command::new(BIN)
        .current_dir(dir)
        .args(&full)
        .output()
        .expect("spawn witchy pm")
}

fn read_http_response(
    stream: &mut std::net::TcpStream,
    poll_timeout: std::time::Duration,
    total_timeout: std::time::Duration,
) -> String {
    use std::io::{ErrorKind, Read};

    stream.set_read_timeout(Some(poll_timeout)).unwrap();
    let deadline = std::time::Instant::now() + total_timeout;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "HTTP fixture response exceeded its total timeout after {} bytes",
            bytes.len()
        );
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e)
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
                    && std::time::Instant::now() < deadline =>
            {
                continue;
            }
            Err(e) => panic!("HTTP fixture response stopped after {} bytes: {e}", bytes.len()),
        }
    }
    String::from_utf8(bytes).expect("HTTP fixture response is UTF-8")
}

fn parse_http_response(raw: String) -> (u16, String) {
    let (headers, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("HTTP fixture response has no header terminator: {raw:?}"));
    let status = headers
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("HTTP fixture response has no valid status: {headers:?}"));
    if let Some(expected) = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().expect("valid Content-Length"))
    }) {
        assert_eq!(
            body.len(),
            expected,
            "HTTP fixture response body is incomplete; refusing to mirror partial JSON"
        );
    }
    (status, body.to_string())
}

/// Minimal HTTP/1.1 POST over a raw TCP socket (the test client). Returns
/// (status code, body).
pub(crate) fn http_post(addr: &str, path: &str, body: &str) -> (u16, String) {
    use std::io::Write;
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    parse_http_response(read_http_response(
        &mut s,
        std::time::Duration::from_millis(250),
        std::time::Duration::from_secs(10),
    ))
}

/// GET a path from a coven server, returning (status, body).
pub(crate) fn http_get(addr: &str, path: &str) -> (u16, String) {
    use std::io::Write;
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    parse_http_response(read_http_response(
        &mut s,
        std::time::Duration::from_millis(250),
        std::time::Duration::from_secs(10),
    ))
}

#[test]
fn raw_http_reader_reassembles_a_body_across_socket_timeouts() {
    use std::io::Write;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello")
            .unwrap();
        stream.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        stream.write_all(b" world").unwrap();
    });

    let mut client = std::net::TcpStream::connect(addr).unwrap();
    let response = read_http_response(
        &mut client,
        std::time::Duration::from_millis(100),
        std::time::Duration::from_secs(2),
    );
    assert_eq!(parse_http_response(response), (200, "hello world".to_string()));
    server.join().unwrap();
}
