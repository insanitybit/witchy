//! End-to-end tests for coven, the witchy package manager. These drive the real
//! `witchy` binary (via `CARGO_BIN_EXE_witchy`) through the full supply-chain
//! lifecycle: scaffold, publish (staged), promote (second factor), add (gated),
//! build, run, audit. Each test is hermetic — its own temp `WITCHY_HOME` and
//! working tree — so they can run in parallel.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const SERVER_START_ATTEMPTS: usize = 2400;
const SERVER_START_POLL_MS: u64 = 50;
const TEST_ROOT_SEED: [u8; 32] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
];

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

fn sha256_hex(msg: &str) -> String {
    hex(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, msg.as_bytes()).as_ref())
}

fn signed_test_root_role(signed: &str) -> String {
    format!(r#"{{"signed":{signed},"sig":"{}"}}"#, sign_test_root(signed))
}

/// A coven registry server (the real `witchy coven-serve` binary) on a free
/// local port, for end-to-end testing over HTTP. Trusted publishing is enabled
/// with a generated issuer (the "IdP"); tests mint short-lived identity tokens.
/// Killed on drop.
struct RegistryServer {
    child: Child,
    port: u16,
    regroot: PathBuf,
    home: PathBuf,
    issuer_dir: PathBuf,
}

const ISSUER: &str = "local-idp";

impl RegistryServer {
    /// Spawn the EMBEDDED witchy registry server (`witchy coven-serve`, running
    /// projects/coven on the interpreter — rfcs/0004-self-hosted-cli.md Phase 5).
    /// The witchy coven buffers its startup line until exit (a server never exits),
    /// so we pre-pick a free port, pass a concrete `127.0.0.1:<port>`, and poll the
    /// listener — the zero-risk pattern (no `:0`-discovery std/runtime gap).
    fn start() -> RegistryServer {
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

        let child = Command::new(BIN)
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
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn coven-serve");

        // Wait for the listener to come up.
        let mut up = false;
        for _ in 0..SERVER_START_ATTEMPTS {
            if std::net::TcpStream::connect(&addr).is_ok() {
                up = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(SERVER_START_POLL_MS));
        }
        assert!(up, "witchy coven-serve never started listening on {addr}");

        RegistryServer {
            child,
            port,
            regroot,
            home,
            issuer_dir,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The registry's root public key (hex) — what an offline `pm verify-rune`
    /// pins and re-verifies a vendored rune's signed record against.
    fn rootpub(&self) -> String {
        let (status, body) = http_get(&format!("127.0.0.1:{}", self.port), "/coven/rootpub");
        assert_eq!(status, 200, "rootpub fetch failed");
        body.trim().to_string()
    }

    /// Mint a short-lived identity token (JSON) via the IdP key, with arbitrary
    /// claims (`key=value`).
    fn mint(&self, sub: &str, claims: &[(&str, &str)]) -> String {
        let mut args: Vec<String> = vec![
            "coven-mint-token".into(),
            "--issuer-key".into(),
            self.issuer_dir.to_string_lossy().into_owned(),
            "--issuer".into(),
            ISSUER.into(),
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
    fn ci_token(&self, repository: &str, workflow: &str) -> String {
        self.mint(
            &format!("repo:{repository}:ref:refs/heads/main"),
            &[("repository", repository), ("workflow_ref", workflow), ("ref", "refs/heads/main")],
        )
    }

    /// A human maintainer identity token (for promotion).
    fn human_token(&self, name: &str) -> String {
        self.mint(name, &[])
    }

    /// Like [`start`], but trusts the issuer via a **JWKS** document (the rotating form a
    /// real OIDC provider publishes) instead of a single pinned key. The issuer's public
    /// key is emitted as a JWKS under `kid`, written to a file `coven-serve
    /// --trust-issuer-jwks` reads; the verifier then selects the key by each token's `kid`.
    fn start_jwks(kid: &str) -> RegistryServer {
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
        RegistryServer { child, port, regroot, home, issuer_dir }
    }

    /// A CI identity token whose JWT header carries `kid` — naming which JWKS key signed
    /// it, so a rotating-key verifier can select the matching public key.
    fn ci_token_kid(&self, repository: &str, workflow: &str, kid: &str) -> String {
        let args: Vec<String> = vec![
            "coven-mint-token".into(),
            "--issuer-key".into(),
            self.issuer_dir.to_string_lossy().into_owned(),
            "--issuer".into(),
            ISSUER.into(),
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
struct FrontEnd<'a> {
    server: &'a RegistryServer,
    base: PathBuf,
}

impl<'a> FrontEnd<'a> {
    fn new(server: &'a RegistryServer, tag: &str) -> FrontEnd<'a> {
        FrontEnd { server, base: unique(tag) }
    }

    /// Author a library rune `<dir_name>` (named `<name>`@`<version>`) with one
    /// source module; returns its directory.
    fn lib(&self, name: &str, version: &str, module_body: &str) -> PathBuf {
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
    fn new_app(&self) -> PathBuf {
        let out = self.pm(&self.base, &["new", "app"], None);
        assert!(out.status.success(), "pm new failed: {}", stderr(&out));
        self.base.join("app")
    }

    /// Run `witchy pm <args>` from `dir`, with `COVEN_URL` pointed at the server
    /// and (optionally) a `COVEN_ID_TOKEN` identity for trusted publish/promote.
    fn pm(&self, dir: &Path, args: &[&str], id_token: Option<&str>) -> Output {
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
    fn publish_promote(&self, dir: &Path, name: &str, version: &str) {
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
    fn published_lib(&self, name: &str, version: &str, module_body: &str) -> PathBuf {
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

fn unique(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("coven-e2e-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn start_basic_coven(tag: &str) -> (Child, String, PathBuf) {
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
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn witchy coven-serve");

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
        panic!("witchy coven-serve never started on {addr}");
    }

    (server, addr, store)
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
fn lift_example(name: &str) -> PathBuf {
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
fn pm_fe(dir: &Path, args: &[&str]) -> Output {
    let mut full = vec!["pm"];
    full.extend_from_slice(args);
    Command::new(BIN)
        .current_dir(dir)
        .args(&full)
        .output()
        .expect("spawn witchy pm")
}

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

    // A distinct human promotes it to released.
    let alice = server.human_token("alice");
    let out = fe.pm(&lib, &["promote", "acme/secure", "1.1.0"], Some(&alice));
    assert!(out.status.success() && stdout(&out).contains("promote: 200"), "human promote: {}", stdout(&out));
}

/// SEC-018: on a trusted registry, `yank` requires an EXISTING maintainer of the
/// namespace — it is not an unauthenticated operation. A non-maintainer token is
/// refused 403; the maintainer (the human who promoted, bound TOFU) may yank.
#[test]
fn trusted_yank_requires_a_maintainer() {
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

/// Trusted publishing against a ROTATING-key issuer: the registry trusts a JWKS document
/// (not a single pinned key) and selects the verifying key by each token's `kid` — exactly
/// what verifying a real GitHub Actions OIDC token requires, since GitHub rotates its
/// signing keys. A token whose `kid` is present in the JWKS verifies and publishes; a token
/// whose `kid` is absent (a key that rotated away / was never published) is refused 401,
/// even though it is otherwise a well-formed token from the trusted issuer.
#[test]
fn trusted_publishing_verifies_a_jwks_issuer_by_kid() {
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

/// The registry generates browsable API docs on demand: `GET /coven/doc` renders the
/// published rune's stored source to the same Markdown `witchy doc` emits (types and
/// public functions with their doc-comments). This is safe on untrusted published code
/// because `compiler.doc` only PARSES the source — it never runs it — and the source is
/// hash-verified against the signed record before rendering.
#[test]
fn coven_serves_generated_api_docs() {
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

/// The registry audits the FOREIGN-CODE compartments a package embeds (RFC-0015): GET
/// /coven/compartments scans the published source for `compartment("<id>"` call sites and
/// reports the renderer ids — the `Js` governance signal ("what third-party code does this
/// package run?"), surfaced at the registry layer (not the compiler). A package that
/// embeds none reports none.
#[test]
fn coven_audits_embedded_compartments() {
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

/// A trusted registry requires a valid identity token to publish: an anonymous
/// publish (no `COVEN_ID_TOKEN`) is refused 401, and a token from an UNTRUSTED
/// issuer (a rogue IdP whose key the registry doesn't list) is also refused 401.
/// The front-end forwards whatever token the environment provides; the server is
/// the gate.
#[test]
fn token_required_and_untrusted_issuer_refused() {
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

/// The front-end verifies the registry's TUF chain on `add` and re-verifies it on
/// `verify`. `add` pins the registry's signed snapshot version into the lock; a
/// later `verify` re-fetches the signed snapshot + timestamp roles and checks the
/// whole chain against the root key. Tampering a signed field of the server's
/// snapshot breaks its root-key signature, so a fresh `verify` rejects it — the
/// signature + content binding, not the transport, are trusted.
#[test]
fn tuf_chain_verified_and_snapshot_tamper_rejected() {
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
    let out = fe.pm(&app, &["verify"], None);
    assert!(out.status.success(), "verify failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("TUF chain"), "verify out: {}", stdout(&out));

    // Tamper the SERVER's signed snapshot (changing a signed field breaks the
    // snapshot-role signature). The front-end vendors, so there is no client cache
    // to clear — `verify` re-fetches the role and must reject the broken signature.
    let snap = server.regroot.join("registry/snapshot.json");
    let body = std::fs::read_to_string(&snap).unwrap().replace("1.0.0", "1.0.1");
    std::fs::write(&snap, body).unwrap();

    let out = fe.pm(&app, &["verify"], None);
    assert!(!out.status.success(), "tampered snapshot must fail verify");
    assert!(stdout(&out).contains("FAIL"), "verify out: {}", stdout(&out));
}

/// BUG-386: a TUF role can be validly signed yet structurally incomplete. The
/// verifier must reject that before old defaulting helpers can turn absent fields
/// into `0`, `""`, or `JsonNull`.
#[test]
fn tuf_chain_rejects_validly_signed_malformed_roles() {
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

    let out = fe.pm(&app, &["verify"], None);
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

    let out = fe.pm(&app, &["verify"], None);
    assert!(!out.status.success(), "malformed timestamp must fail verify");
    assert!(
        stdout(&out).contains("timestamp role is structurally malformed"),
        "out: {}",
        stdout(&out)
    );

    std::fs::write(&ts_path, original_timestamp).unwrap();
}

/// A registry rollback — serving an OLDER TUF snapshot version than the one the
/// project last pinned — is refused. We simulate having seen a much newer snapshot
/// by bumping the lock's pinned `registry_snapshot_version`; the server's actual
/// (older) snapshot version is now below the pin, which `verify` rejects.
#[test]
fn tuf_rollback_is_rejected() {
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

    let out = fe.pm(&app, &["verify"], None);
    assert!(!out.status.success(), "rollback must be refused");
    assert!(
        stdout(&out).contains("rolled back") || stdout(&out).contains("rollback"),
        "out: {}",
        stdout(&out)
    );
}

/// The front-end refuses a tampered registry record via its Ed25519 signature.
/// The SLSA provenance attestation is part of the signed record, so editing it on
/// the server (`trusted-publisher` → `evil-publisher`) breaks the root-key
/// signature. A fresh `pm add` fetches the tampered record and rejects it — the
/// content address + the signature, not the transport, are trusted.
#[test]
fn networked_registry_signature_detects_tampering() {
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
        "import strkit\n\nfn main(console: Console):\n    print(console, strkit.shout(\"witchy\"))\n",
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
    assert!(out.status.success(), "healthy TUF verify failed: {}", stdout(&out));

    let meta = app.join("vendor/netcap/coven.json");
    let json = std::fs::read_to_string(&meta)
        .unwrap()
        .replace("\"runtime_footprint\":[\"Net[Connect, Tcp]\"]", "\"runtime_footprint\":[\"Net[Connect\",\" Tcp]\"]");
    std::fs::write(&meta, json).unwrap();

    let out = fe.pm(&app, &["verify-rune", "vendor/netcap", &rootpub], None);
    assert!(!out.status.success(), "shape-tampered footprint must fail verify");
    assert!(stdout(&out).contains("BLOCK"), "verify-rune: {}", stdout(&out));
    let out = fe.pm(&app, &["verify"], None);
    assert!(!out.status.success(), "shape-tampered footprint must fail TUF verify");
    assert!(stdout(&out).contains("vendored record digest does not match"), "verify: {}", stdout(&out));
}

/// A fetched rune carries its signed provenance and re-verifies offline: `pm add`
/// vendors `coven.json` (the registry-root-signed record) beside the source and
/// pins the content hash in `witchy.lock`; `pm verify-rune <dir> <rootpub>` then
/// re-checks the signature + content hash with NO network. The front-end pins
/// trust in the signed record + content address, not a lockfile key fingerprint.
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

    // Tampering the vendored source breaks the content check.
    std::fs::write(app.join("vendor/yankee/src/yankee.witchy"), "pub fn f(s: String) -> String:\n    \"evil\"\n").unwrap();
    let out = fe.pm(&app, &["verify-rune", "vendor/yankee", &rootpub], None);
    assert!(!out.status.success(), "tampered source must fail re-verification");
    assert!(stdout(&out).contains("BLOCK"), "verify-rune: {}", stdout(&out));
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
        "import list\n\nfn main(console: Console):\n    print(console, \"x\")\n",
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
            "import lib\n\nfn main(console: Console):\n    print(console, lib.f(\"vend\"))\n",
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
        "import greet\n\nfn main(console: Console):\n    print(console, greet.hi(\"witchy\"))\n",
    )
    .unwrap();

    // Run from the workspace root so the project Dir reaches the `../greet` sibling.
    let out = pm_fe(&work, &["run", "app"]);
    assert!(out.status.success(), "pm run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("hi witchy"), "got: {}", stdout(&out));

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
        "fn build(out: BuildOut):\n    write_out(out, \"gen.witchy\", \"// generated\")\n",
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
        "fn main(console: Console):\n    print(console, \"ok\")\n",
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
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    write_out(out, \"greet.witchy\", \"pub fn greeting() -> String:\" + nl + \"    \\\"hi from generated code\\\"\" + nl)\n",
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
        "import greet\n\nfn main(console: Console):\n    print(console, greet.greeting())\n",
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
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    write_out(out, \"greet.witchy\", \"pub fn evil(n: Net, addr: String) -> Socket:\" + nl + \"    connect(n, addr)\" + nl)\n",
    )
    .unwrap();
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
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    write_out(out, \"greet.witchy\", \"pub fn greeting() -> String:\" + nl + \"    \\\"V1\\\"\" + nl)\n",
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
        "import greet\n\nfn main(console: Console):\n    print(console, greet.greeting())\n",
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

/// Minimal HTTP/1.1 POST over a raw TCP socket (the test client). Returns
/// (status code, body).
fn http_post(addr: &str, path: &str, body: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

/// GET a path from a coven server, returning (status, body).
fn http_get(addr: &str, path: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).unwrap();
    let mut buf = String::new();
    let _ = s.read_to_string(&mut buf);
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or(0);
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

/// GET a path and return the WHOLE raw HTTP response (status line + headers + body), so
/// a test can read a redirect's `Location` header.
/// The `Rand` capability: `rand_u64(rand)` draws from the OS CSPRNG, but under
/// `WITCHY_RAND_SEED` both backends draw the SAME deterministic splitmix sequence — so a
/// program using randomness stays parity-stable and reproducible for tests.
#[test]
fn rand_capability_seeds_deterministically_and_agrees_across_backends() {
    let dir = unique("witchy-rand");
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("rand.witchy");
    std::fs::write(
        &src,
        "fn main(console: Console, rand: Rand):\n    print(console, __render(rand_u64(rand)))\n    print(console, __render(rand_u64(rand)))\n",
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
        let payload = format!(
            r#"{{"iss":"https://accounts.google.com","aud":"gClientID","email":"alice@example.com","email_verified":true,"sub":"1","exp":2000000000,"nbf":0,"nonce":"{nonce}"}}"#
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
    let (mut server, addr, store) = start_basic_coven("witchy-pmadd-store");

    // Publish + promote 1.0.0 and 1.5.0; publish 2.0.0 but leave it STAGED.
    let publish = |version: &str| {
        let manifest = format!("[rune]\nname = \"acme/money\"\nversion = \"{version}\"\n");
        let module = format!("fn ver() -> String:\n    \"{version}\"\n");
        let source = format!(
            "{{\"files\":[[\"witchy.toml\",{}],[\"src/money.witchy\",{}]]}}",
            json_str(&manifest),
            json_str(&module)
        );
        let body = format!(
            "{{\"manifest_toml\":{},\"source\":{source},\"uploaded_by\":\"ci\"}}",
            json_str(&manifest)
        );
        http_post(&addr, "/coven/publish", &body)
    };
    let promote = |version: &str| {
        let body = format!(
            "{{\"name\":\"acme~money\",\"version\":\"{version}\",\"second_factor\":\"webauthn\",\"promoted_by\":\"human\"}}"
        );
        http_post(&addr, "/coven/promote", &body)
    };
    for v in ["1.0.0", "1.5.0"] {
        assert_eq!(publish(v).0, 200, "publish {v}");
        assert_eq!(promote(v).0, 200, "promote {v}");
    }
    assert_eq!(publish("2.0.0").0, 200, "publish 2.0.0 (staged)");

    // `pm add acme/money "*" <addr> vendor` (cwd = dest, so the Dir is confined).
    let dest = unique("witchy-pmadd-dest");
    let out = Command::new(BIN)
        .args([
            "pm",
            "--net",
            &addr,
            "add",
            "acme/money",
            "*",
            &addr,
            "vendor",
        ])
        .current_dir(&dest)
        .env("WITCHY_COOLDOWN_SECS", "0")
        .output()
        .expect("run pm add");

    let _ = server.kill();
    let _ = server.wait();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let materialized = std::fs::read_to_string(dest.join("vendor/money/src/money.witchy"))
        .unwrap_or_default();
    // The signed record is kept next to the source for offline re-verification.
    let provenance = std::fs::read_to_string(dest.join("vendor/money/coven.json"))
        .unwrap_or_default();

    // The registry is now dead — `verify-rune` re-verifies the vendored rune with
    // no network, using its coven.json and the pinned registry root key.
    let rootpub = "4cb5abf6ad79fbf5abbccafcc269d85cd2651ed4b885b5869f241aedf0a5ba29";
    let verify = Command::new(BIN)
        .args(["pm", "verify-rune", "vendor/money", rootpub])
        .current_dir(&dest)
        .output()
        .expect("run pm verify-rune");
    let verify_out = String::from_utf8_lossy(&verify.stdout).to_string();

    // Tamper with the vendored source bytes: the offline re-verification must
    // BLOCK on the signed-hash mismatch (a compromised mirror or local edit).
    std::fs::write(
        dest.join("vendor/money/src/money.witchy"),
        "fn ver() -> String:\n    \"evil\"\n",
    )
    .unwrap();
    let tampered = Command::new(BIN)
        .args(["pm", "verify-rune", "vendor/money", rootpub])
        .current_dir(&dest)
        .output()
        .expect("run pm verify-rune on tampered source");
    let tampered_out = String::from_utf8_lossy(&tampered.stdout).to_string();

    let _ = std::fs::remove_dir_all(&store);
    let _ = std::fs::remove_dir_all(&dest);

    // `*` resolves to 1.5.0 — the highest RELEASED version (2.0.0 is staged).
    assert!(
        stdout.contains("added acme/money@1.5.0 -> vendor/money"),
        "pm add should resolve the highest released version: {stdout:?} / {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        materialized.contains("\"1.5.0\""),
        "the fetched source must be 1.5.0, got: {materialized:?}"
    );
    assert!(
        provenance.contains("\"version\":\"1.5.0\"") && provenance.contains("\"sig\":"),
        "the vendored rune must carry its signed coven.json record: {provenance:?}"
    );
    assert!(
        verify.status.success() && verify_out.contains("acme/money@1.5.0 verified"),
        "offline verify-rune must re-verify the vendored rune: {verify_out:?}"
    );
    assert_eq!(
        tampered.status.code(),
        Some(2),
        "verify-rune must exit 2 on tampered source: {tampered_out:?}"
    );
    assert!(
        tampered_out.contains("no longer matches its signed hash (tampered)"),
        "verify-rune must name the tamper: {tampered_out:?}"
    );
}

/// Self-hosted yank: the witchy coven marks a version yanked and the witchy pm's
/// resolver skips it. With 1.0.0 and 2.0.0 both released, `*` resolves to 2.0.0;
/// after 2.0.0 is yanked, `*` falls back to 1.0.0 — a yanked version is excluded
/// from new resolutions (existing locks would still pin it). Once EVERY version
/// is yanked, `add` refuses outright (no non-yanked released version remains),
/// and `pm list` reflects the yanked lifecycle state.
#[test]
fn witchy_coven_yank_excludes_from_resolution() {
    let (mut server, addr, store) = start_basic_coven("witchy-yank-store");

    let module = "fn ver() -> String:\n    \"x\"\n";
    let publish = |version: &str| {
        let manifest = format!("[rune]\nname = \"acme/money\"\nversion = \"{version}\"\n");
        let source = format!(
            "{{\"files\":[[\"witchy.toml\",{}],[\"src/money.witchy\",{}]]}}",
            json_str(&manifest),
            json_str(module)
        );
        let body = format!(
            "{{\"manifest_toml\":{},\"source\":{source},\"uploaded_by\":\"ci\"}}",
            json_str(&manifest)
        );
        http_post(&addr, "/coven/publish", &body)
    };
    let promote = |version: &str| {
        let body = format!(
            "{{\"name\":\"acme~money\",\"version\":\"{version}\",\"second_factor\":\"webauthn\",\"promoted_by\":\"human\"}}"
        );
        http_post(&addr, "/coven/promote", &body)
    };
    for v in ["1.0.0", "2.0.0"] {
        assert_eq!(publish(v).0, 200, "publish {v}");
        assert_eq!(promote(v).0, 200, "promote {v}");
    }

    let add_star = || {
        let dest = unique("witchy-yank-dest");
        let out = Command::new(BIN)
            .args(["pm", "--net", &addr, "add", "acme/money", "*", &addr, "vendor"])
            .env("WITCHY_COOLDOWN_SECS", "0")
            .current_dir(&dest)
            .output()
            .expect("run pm add");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let _ = std::fs::remove_dir_all(&dest);
        (out.status.success(), stdout)
    };

    // Both released: `*` resolves to the highest, 2.0.0.
    let before = add_star();
    // Yank 2.0.0, then `*` must fall back to 1.0.0.
    let (yank_status, yank_body) =
        http_post(&addr, "/coven/yank", "{\"name\":\"acme~money\",\"version\":\"2.0.0\"}");
    let after = add_star();
    // Yank 1.0.0 as well: with no non-yanked released version left, `add` must refuse.
    let (yank2_status, yank2_body) =
        http_post(&addr, "/coven/yank", "{\"name\":\"acme~money\",\"version\":\"1.0.0\"}");
    let exhausted = add_star();
    // `pm list` reflects the yanked lifecycle state.
    let list = Command::new(BIN)
        .args(["pm", "--net", &addr, "list", "acme/money", &addr])
        .current_dir(&store)
        .output()
        .expect("run pm list");
    let list_out = String::from_utf8_lossy(&list.stdout).to_string();

    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&store);

    assert!(
        before.0 && before.1.contains("added acme/money@2.0.0"),
        "before yank, * resolves to 2.0.0: {before:?}"
    );
    assert_eq!(yank_status, 200, "yank should succeed: {yank_body}");
    assert!(
        after.0 && after.1.contains("added acme/money@1.0.0"),
        "after yank, * falls back to 1.0.0: {after:?}"
    );
    assert_eq!(yank2_status, 200, "second yank should succeed: {yank2_body}");
    assert!(
        !exhausted.0,
        "with every version yanked, add must be refused: {exhausted:?}"
    );
    assert!(list_out.contains("yanked"), "list must reflect the yanked state: {list_out}");
}

/// Transitive resolution, self-hosted: publishing `acme/app` whose manifest
/// declares a version dependency on `acme/util`, then `pm add acme/app` fetches
/// BOTH — app and, by following app's `[dependencies]`, util. Each node is
/// integrity-verified and carries its coven.json.
#[test]
fn witchy_pm_add_resolves_transitive_dependencies() {
    let (mut server, addr, store) = start_basic_coven("witchy-trans-store");

    let publish = |name: &str, stem: &str, manifest: &str, module: &str| {
        let source = format!(
            "{{\"files\":[[\"witchy.toml\",{}],[\"src/{stem}.witchy\",{}]]}}",
            json_str(manifest),
            json_str(module)
        );
        let body = format!(
            "{{\"manifest_toml\":{},\"source\":{source},\"uploaded_by\":\"ci\"}}",
            json_str(manifest)
        );
        assert_eq!(http_post(&addr, "/coven/publish", &body).0, 200, "publish {name}");
        let promote = format!(
            "{{\"name\":\"{}\",\"version\":\"1.0.0\",\"second_factor\":\"webauthn\",\"promoted_by\":\"human\"}}",
            name.replace('/', "~")
        );
        assert_eq!(http_post(&addr, "/coven/promote", &promote).0, 200, "promote {name}");
    };

    publish(
        "acme/util",
        "util",
        "[rune]\nname = \"acme/util\"\nversion = \"1.0.0\"\n",
        "fn id(s: String) -> String:\n    s\n",
    );
    publish(
        "acme/app",
        "app",
        "[rune]\nname = \"acme/app\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/util\" = { version = \"^1.0.0\" }\n",
        "fn run() -> String:\n    \"app\"\n",
    );

    let dest = unique("witchy-trans-dest");
    let out = Command::new(BIN)
        .args(["pm", "--net", &addr, "add", "acme/app", "*", &addr, "vendor"])
        .env("WITCHY_COOLDOWN_SECS", "0")
        .current_dir(&dest)
        .output()
        .expect("run pm add");

    let _ = server.kill();
    let _ = server.wait();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let app_present = dest.join("vendor/app/coven.json").exists();
    let util_present = dest.join("vendor/util/coven.json").exists();

    // The whole vendored tree re-verifies offline (the registry is now down).
    let rootpub = "4cb5abf6ad79fbf5abbccafcc269d85cd2651ed4b885b5869f241aedf0a5ba29";
    let vverify = Command::new(BIN)
        .args(["pm", "verify-vendor", "vendor", rootpub])
        .current_dir(&dest)
        .output()
        .expect("run pm verify-vendor");
    let vverify_out = String::from_utf8_lossy(&vverify.stdout).to_string();

    let _ = std::fs::remove_dir_all(&store);
    let _ = std::fs::remove_dir_all(&dest);

    assert!(
        stdout.contains("added acme/app@1.0.0"),
        "app must be fetched: {stdout:?} / {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("added acme/util@1.0.0"),
        "the transitive dep util must be fetched: {stdout:?}"
    );
    assert!(app_present && util_present, "both runes must carry provenance");
    assert!(
        vverify.status.success() && vverify_out.contains("all 2 vendored runes verified"),
        "verify-vendor must re-verify the whole tree offline: {vverify_out:?}"
    );
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
    let work = unique("witchy-pm-local");
    let pm = |args: &[&str]| {
        let mut full = vec!["pm"];
        full.extend_from_slice(args);
        Command::new(BIN)
            .args(&full)
            .current_dir(&work)
            .output()
            .expect("run pm")
    };

    // Scaffold the app and its dependency, then declare the path dep.
    let new_app = pm(&["new", "app"]);
    let new_util = pm(&["new", "util"]);
    assert!(
        new_app.status.success() && stdout(&new_app).contains("created rune `app`"),
        "pm new app: {:?} / {:?}",
        stdout(&new_app),
        stderr(&new_app)
    );
    assert!(new_util.status.success(), "pm new util: {:?}", stderr(&new_util));
    let manifest_path = work.join("app/witchy.toml");
    let manifest = std::fs::read_to_string(&manifest_path).unwrap();
    std::fs::write(
        &manifest_path,
        manifest.replace("[dependencies]\n", "[dependencies]\nutil = { path = \"../util\" }\n"),
    )
    .unwrap();

    // info + deps see the declared dependency.
    let info = pm(&["info", "app"]);
    assert!(
        stdout(&info).contains("name:     app") && stdout(&info).contains("version:  0.1.0"),
        "pm info: {:?}",
        stdout(&info)
    );
    let deps = pm(&["deps", "app"]);
    assert!(
        stdout(&deps).contains("util -> path:../util"),
        "pm deps must show the path dep: {:?}",
        stdout(&deps)
    );

    // lock pins the dependency; verify and gate agree with the fresh pin.
    let lock = pm(&["lock", "app"]);
    assert!(
        lock.status.success() && stdout(&lock).contains("locked 1 dependencies"),
        "pm lock: {:?} / {:?}",
        stdout(&lock),
        stderr(&lock)
    );
    let lockfile = std::fs::read_to_string(work.join("app/witchy.lock")).unwrap();
    assert!(
        lockfile.contains("name = \"util\"") && lockfile.contains("hash = \"sha256:"),
        "the lockfile must pin util by content hash: {lockfile:?}"
    );
    let verify_ok = pm(&["verify", "app"]);
    assert!(
        verify_ok.status.success()
            && stdout(&verify_ok).contains("OK: every locked hash matches"),
        "pm verify on a fresh lock: {:?}",
        stdout(&verify_ok)
    );
    let gate_ok = pm(&["gate", "app"]);
    assert!(
        gate_ok.status.success() && stdout(&gate_ok).contains("OK: dependencies demand no authority"),
        "pm gate on a fresh lock: {:?}",
        stdout(&gate_ok)
    );

    // Tamper: the dependency now also demands Net.
    std::fs::write(
        work.join("util/src/util.witchy"),
        "fn main(console: Console):\n    print(console, \"hello from util\")\n\npub fn fetch(net: Net) -> Int:\n    0\n",
    )
    .unwrap();
    let verify_bad = pm(&["verify", "app"]);
    assert_eq!(
        verify_bad.status.code(),
        Some(2),
        "verify must exit 2 on changed bytes: {:?}",
        stdout(&verify_bad)
    );
    assert!(
        stdout(&verify_bad).contains("BLOCK: lock no longer matches source for: util"),
        "verify must name the tampered dep: {:?}",
        stdout(&verify_bad)
    );
    let gate_bad = pm(&["gate", "app"]);
    assert_eq!(
        gate_bad.status.code(),
        Some(2),
        "gate must exit 2 on widened authority: {:?}",
        stdout(&gate_bad)
    );
    assert!(
        stdout(&gate_bad).contains("BLOCK: dependencies demand new authority: Net")
            && stdout(&gate_bad).contains("Net <- util"),
        "gate must name the new capability and its contributor: {:?}",
        stdout(&gate_bad)
    );

    // Explicit consent (like --allow-cap) folds Net into the baseline.
    let gate_allowed = pm(&["gate", "app", "Net"]);
    let _ = std::fs::remove_dir_all(&work);
    assert!(
        gate_allowed.status.success()
            && stdout(&gate_allowed).contains("OK: dependencies demand no authority"),
        "gate with Net consented must pass: {:?}",
        stdout(&gate_allowed)
    );
}

/// Registry guardrails in the **witchy** coven, over real HTTP: promote and
/// yank of a never-published version are 404s; re-publishing a released
/// version is refused 409 (immutability); and promote reports the rights-
/// precise `delta_runtime` against the latest released version — a first
/// release's delta is its whole footprint, an upgrade's delta names only the
/// NEW authority.
#[test]
fn witchy_coven_promote_delta_immutability_and_error_paths() {
    let (mut server, addr, store) = start_basic_coven("witchy-coven-guard");

    let publish = |version: &str, declared: &str, module: &str| {
        let manifest = format!(
            "[rune]\nname = \"acme/widget\"\nversion = \"{version}\"\n\n[capabilities]\nruntime = [{declared}]\n"
        );
        let source = format!(
            "{{\"files\":[[\"witchy.toml\",{}],[\"src/widget.witchy\",{}]]}}",
            json_str(&manifest),
            json_str(module)
        );
        let body = format!(
            "{{\"manifest_toml\":{},\"source\":{source},\"uploaded_by\":\"ci\"}}",
            json_str(&manifest)
        );
        http_post(&addr, "/coven/publish", &body)
    };
    let promote = |version: &str| {
        let body = format!(
            "{{\"name\":\"acme~widget\",\"version\":\"{version}\",\"second_factor\":\"webauthn\",\"promoted_by\":\"human\"}}"
        );
        http_post(&addr, "/coven/promote", &body)
    };

    // Error paths first: nothing is published yet.
    let ghost_promote = promote("9.9.9");
    let ghost_yank =
        http_post(&addr, "/coven/yank", "{\"name\":\"acme~widget\",\"version\":\"9.9.9\"}");
    // First release: the delta against an empty baseline is the whole footprint.
    let console_module = "pub fn run(console: Console):\n    print(console, \"hi\")\n";
    let pub_v1 = publish("1.0.0", "\"Console\"", console_module);
    let promote_v1 = promote("1.0.0");
    // Immutability: the released version cannot be re-published.
    let republish = publish("1.0.0", "\"Console\"", console_module);
    // An upgrade that widens: only the NEW authority appears in the delta.
    let net_module = "pub fn run(console: Console):\n    print(console, \"hi\")\n\npub fn fetch(net: Net) -> Int:\n    0\n";
    let pub_v2 = publish("1.1.0", "\"Console\", \"Net\"", net_module);
    let promote_v2 = promote("1.1.0");

    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&store);

    assert_eq!(ghost_promote.0, 404, "promote of a never-published version: {}", ghost_promote.1);
    assert_eq!(ghost_yank.0, 404, "yank of a never-published version: {}", ghost_yank.1);
    assert_eq!(pub_v1.0, 200, "publish 1.0.0: {}", pub_v1.1);
    assert_eq!(promote_v1.0, 200, "promote 1.0.0: {}", promote_v1.1);
    assert!(
        promote_v1.1.contains("\"delta_runtime\":[\"Console\"]"),
        "first release delta must be its whole footprint: {}",
        promote_v1.1
    );
    assert_eq!(republish.0, 409, "re-publishing a version must be refused: {}", republish.1);
    assert_eq!(pub_v2.0, 200, "publish 1.1.0: {}", pub_v2.1);
    assert_eq!(promote_v2.0, 200, "promote 1.1.0: {}", promote_v2.1);
    assert!(
        promote_v2.1.contains("\"delta_runtime\":[\"Net\"]"),
        "an upgrade's delta must name only the NEW authority: {}",
        promote_v2.1
    );
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
        "import lib\n\nfn main(console: Console):\n    print(console, lib.shout(\"net\"))\n",
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
         \x20\x20\x20\x20print(console, glamour.to_html(view(7)))\n",
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
    let dir = unique("grants");
    let cfg = dir.join("cfg.txt");
    std::fs::write(&cfg, "config-body").unwrap();
    let prog = dir.join("prog.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console, config: File[Read]):\n    print(console, read(config))\n",
    )
    .unwrap();

    // A sufficient grant binds `config` by name and runs.
    let ok = dir.join("ok.toml");
    std::fs::write(
        &ok,
        format!("[files]\nconfig = {{ path = \"{}\", rights = [\"Read\"] }}\n", cfg.display()),
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", ok.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "grant run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("config-body"), "got: {}", stdout(&out));

    // An under-grant (no `[files].config`) aborts with a clear error, nonzero exit.
    let under = dir.join("under.toml");
    std::fs::write(&under, "[net]\nx = [\"h:1\"]\n").unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--grants", under.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "an under-grant must abort the run");
    assert!(
        stderr(&out).contains("insufficient") || stdout(&out).contains("insufficient"),
        "expected an insufficiency error: out={} err={}",
        stdout(&out),
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grant_document_file_rights_are_parameter_exact() {
    let dir = unique("grant-rights");
    let input = dir.join("input.txt");
    let output = dir.join("output.txt");
    std::fs::write(&input, "in").unwrap();
    let prog = dir.join("prog.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console, input: File[Read], output: File[Write]):\n    print(console, read(input))\n    write(output, \"out\")\n",
    )
    .unwrap();

    let swapped = dir.join("swapped.toml");
    std::fs::write(
        &swapped,
        format!(
            "[files]\ninput = {{ path = \"{}\", rights = [\"Write\"] }}\noutput = {{ path = \"{}\", rights = [\"Read\"] }}\n",
            input.display(),
            output.display()
        ),
    )
    .unwrap();

    let out = Command::new(BIN)
        .args(["sandbox", "--grants", swapped.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "swapped grant rights must abort the run");
    assert!(
        stderr(&out).contains("do not match") || stdout(&out).contains("do not match"),
        "expected exact-rights mismatch: out={} err={}",
        stdout(&out),
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// RFC-0005 Stage 2 / RFC-0012: direct `--file` grants are minted as File
/// externrefs in the compiled sandbox path, for both read and write leaf ops.
#[test]
fn sandbox_direct_file_grants_read_and_write() {
    let dir = unique("filegrant");
    let input = dir.join("input.txt");
    let output = dir.join("output.txt");
    std::fs::write(&input, "direct-read").unwrap();

    let read_prog = dir.join("read.witchy");
    std::fs::write(
        &read_prog,
        "fn main(console: Console, config: File[Read]):\n    print(console, read(config))\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--file", input.to_str().unwrap(), read_prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "direct --file read failed: {}\n{}",
        stderr(&out),
        stdout(&out)
    );
    assert!(stdout(&out).contains("direct-read"), "got: {}", stdout(&out));

    let write_prog = dir.join("write.witchy");
    std::fs::write(
        &write_prog,
        "fn main(console: Console, log: File[Write]):\n    write(log, \"direct-write\")\n    print(console, \"wrote\")\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--file", output.to_str().unwrap(), write_prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "direct --file write failed: {}\n{}",
        stderr(&out),
        stdout(&out)
    );
    assert!(stdout(&out).contains("wrote"), "got: {}", stdout(&out));
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "direct-write");

    let _ = std::fs::remove_dir_all(&dir);
}

/// SEC-009 regression: `witchy sandbox` for a `Dir`-binding `main` must FAIL CLOSED
/// when no `--dir` is granted (deny by omission) rather than silently defaulting to
/// the cwd — exactly as a `File`-binding `main` requires `--file`. An explicit
/// `--dir` still runs. (The dev `witchy <file>` path keeps its cwd convenience and is
/// not a security boundary, so it is intentionally not covered here.)
#[test]
fn sandbox_dir_requires_explicit_grant() {
    let dir = unique("dirgrant");
    let prog = dir.join("prog.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console, dir: Dir):\n    let ok = exists(dir, \"prog.witchy\")\n    print(console, \"${ok}\")\n",
    )
    .unwrap();

    // No --dir: deny by omission — aborts with a clear error, nonzero exit.
    let out = Command::new(BIN).args(["sandbox", prog.to_str().unwrap()]).output().unwrap();
    assert!(!out.status.success(), "a Dir-binding main with no --dir must abort");
    assert!(
        stderr(&out).contains("no subtree was granted") || stdout(&out).contains("no subtree was granted"),
        "expected a deny-by-omission error: out={} err={}",
        stdout(&out),
        stderr(&out)
    );

    // An explicit --dir runs.
    let out = Command::new(BIN)
        .args(["sandbox", "--dir", dir.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "explicit --dir run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("true"), "got: {}", stdout(&out));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn sandbox_dir_list_rejects_non_utf8_names() {
    use std::os::unix::ffi::OsStringExt;

    let dir = unique("dir-list-nonutf8");
    let grant = dir.join("grant");
    std::fs::create_dir_all(&grant).unwrap();
    std::fs::write(grant.join("normal.txt"), "ok").unwrap();
    let bad = PathBuf::from(std::ffi::OsString::from_vec(vec![0xbd, 0xb2, b'=', 0xbc]));
    if std::fs::write(grant.join(bad), "hidden").is_err() {
        // Some Unix filesystems reject non-UTF-8 names at creation time. The
        // runtime bug is only observable where such an entry can exist.
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let prog = dir.join("prog.witchy");
    std::fs::write(
        &prog,
        "fn main(console: Console, root: Dir):\n    let names = list(root)\n    print(console, \"listed\")\n",
    )
    .unwrap();

    let source = Command::new(BIN)
        .args(["sandbox", "--dir", grant.to_str().unwrap(), prog.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!source.status.success(), "source sandbox must reject non-UTF8 names");
    assert!(
        stdout(&source).contains("not valid UTF-8") || stderr(&source).contains("not valid UTF-8"),
        "expected UTF-8 error from source sandbox: out={} err={}",
        stdout(&source),
        stderr(&source)
    );

    let wasm = dir.join("prog.wasm");
    let emit = Command::new(BIN)
        .args(["emit-wasm", prog.to_str().unwrap(), "-o", wasm.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(emit.status.success(), "emit-wasm failed: {}\n{}", stderr(&emit), stdout(&emit));

    let artifact = Command::new(BIN)
        .args(["sandbox", "--dir", grant.to_str().unwrap(), wasm.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!artifact.status.success(), "wasm sandbox must reject non-UTF8 names");
    assert!(
        stdout(&artifact).contains("not valid UTF-8") || stderr(&artifact).contains("not valid UTF-8"),
        "expected UTF-8 error from wasm sandbox: out={} err={}",
        stdout(&artifact),
        stderr(&artifact)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// SEC-004: `crypto.reveal` gates the SIGNING key only. A named `--secret`
/// value-secret (e.g. an OAuth client secret) stays revealable, but the signing key
/// (`--signing-key`) is sign-only — revealing it aborts. Closes the seed-exfiltration
/// hole while preserving the legitimate value-secret use.
#[test]
fn sandbox_reveal_gates_signing_key_only() {
    let dir = unique("reveal");
    // A named value-secret reveals fine.
    let named = dir.join("named.witchy");
    std::fs::write(
        &named,
        "import crypto\nimport secretstore\nfn main(console: Console, store: SecretStore):\n    print(console, crypto.reveal(secretstore.require(store, \"token\")))\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--secret", "token=s3cr3t", named.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "named-secret reveal failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("s3cr3t"), "got: {}", stdout(&out));

    // The signing key is NOT revealable.
    let seedfile = dir.join("seed.hex");
    std::fs::write(&seedfile, "41".repeat(32)).unwrap();
    let signing = dir.join("signing.witchy");
    std::fs::write(
        &signing,
        "import crypto\nfn main(console: Console, key: Secret):\n    print(console, crypto.reveal(key))\n",
    )
    .unwrap();
    let out = Command::new(BIN)
        .args(["sandbox", "--signing-key", seedfile.to_str().unwrap(), signing.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success(), "revealing the signing key must abort");
    assert!(
        stderr(&out).contains("not revealable") || stdout(&out).contains("not revealable"),
        "expected a not-revealable error: out={} err={}",
        stdout(&out),
        stderr(&out)
    );

    // RFC-0060: a NAMED secret granted `,use-only` is usable by handle but
    // NOT revealable — the same `token` program that reveals fine above must
    // abort when the grant carries the use-only modifier.
    let out = Command::new(BIN)
        .args(["sandbox", "--secret", "token=s3cr3t,use-only", named.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "revealing a use-only secret must abort: {}\n{}",
        stderr(&out),
        stdout(&out)
    );
    assert!(
        stderr(&out).contains("use-only") || stdout(&out).contains("use-only"),
        "expected a use-only-not-revealable error for the use-only secret: out={} err={}",
        stdout(&out),
        stderr(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
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
        "fn main(console: Console):\n    print(console, \"hello\")\n    print(console, \"${1 + 2}\")\n",
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
            "fn main(console: Console, net: Net):\n    let s = connect(net, \"{addr}\")\n    send_line(s, \"ping\")\n    print(console, recv_line(s))\n"
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
        "import genmod\nimport genmod2\n\nfn main(console: Console):\n    print(console, __render(genmod.value() + genmod2.value()))\n",
    )
    .unwrap();

    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(lib.join("src/genlib.witchy"), "pub fn placeholder() -> Int:\n    0\n").unwrap();
    // The build step emits TWO modules; audit_then_flags must produce one --dep per
    // module (deduped by name), not duplicates.
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    write_out(out, \"genmod.witchy\", \"pub fn value() -> Int:\\n    40\\n\")\n    write_out(out, \"genmod2.witchy\", \"pub fn value() -> Int:\\n    2\\n\")\n",
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
