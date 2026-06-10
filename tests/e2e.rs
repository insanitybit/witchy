//! End-to-end tests for coven, the witchy package manager. These drive the real
//! `witchy` binary (via `CARGO_BIN_EXE_witchy`) through the full supply-chain
//! lifecycle: scaffold, publish (staged), promote (second factor), add (gated),
//! build, run, audit. Each test is hermetic — its own temp `WITCHY_HOME` and
//! working tree — so they can run in parallel.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

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
    /// Bind to an ephemeral port (`:0`) and discover the actual port from the
    /// server's startup line — race-free, unlike pre-picking a port.
    fn start() -> RegistryServer {
        let regroot = unique("coven-regroot");
        let home = unique("coven-srv-home");
        // Generate the IdP signing key and capture its public key (the JWKS).
        let issuer_dir = unique("coven-issuer");
        let gen_out = Command::new(BIN)
            .args(["coven-gen-issuer", "--out", issuer_dir.to_str().unwrap()])
            .output()
            .expect("gen issuer");
        let pubhex = String::from_utf8_lossy(&gen_out.stdout).trim().to_string();

        let mut child = Command::new(BIN)
            .args([
                "coven-serve",
                "--addr",
                "127.0.0.1:0",
                "--root",
                regroot.to_str().unwrap(),
                "--trust-issuer",
                &format!("{ISSUER}={pubhex}"),
            ])
            .env("WITCHY_HOME", &home)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn coven-serve");

        // The server prints "...serving at http://HOST:PORT ..." once bound.
        let stdout = child.stdout.take().expect("piped stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read server startup line");
        let port = line
            .split("http://")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|hostport| hostport.rsplit(':').next())
            .and_then(|p| p.trim().parse::<u16>().ok())
            .unwrap_or_else(|| panic!("could not parse server port from: {line:?}"));

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

fn unique(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("coven-e2e-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Sandbox {
    home: PathBuf,
    work: PathBuf,
    /// When set, every command talks to a remote coven server instead of the
    /// local-directory registry.
    coven_url: Option<String>,
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        Sandbox {
            home: unique(&format!("{tag}-home")),
            work: unique(&format!("{tag}-work")),
            coven_url: None,
        }
    }

    fn run(&self, dir: &Path, user: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.current_dir(dir)
            .env("WITCHY_HOME", &self.home)
            .env("WITCHY_USER", user)
            // Most tests publish and immediately consume; zero the staging
            // cooldown so they exercise their own subject. The cooldown itself
            // has a dedicated test that overrides this.
            .env("WITCHY_COOLDOWN_SECS", "0")
            .args(args);
        if let Some(u) = &self.coven_url {
            cmd.env("COVEN_URL", u);
        }
        cmd.output().expect("spawn witchy")
    }

    /// Like `run`, but presenting a short-lived identity token (trusted
    /// publishing) via `COVEN_ID_TOKEN`.
    fn run_id(&self, dir: &Path, user: &str, id_token: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.current_dir(dir)
            .env("WITCHY_HOME", &self.home)
            .env("WITCHY_USER", user)
            .env("WITCHY_COOLDOWN_SECS", "0")
            .env("COVEN_ID_TOKEN", id_token)
            .args(args);
        if let Some(u) = &self.coven_url {
            cmd.env("COVEN_URL", u);
        }
        cmd.output().expect("spawn witchy")
    }

    /// Create + publish + promote a library rune in one shot. Returns its dir.
    fn publish_lib(&self, name: &str, version: &str, module_body: &str) -> PathBuf {
        let dir_name = name.rsplit('/').next().unwrap();
        let dir = self.work.join(dir_name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("witchy.toml"),
            format!("[rune]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
        let module = dir_name.replace('-', "_");
        std::fs::write(dir.join("src").join(format!("{module}.witchy")), module_body).unwrap();
        let out = self.run(&dir, "ci-bot", &["publish"]);
        assert!(out.status.success(), "publish failed: {}", stderr(&out));
        let out = self.run(
            &dir,
            "alice",
            &["promote", &format!("{name}@{version}"), "--factor", "webauthn"],
        );
        assert!(out.status.success(), "promote failed: {}", stderr(&out));
        dir
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
        let _ = std::fs::remove_dir_all(&self.work);
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn new_app(sb: &Sandbox) -> PathBuf {
    let out = sb.run(&sb.work, "dev", &["new", "app"]);
    assert!(out.status.success(), "new failed: {}", stderr(&out));
    sb.work.join("app")
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

#[test]
fn networked_registry_full_lifecycle() {
    let server = RegistryServer::start();
    let mut sb = Sandbox::new("net");
    sb.coven_url = Some(server.url());

    let app = new_app(&sb);
    // Publish to the remote server.
    let lib = sb.work.join("lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n").unwrap();
    std::fs::write(lib.join("src/lib.witchy"), "fn shout(s: String) -> String:\n    \"HEY \" <> s\n").unwrap();
    // Publish via a trusted CI identity token (no long-lived API key).
    let ci = server.ci_token("acme/lib-repo", "release.yml");
    assert!(sb.run_id(&lib, "ci", &ci, &["publish"]).status.success());

    // Staged over the network → not addable.
    let out = sb.run(&app, "dev", &["add", "acme/lib"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("STAGED"), "stderr: {}", stderr(&out));

    // Promote over the network with a human identity token + a second factor.
    let alice = server.human_token("alice");
    let out = sb.run_id(&lib, "alice", &alice, &["promote", "acme/lib@1.0.0", "--factor", "webauthn"]);
    assert!(out.status.success(), "remote promote failed: {}", stderr(&out));
    assert!(stdout(&out).contains("RELEASED"));

    // Add (fetched over HTTP, signature-verified) and run.
    let out = sb.run(&app, "dev", &["add", "acme/lib"]);
    assert!(out.status.success(), "remote add failed: {}", stderr(&out));
    std::fs::write(
        app.join("src/app.witchy"),
        "import lib\n\nfn main(console: Console):\n    print(console, lib.shout(\"net\"))\n",
    )
    .unwrap();
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "remote run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("HEY net"), "got: {}", stdout(&out));

    // The lock pinned the remote registry's key fingerprint.
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("ed25519:"), "lock should pin remote key: {lock}");

    // `list` over the network reflects the released state.
    let out = sb.run(&app, "dev", &["list", "acme/lib"]);
    assert!(stdout(&out).contains("released"));
}

#[test]
fn trusted_publishing_binds_repo_and_rejects_others() {
    let server = RegistryServer::start();
    let mut sb = Sandbox::new("auth");
    sb.coven_url = Some(server.url());

    let lib = sb.work.join("lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/secure\"\nversion = \"1.0.0\"\n").unwrap();
    std::fs::write(lib.join("src/secure.witchy"), "fn f(s: String) -> String:\n    s\n").unwrap();

    // First trusted publish from acme/secure-repo / release.yml binds the
    // namespace `acme` to that exact source + workflow (TOFU).
    let good = server.ci_token("acme/secure-repo", "release.yml");
    assert!(sb.run_id(&lib, "ci", &good, &["publish"]).status.success());

    // A token from a DIFFERENT repository cannot publish to `acme` — even though
    // it's a valid token from the trusted issuer. (Namespace-squat / hijack.)
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/secure\"\nversion = \"1.1.0\"\n").unwrap();
    let evil = server.ci_token("evil/fork", "release.yml");
    let out = sb.run_id(&lib, "ci", &evil, &["publish"]);
    assert!(!out.status.success(), "publish from wrong repo must be refused");
    assert!(stderr(&out).contains("not authorized") || stderr(&out).contains("policy"), "stderr: {}", stderr(&out));

    // A token from the right repo but a NON-release workflow is also refused.
    let wrong_wf = server.ci_token("acme/secure-repo", "ci.yml");
    let out = sb.run_id(&lib, "ci", &wrong_wf, &["publish"]);
    assert!(!out.status.success(), "publish from wrong workflow must be refused");

    // The legitimate CI identity may publish the new version.
    let out = sb.run_id(&lib, "ci", &good, &["publish"]);
    assert!(out.status.success(), "legit re-publish failed: {}", stderr(&out));

    // Promotion requires a human identity + second factor, and the human must
    // not be the CI that staged it (separation of duties).
    let alice = server.human_token("alice");
    let out = sb.run_id(&lib, "alice", &alice, &["promote", "acme/secure@1.0.0", "--factor", "webauthn"]);
    assert!(out.status.success(), "human promote failed: {}", stderr(&out));
    assert!(stdout(&out).contains("RELEASED"));
}

#[test]
fn bearer_token_is_not_accepted_for_publishing() {
    let server = RegistryServer::start();
    let mut sb = Sandbox::new("nobearer");
    sb.coven_url = Some(server.url());
    let lib = sb.work.join("lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/x\"\nversion = \"1.0.0\"\n").unwrap();
    std::fs::write(lib.join("src/x.witchy"), "fn f(s: String) -> String:\n    s\n").unwrap();
    // No identity token at all → remote publish is refused outright.
    let out = sb.run(&lib, "ci", &["publish"]);
    assert!(!out.status.success(), "publish without an identity token must be refused");
    assert!(stderr(&out).contains("identity token"), "stderr: {}", stderr(&out));

    // A token from an UNTRUSTED issuer is also refused.
    let other_issuer = unique("rogue-idp");
    let gen_out = Command::new(BIN).args(["coven-gen-issuer", "--out", other_issuer.to_str().unwrap()]).output().unwrap();
    assert!(gen_out.status.success());
    let mint = Command::new(BIN)
        .args(["coven-mint-token", "--issuer-key", other_issuer.to_str().unwrap(), "--issuer", "rogue", "--sub", "x", "--claim", "repository=acme/x"])
        .output()
        .unwrap();
    let rogue = String::from_utf8_lossy(&mint.stdout).trim().to_string();
    let out = sb.run_id(&lib, "ci", &rogue, &["publish"]);
    assert!(!out.status.success(), "token from untrusted issuer must be refused");
    assert!(stderr(&out).contains("not trusted") || stderr(&out).contains("untrusted"), "stderr: {}", stderr(&out));
    let _ = std::fs::remove_dir_all(&other_issuer);
}

#[test]
fn tuf_chain_verified_and_snapshot_tamper_rejected() {
    let server = RegistryServer::start();
    let mut sb = Sandbox::new("tuf");
    sb.coven_url = Some(server.url());
    let app = new_app(&sb);

    let lib = sb.work.join("lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/t\"\nversion = \"1.0.0\"\n").unwrap();
    std::fs::write(lib.join("src/t.witchy"), "fn f(s: String) -> String:\n    s\n").unwrap();
    let ci = server.ci_token("acme/t-repo", "release.yml");
    assert!(sb.run_id(&lib, "ci", &ci, &["publish"]).status.success());
    let alice = server.human_token("alice");
    assert!(sb.run_id(&lib, "alice", &alice, &["promote", "acme/t@1.0.0", "--factor", "totp"]).status.success());
    assert!(sb.run(&app, "dev", &["add", "acme/t"]).status.success());

    // The lock pinned a TUF snapshot version, and verify confirms the chain.
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("registry_snapshot_version"), "lock should pin snapshot version: {lock}");
    let out = sb.run(&app, "dev", &["verify"]);
    assert!(out.status.success(), "verify failed: {}", stderr(&out));
    assert!(stdout(&out).contains("TUF chain"), "verify out: {}", stdout(&out));

    // Tamper the SERVER's signed snapshot (changing a signed field breaks the
    // snapshot-role signature). Clear the client store so it must re-consult.
    let snap = server.regroot.join("snapshot.json");
    let body = std::fs::read_to_string(&snap).unwrap().replace("1.0.0", "1.0.1");
    std::fs::write(&snap, body).unwrap();
    std::fs::remove_dir_all(sb.home.join("store")).ok();

    let out = sb.run(&app, "dev", &["verify"]);
    assert!(!out.status.success(), "tampered snapshot must fail verify");
    assert!(stdout(&out).contains("FAIL"), "verify out: {}", stdout(&out));
}

#[test]
fn tuf_rollback_is_rejected() {
    let server = RegistryServer::start();
    let mut sb = Sandbox::new("rollback");
    sb.coven_url = Some(server.url());
    let app = new_app(&sb);

    let lib = sb.work.join("lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/r\"\nversion = \"1.0.0\"\n").unwrap();
    std::fs::write(lib.join("src/r.witchy"), "fn f(s: String) -> String:\n    s\n").unwrap();
    let ci = server.ci_token("acme/r-repo", "release.yml");
    assert!(sb.run_id(&lib, "ci", &ci, &["publish"]).status.success());
    let alice = server.human_token("alice");
    assert!(sb.run_id(&lib, "alice", &alice, &["promote", "acme/r@1.0.0", "--factor", "totp"]).status.success());
    assert!(sb.run(&app, "dev", &["add", "acme/r"]).status.success());

    // Simulate having previously seen a much newer snapshot: bump the pinned
    // version in the lock. The server now presents an older snapshot version —
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

    let out = sb.run(&app, "dev", &["verify"]);
    assert!(!out.status.success(), "rollback must be refused");
    assert!(stdout(&out).contains("rolled back") || stdout(&out).contains("rollback"), "out: {}", stdout(&out));
}

#[test]
fn networked_registry_signature_detects_tampering() {
    let server = RegistryServer::start();
    let mut sb = Sandbox::new("nettamper");
    sb.coven_url = Some(server.url());
    let app = new_app(&sb);

    let lib = sb.work.join("lib");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"acme/x\"\nversion = \"1.0.0\"\n").unwrap();
    std::fs::write(lib.join("src/x.witchy"), "fn f(s: String) -> String:\n    s\n").unwrap();
    let ci = server.ci_token("acme/x-repo", "release.yml");
    assert!(sb.run_id(&lib, "ci", &ci, &["publish"]).status.success());
    let alice = server.human_token("alice");
    assert!(sb
        .run_id(&lib, "alice", &alice, &["promote", "acme/x@1.0.0", "--factor", "totp"])
        .status
        .success());
    assert!(sb.run(&app, "dev", &["add", "acme/x"]).status.success());

    // Tamper a signed field of the record in the SERVER's storage (the
    // provenance attestation is signed, so editing it breaks the signature).
    let meta = server.regroot.join("acme/x/1.0.0/coven.json");
    let json = std::fs::read_to_string(&meta).unwrap().replace("trusted-publisher", "evil-publisher");
    std::fs::write(&meta, json).unwrap();

    // A fresh client (clear its store so it must re-fetch) must reject the
    // tampered record via the signature — verify re-fetches from the server.
    std::fs::remove_dir_all(sb.home.join("store")).ok();
    let out = sb.run(&app, "dev", &["verify"]);
    assert!(!out.status.success(), "tampered remote record must fail verify");
    assert!(
        stdout(&out).contains("FAIL") || stderr(&out).contains("signature"),
        "stdout {} stderr {}",
        stdout(&out),
        stderr(&out)
    );
}

#[test]
fn scaffold_and_run() {
    let sb = Sandbox::new("scaffold");
    let app = new_app(&sb);
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("hello from app"), "got: {}", stdout(&out));
}

#[test]
fn full_lifecycle_publish_promote_add_use() {
    let sb = Sandbox::new("lifecycle");
    let app = new_app(&sb);
    sb.publish_lib(
        "acme/strkit",
        "0.1.0",
        "fn shout(s: String) -> String:\n    \"HEY \" <> s\n",
    );

    // Add the released library (pure — no capability widening, so no consent needed).
    let out = sb.run(&app, "dev", &["add", "acme/strkit"]);
    assert!(out.status.success(), "add failed: {}", stderr(&out));
    assert!(stdout(&out).contains("demands no capabilities"));

    // Use it from main.
    std::fs::write(
        app.join("src").join("app.witchy"),
        "import strkit\n\nfn main(console: Console):\n    print(console, strkit.shout(\"witchy\"))\n",
    )
    .unwrap();
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("HEY witchy"), "got: {}", stdout(&out));

    // The lockfile pins the dependency.
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("acme/strkit"));
    assert!(lock.contains("sha256:"));
}

#[test]
fn staged_dependency_is_not_resolvable() {
    let sb = Sandbox::new("staged");
    let app = new_app(&sb);

    // Publish WITHOUT promoting.
    let dir = sb.work.join("json");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/json\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/json.witchy"), "fn p(s: String) -> String:\n    s\n").unwrap();
    assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());

    // Adding a pinned-but-staged version must fail and mention STAGED.
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"acme/json\" = \"^1.0.0\"\n",
    )
    .unwrap();
    let out = sb.run(&app, "dev", &["build"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("STAGED"), "stderr: {}", stderr(&out));
}

#[test]
fn promote_requires_second_factor() {
    let sb = Sandbox::new("factor");
    let dir = sb.work.join("lib");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/lib\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.witchy"), "fn f(s: String) -> String:\n    s\n").unwrap();
    assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());

    // No --factor -> refused.
    let out = sb.run(&dir, "alice", &["promote", "acme/lib@1.0.0"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("second factor"), "stderr: {}", stderr(&out));

    // With --factor -> released.
    let out = sb.run(&dir, "alice", &["promote", "acme/lib@1.0.0", "--factor", "webauthn"]);
    assert!(out.status.success(), "promote failed: {}", stderr(&out));
    assert!(stdout(&out).contains("RELEASED"));
    assert!(stdout(&out).contains("separation of duties"));
}

#[test]
fn gate_blocks_capability_widening_then_allows_with_consent() {
    let sb = Sandbox::new("gate");
    let app = new_app(&sb);
    // A library that demands Net in its public API.
    sb.publish_lib(
        "acme/netkit",
        "0.1.0",
        "fn fetch(net: Net, url: String) -> String:\n    url\n",
    );

    // Adding it to a pure app must BLOCK and write nothing.
    let out = sb.run(&app, "dev", &["add", "acme/netkit"]);
    assert!(!out.status.success(), "expected block, got success");
    assert!(stdout(&out).contains("BLOCKED"));
    assert!(stdout(&out).contains("Net"));
    assert!(!app.join("witchy.lock").exists(), "lock must not be written on block");
    let manifest = std::fs::read_to_string(app.join("witchy.toml")).unwrap();
    assert!(!manifest.contains("netkit"), "manifest must be untouched on block");

    // A dry run with consent should pass the gate but still write nothing.
    let out = sb.run(&app, "dev", &["add", "acme/netkit", "--allow-cap", "Net", "--dry-run"]);
    assert!(out.status.success(), "dry-run failed: {}", stderr(&out));
    assert!(!app.join("witchy.lock").exists(), "dry-run must not write");

    // With explicit consent (no dry run), it proceeds and records Net.
    let out = sb.run(&app, "dev", &["add", "acme/netkit", "--allow-cap", "Net"]);
    assert!(out.status.success(), "consented add failed: {}", stderr(&out));
    assert!(app.join("witchy.lock").exists());

    // Audit reflects the new authority; why-cap traces it.
    let out = sb.run(&app, "dev", &["audit"]);
    assert!(stdout(&out).contains("Net"));
    let out = sb.run(&app, "dev", &["why-cap", "Net"]);
    assert!(stdout(&out).contains("acme/netkit"));

    // Verify the lock against the registry.
    let out = sb.run(&app, "dev", &["verify"]);
    assert!(out.status.success(), "verify failed: {}", stderr(&out));
    assert!(stdout(&out).contains("verified"));
}

#[test]
fn transitive_dependency_caps_aggregate() {
    let sb = Sandbox::new("transitive");
    let app = new_app(&sb);
    sb.publish_lib("acme/url", "1.0.0", "fn parse(s: String) -> String:\n    s\n");

    // http depends on url and demands Net.
    let dir = sb.work.join("http");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/http\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/url\" = \"^1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/http.witchy"), "fn get(net: Net, u: String) -> String:\n    u\n").unwrap();
    assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
    assert!(sb
        .run(&dir, "alice", &["promote", "acme/http@1.0.0", "--factor", "totp"])
        .status
        .success());

    // Adding http pulls url transitively; Net must surface and gate-block.
    let out = sb.run(&app, "dev", &["add", "acme/http", "--allow-cap", "Net"]);
    assert!(out.status.success(), "add failed: {}", stderr(&out));
    let out = sb.run(&app, "dev", &["why", "acme/url"]);
    assert!(stdout(&out).contains("acme/http"), "why output: {}", stdout(&out));
    assert!(stdout(&out).contains("acme/url"));
}

#[test]
fn upgrade_that_widens_is_gated() {
    let sb = Sandbox::new("upgrade");
    let app = new_app(&sb);

    // v1.0.0 of a logger: pure, no capabilities.
    sb.publish_lib(
        "acme/logger",
        "1.0.0",
        "fn line(s: String) -> String:\n    s\n",
    );
    let out = sb.run(&app, "dev", &["add", "acme/logger"]);
    assert!(out.status.success(), "add v1 failed: {}", stderr(&out));

    // v1.1.0 quietly starts demanding Net (a classic account-takeover scenario).
    let dir = sb.work.join("logger");
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/logger\"\nversion = \"1.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/logger.witchy"),
        "fn line(s: String) -> String:\n    s\nfn beacon(net: Net, s: String) -> String:\n    s\n",
    )
    .unwrap();
    assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
    assert!(sb
        .run(&dir, "alice", &["promote", "acme/logger@1.1.0", "--factor", "webauthn"])
        .status
        .success());

    // `update` must BLOCK: the upgrade widens logger's footprint with Net.
    let out = sb.run(&app, "dev", &["update"]);
    assert!(!out.status.success(), "update should block the widening upgrade");
    assert!(stdout(&out).contains("Net"), "got: {}", stdout(&out));

    // With consent it proceeds.
    let out = sb.run(&app, "dev", &["update", "--allow-cap", "Net"]);
    assert!(out.status.success(), "consented update failed: {}", stderr(&out));
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("1.1.0"), "lock should pin the upgraded version");
}

#[test]
fn diamond_dependency_resolves_shared_base_once() {
    let sb = Sandbox::new("diamond");
    let app = new_app(&sb);
    sb.publish_lib("acme/base", "1.0.0", "fn b(s: String) -> String:\n    s\n");

    // left and right both depend on base.
    for side in ["left", "right"] {
        let dir = sb.work.join(side);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("witchy.toml"),
            format!("[rune]\nname = \"acme/{side}\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/base\" = \"^1.0.0\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join(format!("src/{side}.witchy")), "fn x(s: String) -> String:\n    s\n").unwrap();
        assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
        assert!(sb
            .run(&dir, "alice", &["promote", &format!("acme/{side}@1.0.0"), "--factor", "totp"])
            .status
            .success());
    }

    assert!(sb.run(&app, "dev", &["add", "acme/left"]).status.success());
    assert!(sb.run(&app, "dev", &["add", "acme/right"]).status.success());

    // base appears exactly once in the lock despite two paths to it.
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    let occurrences = lock.matches("name = \"acme/base\"").count();
    assert_eq!(occurrences, 1, "shared base must resolve once; lock:\n{lock}");
    assert!(sb.run(&app, "dev", &["build"]).status.success());
}

#[test]
fn update_single_package_leaves_others_pinned() {
    let sb = Sandbox::new("update1");
    let app = new_app(&sb);
    sb.publish_lib("acme/a", "1.0.0", "fn f(s: String) -> String:\n    s\n");
    sb.publish_lib("acme/b", "1.0.0", "fn g(s: String) -> String:\n    s\n");
    assert!(sb.run(&app, "dev", &["add", "acme/a"]).status.success());
    assert!(sb.run(&app, "dev", &["add", "acme/b"]).status.success());

    // Newer versions of both become available.
    for n in ["a", "b"] {
        let dir = sb.work.join(n);
        std::fs::write(
            dir.join("witchy.toml"),
            format!("[rune]\nname = \"acme/{n}\"\nversion = \"1.1.0\"\n"),
        )
        .unwrap();
        assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
        assert!(sb
            .run(&dir, "alice", &["promote", &format!("acme/{n}@1.1.0"), "--factor", "totp"])
            .status
            .success());
    }

    // Update only acme/a; acme/b must stay pinned at 1.0.0.
    let out = sb.run(&app, "dev", &["update", "acme/a"]);
    assert!(out.status.success(), "update failed: {}", stderr(&out));
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    // a moved to 1.1.0, b stayed at 1.0.0.
    let a_at = lock.find("acme/a").map(|i| &lock[i..i + 60]).unwrap_or("");
    assert!(a_at.contains("1.1.0"), "acme/a should be 1.1.0; lock:\n{lock}");
    let b_at = lock.find("acme/b").map(|i| &lock[i..i + 60]).unwrap_or("");
    assert!(b_at.contains("1.0.0"), "acme/b should stay 1.0.0; lock:\n{lock}");
}

#[test]
fn yank_excludes_from_new_resolution() {
    let sb = Sandbox::new("yank");
    let lib = sb.publish_lib("acme/old", "1.0.0", "fn f(s: String) -> String:\n    s\n");

    // Yank it.
    let out = sb.run(&lib, "alice", &["yank", "acme/old@1.0.0"]);
    assert!(out.status.success(), "yank failed: {}", stderr(&out));

    // A fresh app can no longer add it (no non-yanked released version).
    let app = new_app(&sb);
    let out = sb.run(&app, "dev", &["add", "acme/old"]);
    assert!(!out.status.success(), "yanked version must not be addable");

    // `list` reflects the yanked state.
    let out = sb.run(&app, "dev", &["list", "acme/old"]);
    assert!(stdout(&out).contains("yanked"), "list: {}", stdout(&out));
}

#[test]
fn provenance_is_always_recorded() {
    let sb = Sandbox::new("prov");
    let app = new_app(&sb);
    sb.publish_lib("acme/p", "1.0.0", "fn f(s: String) -> String:\n    s\n");
    let out = sb.run(&app, "dev", &["add", "acme/p"]);
    assert!(out.status.success(), "add failed: {}", stderr(&out));
    let out = sb.run(&app, "dev", &["audit"]);
    let s = stdout(&out);
    assert!(s.contains("provenance:"), "audit: {s}");
    assert!(s.contains("uploader=ci-bot"), "audit: {s}");
}

#[test]
fn signature_detects_registry_metadata_tampering() {
    let sb = Sandbox::new("tamper");
    let app = new_app(&sb);
    sb.publish_lib("acme/x", "1.0.0", "fn f(s: String) -> String:\n    s\n");
    assert!(sb.run(&app, "dev", &["add", "acme/x"]).status.success());

    // A healthy verify checks signatures against the registry.
    assert!(sb.run(&app, "dev", &["verify"]).status.success());

    // Attacker edits a signed field of the registry record (source untouched, so
    // content hashing alone would miss it — the Ed25519 signature must catch it).
    let meta = sb.home.join("registry/acme/x/1.0.0/coven.json");
    let json = std::fs::read_to_string(&meta).unwrap().replace("ci-bot", "attacker");
    std::fs::write(&meta, json).unwrap();

    // `verify` re-fetches from the registry and must reject the tampered record.
    // (A `build` legitimately keeps working — it uses the trusted, hash-pinned
    // copy already in the local store; tampering upstream cannot affect it.)
    let out = sb.run(&app, "dev", &["verify"]);
    assert!(!out.status.success(), "tampered metadata must fail verify");
    assert!(
        stdout(&out).contains("FAIL") || stderr(&out).contains("signature") || stderr(&out).contains("tampered"),
        "stdout: {} stderr: {}",
        stdout(&out),
        stderr(&out)
    );
}

#[test]
fn lock_pins_registry_key_fingerprint() {
    let sb = Sandbox::new("pin");
    let app = new_app(&sb);
    sb.publish_lib("acme/y", "1.0.0", "fn f(s: String) -> String:\n    s\n");
    assert!(sb.run(&app, "dev", &["add", "acme/y"]).status.success());
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("registry_root"), "lock must pin the registry key");
    assert!(lock.contains("ed25519:"), "fingerprint format");
    // verify reports the pinned key as OK.
    let out = sb.run(&app, "dev", &["verify"]);
    assert!(out.status.success(), "verify failed: {}", stderr(&out));
    assert!(stdout(&out).contains("coven root key"));
}

#[test]
fn std_shadowing_dependency_is_refused() {
    let sb = Sandbox::new("shadow");
    let app = new_app(&sb);
    // A malicious rune whose module is literally `list` — trying to impersonate
    // the standard library's `list` module.
    let dir = sb.work.join("badlist");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"evil/list\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/list.witchy"), "fn range(n: Int) -> Int:\n    0\n").unwrap();
    assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
    assert!(sb
        .run(&dir, "alice", &["promote", "evil/list@1.0.0", "--factor", "totp"])
        .status
        .success());

    assert!(sb.run(&app, "dev", &["add", "evil/list"]).status.success());
    let out = sb.run(&app, "dev", &["build"]);
    assert!(!out.status.success(), "std-shadowing rune must be refused at build");
    assert!(stderr(&out).contains("shadow"), "stderr: {}", stderr(&out));
}

#[test]
fn module_name_collision_between_deps_is_caught() {
    let sb = Sandbox::new("collision");
    let app = new_app(&sb);
    // Two different runes that both expose a module named `util`.
    for ns in ["a", "b"] {
        let dir = sb.work.join(format!("util-{ns}"));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("witchy.toml"),
            format!("[rune]\nname = \"{ns}/util\"\nversion = \"1.0.0\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join("src/util.witchy"), "fn helper(s: String) -> String:\n    s\n").unwrap();
        assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
        assert!(sb
            .run(&dir, "alice", &["promote", &format!("{ns}/util@1.0.0"), "--factor", "totp"])
            .status
            .success());
    }
    assert!(sb.run(&app, "dev", &["add", "a/util"]).status.success());
    assert!(sb.run(&app, "dev", &["add", "b/util"]).status.success());
    let out = sb.run(&app, "dev", &["build"]);
    assert!(!out.status.success(), "module collision must be caught");
    assert!(stderr(&out).contains("collision"), "stderr: {}", stderr(&out));
}

#[test]
fn build_is_offline_from_the_store() {
    let sb = Sandbox::new("offline");
    let app = new_app(&sb);
    sb.publish_lib("acme/lib", "1.0.0", "fn f(s: String) -> String:\n    s\n");
    assert!(sb.run(&app, "dev", &["add", "acme/lib"]).status.success());
    std::fs::write(
        app.join("src/app.witchy"),
        "import lib\n\nfn main(console: Console):\n    print(console, lib.f(\"ok\"))\n",
    )
    .unwrap();
    assert!(sb.run(&app, "dev", &["run"]).status.success());

    // Now obliterate the registry entirely. A build/run must still succeed,
    // straight from the content-addressed store (hash-verified against the lock).
    std::fs::remove_dir_all(sb.home.join("registry")).unwrap();
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "offline run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("ok"), "got: {}", stdout(&out));
}

#[test]
fn vendored_sources_build_with_no_store_or_registry() {
    let sb = Sandbox::new("vendor");
    let app = new_app(&sb);
    sb.publish_lib("acme/lib", "1.0.0", "fn f(s: String) -> String:\n    s\n");
    assert!(sb.run(&app, "dev", &["add", "acme/lib"]).status.success());
    std::fs::write(
        app.join("src/app.witchy"),
        "import lib\n\nfn main(console: Console):\n    print(console, lib.f(\"vend\"))\n",
    )
    .unwrap();
    // Vendor the sources into the repo.
    assert!(sb.run(&app, "dev", &["vendor"]).status.success());
    assert!(app.join("vendor/acme/lib/witchy.toml").exists(), "vendor must write sources");

    // Simulate a fresh clone on another machine: no global store, no registry —
    // only the committed vendor/ tree and witchy.lock.
    std::fs::remove_dir_all(&sb.home).unwrap();
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "vendored run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("vend"), "got: {}", stdout(&out));
}

#[test]
fn outdated_reports_newer_versions_and_flags_widening() {
    let sb = Sandbox::new("outdated");
    let app = new_app(&sb);
    sb.publish_lib("acme/lib", "1.0.0", "fn f(s: String) -> String:\n    s\n");
    assert!(sb.run(&app, "dev", &["add", "acme/lib"]).status.success());

    // Up to date initially.
    let out = sb.run(&app, "dev", &["outdated"]);
    assert!(stdout(&out).contains("up to date") || stdout(&out).contains("latest"), "got: {}", stdout(&out));

    // Publish a newer version that demands Net.
    let dir = sb.work.join("lib");
    std::fs::write(dir.join("witchy.toml"), "[rune]\nname = \"acme/lib\"\nversion = \"1.1.0\"\n").unwrap();
    std::fs::write(
        dir.join("src/lib.witchy"),
        "fn f(s: String) -> String:\n    s\nfn net_f(net: Net, s: String) -> String:\n    s\n",
    )
    .unwrap();
    assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
    assert!(sb
        .run(&dir, "alice", &["promote", "acme/lib@1.1.0", "--factor", "totp"])
        .status
        .success());

    let out = sb.run(&app, "dev", &["outdated"]);
    let s = stdout(&out);
    assert!(s.contains("1.0.0 -> 1.1.0"), "outdated: {s}");
    assert!(s.contains("widen") && s.contains("Net"), "should flag widening: {s}");
}

#[test]
fn tree_shows_transitive_deps_and_capabilities() {
    let sb = Sandbox::new("tree");
    let app = new_app(&sb);
    sb.publish_lib("acme/url", "1.0.0", "fn parse(s: String) -> String:\n    s\n");

    let dir = sb.work.join("http");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/http\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/url\" = \"^1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/http.witchy"), "fn get(net: Net, u: String) -> String:\n    u\n").unwrap();
    assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
    assert!(sb
        .run(&dir, "alice", &["promote", "acme/http@1.0.0", "--factor", "totp"])
        .status
        .success());
    assert!(sb.run(&app, "dev", &["add", "acme/http", "--allow-cap", "Net"]).status.success());

    let out = sb.run(&app, "dev", &["tree"]);
    let s = stdout(&out);
    assert!(s.contains("acme/http"), "tree: {s}");
    assert!(s.contains("acme/url"), "tree: {s}");
    assert!(s.contains("Net"), "tree should annotate caps: {s}");
    assert!(s.contains("└──") || s.contains("├──"), "tree should draw branches: {s}");
}

#[test]
fn path_dependency_builds_and_runs() {
    let sb = Sandbox::new("path");
    let app = new_app(&sb);

    // A sibling library on disk (no registry involved).
    let lib = sb.work.join("greet");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"greet\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(
        lib.join("src/greet.witchy"),
        "fn hi(s: String) -> String:\n    \"hi \" <> s\n",
    )
    .unwrap();

    // Add it as a path dependency, then use it.
    let out = sb.run(&app, "dev", &["add", "greet", "--path", "../greet"]);
    assert!(out.status.success(), "add --path failed: {}", stderr(&out));
    std::fs::write(
        app.join("src/app.witchy"),
        "import greet\n\nfn main(console: Console):\n    print(console, greet.hi(\"witchy\"))\n",
    )
    .unwrap();
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("hi witchy"), "got: {}", stdout(&out));
}

/// Build-time execution is **default-deny, twice over** — even for a "safe" build
/// step that demands only the confined `BuildOut` sandbox. (1) `add` runs the
/// widening gate: the appearance of any build-axis kind must be accepted with
/// `--allow-build-cap`. (2) `build` then still refuses to accept the *existence*
/// of a build step until the consumer writes a `[build.grants."name"]` section —
/// you consent to any code execution before you consent to safe code execution.
/// An empty section is that consent (it permits only `BuildOut`).
#[test]
fn build_steps_are_default_deny_even_when_safe() {
    let sb = Sandbox::new("builddeny");
    let app = new_app(&sb);

    let lib = sb.work.join("safegen");
    std::fs::create_dir_all(lib.join("src")).unwrap();
    std::fs::write(lib.join("witchy.toml"), "[rune]\nname = \"safegen\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(
        lib.join("src/safegen.witchy"),
        "pub fn shout(s: String) -> String:\n    \"HEY \" <> s\n",
    )
    .unwrap();
    // A BuildOut-only build step: writes into its confined sandbox, nothing else.
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    write_out(out, \"gen.witchy\", \"// generated\")\n",
    )
    .unwrap();

    // Layer 1 — the gate: even BuildOut appearing on the build axis is a
    // widening that `add` blocks until explicitly accepted.
    let out = sb.run(&app, "dev", &["add", "safegen", "--path", "../safegen"]);
    assert!(!out.status.success(), "add must gate on the new build-axis kind");
    let blocked = format!("{}{}", stdout(&out), stderr(&out));
    assert!(blocked.contains("BuildOut"), "the gate should name BuildOut: {blocked}");
    let out = sb.run(
        &app,
        "dev",
        &["add", "safegen", "--path", "../safegen", "--allow-build-cap", "BuildOut"],
    );
    assert!(out.status.success(), "accepting the kind should let add proceed: {}", stderr(&out));

    // Layer 2 — execution consent: the build still refuses while no
    // [build.grants."safegen"] section exists at all.
    let out = sb.run(&app, "dev", &["build"]);
    assert!(!out.status.success(), "a build step must be denied without a grants section");
    assert!(
        stderr(&out).contains("build-time code execution is denied by default"),
        "denial should say why: {}",
        stderr(&out)
    );

    // The empty section is the explicit consent — it grants only BuildOut.
    let manifest = std::fs::read_to_string(app.join("witchy.toml")).unwrap();
    std::fs::write(
        app.join("witchy.toml"),
        format!("{manifest}\n[build.grants.\"safegen\"]\n"),
    )
    .unwrap();
    let out = sb.run(&app, "dev", &["build"]);
    assert!(
        out.status.success(),
        "an empty grants section accepts a BuildOut-only step: {}",
        stderr(&out)
    );
}

/// Staging cooldown (§8): a freshly released version is not resolvable until its
/// cooldown window passes — protection against a compromised release being
/// consumed the moment it lands — unless the consumer explicitly accepts it with
/// `--allow-fresh`. The `released_at` stamp is part of the signed record, so the
/// window can't be erased by metadata tampering.
#[test]
fn fresh_releases_cool_down_before_resolving() {
    let sb = Sandbox::new("cooldown");
    let app = new_app(&sb);
    sb.publish_lib("acme/fresh", "1.0.0", "fn f(s: String) -> String:\n    s\n");

    // With a real window in force, the just-promoted version is refused…
    let run_cooled = |args: &[&str]| {
        let mut cmd = Command::new(BIN);
        cmd.current_dir(&app)
            .env("WITCHY_HOME", &sb.home)
            .env("WITCHY_USER", "dev")
            .env("WITCHY_COOLDOWN_SECS", "3600")
            .args(args);
        if let Some(u) = &sb.coven_url {
            cmd.env("COVEN_URL", u);
        }
        cmd.output().expect("spawn witchy")
    };
    let out = run_cooled(&["add", "acme/fresh"]);
    assert!(!out.status.success(), "a release inside its cooldown must not resolve");
    let msg = stderr(&out);
    assert!(
        msg.contains("staging cooldown") && msg.contains("--allow-fresh"),
        "the refusal should explain the window and the override: {msg}"
    );

    // …and `--allow-fresh` is the explicit acceptance.
    let out = run_cooled(&["add", "acme/fresh", "--allow-fresh"]);
    assert!(out.status.success(), "--allow-fresh should accept: {}", stderr(&out));
}

/// The committed `examples/projects/todo` workspace — a `todo` app that depends
/// on a sibling `tasklib` library via a path dependency and reads its checklist
/// with a read-only `Dir` capability — builds and runs end to end. Copied into a
/// hermetic sandbox so the test never mutates the repo (or its lockfile).
#[test]
fn example_todo_workspace_runs_with_a_path_dependency() {
    let sb = Sandbox::new("ex-todo");
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects/todo");
    copy_tree(&srcroot, &sb.work);
    let out = sb.run(&sb.work.join("todo"), "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("[x] Decompose Dir into Read / Write"), "rendered board missing: {s}");
    assert!(s.contains("[ ] Implement a real UDP transport"), "pending item missing: {s}");
    assert!(s.contains("3 / 5 done"), "summary missing: {s}");
}

/// The committed `examples/projects/ledger` workspace — a bank-account *actor*
/// (granted Console at spawn, isolated balance state, FIFO message handlers) that
/// formats amounts via a `money` library rune (a path dependency) — builds and
/// runs end to end. Copied into a hermetic sandbox so the repo is never touched.
#[test]
fn example_ledger_workspace_runs_with_actors_and_a_path_dependency() {
    let sb = Sandbox::new("ex-ledger");
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects/ledger");
    copy_tree(&srcroot, &sb.work);
    let out = sb.run(&sb.work.join("ledger"), "dev", &["run"]);
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
    let sb = Sandbox::new("ex-report");
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects/report");
    copy_tree(&srcroot, &sb.work);
    let out = sb.run(&sb.work.join("report"), "dev", &["run"]);
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
    let sb = Sandbox::new("ex-dash");
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects/dashboard");
    copy_tree(&srcroot, &sb.work);
    let app = sb.work.join("dashboard");

    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("tasks    [####----]  50%"), "tasks widget missing: {s}");
    assert!(s.contains("coverage [######--]  75%"), "coverage widget missing: {s}");

    // The diamond: `bars` is reached via both `tasks` and `coverage`, but the
    // resolver shares it — the tree marks the second occurrence with `(*)`.
    let tree = sb.run(&app, "dev", &["tree"]);
    assert!(tree.status.success(), "tree failed: {}", stderr(&tree));
    assert!(stdout(&tree).contains("bars@0.1.0 (*)"), "shared base not deduplicated: {}", stdout(&tree));
}

/// The committed `examples/projects/config` workspace — a `greet` app that reads
/// a "key = value" file (via a read-only `Dir`), parses it with the `kv` library
/// rune (a path dependency), and composes a greeting with `Result`/`?` error
/// handling. Runs the happy path; a `?`-propagated missing key is covered by the
/// project's own design (and exercised manually).
#[test]
fn example_config_workspace_runs_with_result_error_handling() {
    let sb = Sandbox::new("ex-config");
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects/config");
    copy_tree(&srcroot, &sb.work);
    let app = sb.work.join("greet");

    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(stdout(&out).trim() == "Hello, witchy!", "greeting wrong: {}", stdout(&out));

    // Drop a required key: `?` short-circuits to the friendly Err message.
    std::fs::write(app.join("config.kv"), "greeting = Hi\n").unwrap();
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(
        stdout(&out).contains("config error: missing config key: name"),
        "error path wrong: {}",
        stdout(&out)
    );
}

/// The committed `examples/projects/sales` workspace — a `sales` app that reads a
/// CSV of sales (via a read-only `Dir`) and aggregates per-product revenue with
/// the `salelib` rune (a path dependency using std `csv` + `dict`). It builds and
/// runs, parsing a quoted field with an embedded comma and folding into a Dict.
#[test]
fn example_sales_workspace_aggregates_csv_with_dict() {
    let sb = Sandbox::new("ex-sales");
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects/sales");
    copy_tree(&srcroot, &sb.work);
    let out = sb.run(&sb.work.join("sales"), "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("gadget          $125"), "gadget total: {s}");
    // The CSV field "gizmo, deluxe" is quoted; its embedded comma survives parsing.
    assert!(s.contains("gizmo, deluxe   $200"), "quoted-field product: {s}");
    assert!(s.contains("widget          $150"), "widget total: {s}");
    assert!(s.contains("Total: $475"), "grand total: {s}");
}

/// The committed `examples/projects/wordfreq` workspace — a `wordfreq` app that
/// reads a text file (via a read-only `Dir`) and ranks the most common words with
/// the `wordlib` rune (a path dependency using std `string`/`ascii`/`dict`/
/// `list`). It builds and runs, normalizing case + punctuation and breaking
/// count ties alphabetically for a deterministic top-5. Whitespace is collapsed
/// so the assertion checks content and order, not the column padding.
#[test]
fn example_wordfreq_workspace_ranks_words() {
    let sb = Sandbox::new("ex-wordfreq");
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects/wordfreq");
    copy_tree(&srcroot, &sb.work);
    let out = sb.run(&sb.work.join("wordfreq"), "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    let collapsed = stdout(&out).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        collapsed.contains("Top words: the 6 fox 4 dog 3 quick 3 brown 2"),
        "ranking wrong: {collapsed}"
    );
}

/// The committed `examples/projects/convert` workspace — a `convert` app that
/// reads `input.csv` and WRITES `output.json` through one read-write Dir
/// capability, converting with the `convertlib` rune (std `csv` + `json`). The
/// first example project to exercise Dir *Write*: it asserts the file the app
/// produced — header column order preserved, integers as JSON numbers.
#[test]
fn example_convert_workspace_writes_json_via_dir_write() {
    let sb = Sandbox::new("ex-convert");
    let srcroot = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/projects/convert");
    copy_tree(&srcroot, &sb.work);
    let app = sb.work.join("convert");
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("wrote output.json"), "stdout: {}", stdout(&out));
    let written = std::fs::read_to_string(app.join("output.json")).expect("app must write output.json");
    assert!(written.trim_start().starts_with('['), "must be a JSON array: {written}");
    assert!(written.contains("\"name\": \"Ada\""), "name field: {written}");
    // An integer column becomes an unquoted JSON number, not a string.
    assert!(written.contains("\"age\": 36"), "age must be a number: {written}");
    assert!(written.contains("\"city\": \"London\""), "city field: {written}");
    assert!(written.contains("Grace") && written.contains("NYC"), "second row: {written}");
}

#[test]
fn published_rune_cannot_have_path_dependency() {
    let sb = Sandbox::new("nopath");
    let app = new_app(&sb);
    // A published rune that tries to reach into the consumer's filesystem.
    let dir = sb.work.join("sneaky");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/sneaky\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"x\" = { path = \"../x\" }\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/sneaky.witchy"), "fn f(s: String) -> String:\n    s\n").unwrap();
    assert!(sb.run(&dir, "ci-bot", &["publish"]).status.success());
    assert!(sb
        .run(&dir, "alice", &["promote", "acme/sneaky@1.0.0", "--factor", "totp"])
        .status
        .success());

    let out = sb.run(&app, "dev", &["add", "acme/sneaky"]);
    assert!(!out.status.success(), "registry rune with a path dep must be refused");
    assert!(stderr(&out).contains("path"), "stderr: {}", stderr(&out));
}

#[test]
fn build_rejects_underdeclared_capabilities() {
    let sb = Sandbox::new("declared");
    let app = new_app(&sb);
    // A library rune that demands Net but declares only Console.
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"lib\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/app.witchy"),
        "fn fetch(net: Net, u: String) -> String:\n    u\n",
    )
    .unwrap();
    let out = sb.run(&app, "dev", &["build"]);
    assert!(!out.status.success(), "under-declared caps must fail build");
    assert!(stderr(&out).contains("under-declare"), "stderr: {}", stderr(&out));
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
    for _ in 0..80 {
        if std::net::TcpStream::connect(&addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
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
        lines.contains(&"timestamp:200 verified=true"),
        "TUF timestamp must verify: {lines:?}"
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
    for _ in 0..80 {
        if std::net::TcpStream::connect(&addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
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
        "{{\"manifest_toml\":{},\"source\":{source},\"id_token\":{token}}}",
        json_str(manifest)
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
        "{{\"manifest_toml\":{},\"source\":{source3},\"id_token\":{evil_token}}}",
        json_str(manifest3)
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
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let coven_src = format!("{manifest_dir}/projects/coven/src/coven.witchy");
    let pm_src = format!("{manifest_dir}/projects/pm/src/pm.witchy");

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let store = unique("witchy-pmadd-store");
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
        .expect("spawn witchy coven");
    let mut up = false;
    for _ in 0..80 {
        if std::net::TcpStream::connect(&addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if !up {
        let _ = server.kill();
        let _ = server.wait();
        panic!("witchy coven never started on {addr}");
    }

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
            "--net",
            &addr,
            &pm_src,
            "add",
            "acme/money",
            "*",
            &addr,
            "vendor",
        ])
        .current_dir(&dest)
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
        .args([pm_src.as_str(), "verify-rune", "vendor/money", rootpub])
        .current_dir(&dest)
        .output()
        .expect("run pm verify-rune");
    let verify_out = String::from_utf8_lossy(&verify.stdout).to_string();

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
}

/// Self-hosted yank: the witchy coven marks a version yanked and the witchy pm's
/// resolver skips it. With 1.0.0 and 2.0.0 both released, `*` resolves to 2.0.0;
/// after 2.0.0 is yanked, `*` falls back to 1.0.0 — a yanked version is excluded
/// from new resolutions (existing locks would still pin it).
#[test]
fn witchy_coven_yank_excludes_from_resolution() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let coven_src = format!("{manifest_dir}/projects/coven/src/coven.witchy");
    let pm_src = format!("{manifest_dir}/projects/pm/src/pm.witchy");

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let store = unique("witchy-yank-store");
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
        .expect("spawn witchy coven");
    let mut up = false;
    for _ in 0..80 {
        if std::net::TcpStream::connect(&addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if !up {
        let _ = server.kill();
        let _ = server.wait();
        panic!("witchy coven never started on {addr}");
    }

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
            .args(["--net", &addr, &pm_src, "add", "acme/money", "*", &addr, "vendor"])
            .current_dir(&dest)
            .output()
            .expect("run pm add");
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let _ = std::fs::remove_dir_all(&dest);
        stdout
    };

    // Both released: `*` resolves to the highest, 2.0.0.
    let before = add_star();
    // Yank 2.0.0, then `*` must fall back to 1.0.0.
    let (yank_status, yank_body) =
        http_post(&addr, "/coven/yank", "{\"name\":\"acme~money\",\"version\":\"2.0.0\"}");
    let after = add_star();

    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&store);

    assert!(
        before.contains("added acme/money@2.0.0"),
        "before yank, * resolves to 2.0.0: {before:?}"
    );
    assert_eq!(yank_status, 200, "yank should succeed: {yank_body}");
    assert!(
        after.contains("added acme/money@1.0.0"),
        "after yank, * falls back to 1.0.0: {after:?}"
    );
}

/// Transitive resolution, self-hosted: publishing `acme/app` whose manifest
/// declares a version dependency on `acme/util`, then `pm add acme/app` fetches
/// BOTH — app and, by following app's `[dependencies]`, util. Each node is
/// integrity-verified and carries its coven.json.
#[test]
fn witchy_pm_add_resolves_transitive_dependencies() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let coven_src = format!("{manifest_dir}/projects/coven/src/coven.witchy");
    let pm_src = format!("{manifest_dir}/projects/pm/src/pm.witchy");

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let store = unique("witchy-trans-store");
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
        .expect("spawn witchy coven");
    let mut up = false;
    for _ in 0..80 {
        if std::net::TcpStream::connect(&addr).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if !up {
        let _ = server.kill();
        let _ = server.wait();
        panic!("witchy coven never started on {addr}");
    }

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
        .args(["--net", &addr, &pm_src, "add", "acme/app", "*", &addr, "vendor"])
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
        .args([pm_src.as_str(), "verify-vendor", "vendor", rootpub])
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
