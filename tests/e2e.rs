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
    std::fs::write(lib.join("src/lib.witchy"), "fn shout(s: String) -> String { \"HEY \" <> s }\n").unwrap();
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
        "import lib\n\nfn main(console: Console) {\n  print(console, lib.shout(\"net\"))\n}\n",
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
    std::fs::write(lib.join("src/secure.witchy"), "fn f(s: String) -> String { s }\n").unwrap();

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
    std::fs::write(lib.join("src/x.witchy"), "fn f(s: String) -> String { s }\n").unwrap();
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
    std::fs::write(lib.join("src/t.witchy"), "fn f(s: String) -> String { s }\n").unwrap();
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
    std::fs::write(lib.join("src/r.witchy"), "fn f(s: String) -> String { s }\n").unwrap();
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
    std::fs::write(lib.join("src/x.witchy"), "fn f(s: String) -> String { s }\n").unwrap();
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
        "fn shout(s: String) -> String {\n  \"HEY \" <> s\n}\n",
    );

    // Add the released library (pure — no capability widening, so no consent needed).
    let out = sb.run(&app, "dev", &["add", "acme/strkit"]);
    assert!(out.status.success(), "add failed: {}", stderr(&out));
    assert!(stdout(&out).contains("demands no capabilities"));

    // Use it from main.
    std::fs::write(
        app.join("src").join("app.witchy"),
        "import strkit\n\nfn main(console: Console) {\n  print(console, strkit.shout(\"witchy\"))\n}\n",
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
    std::fs::write(dir.join("src/json.witchy"), "fn p(s: String) -> String { s }\n").unwrap();
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
    std::fs::write(dir.join("src/lib.witchy"), "fn f(s: String) -> String { s }\n").unwrap();
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
        "fn fetch(net: Net, url: String) -> String {\n  url\n}\n",
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
    sb.publish_lib("acme/url", "1.0.0", "fn parse(s: String) -> String { s }\n");

    // http depends on url and demands Net.
    let dir = sb.work.join("http");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/http\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/url\" = \"^1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/http.witchy"), "fn get(net: Net, u: String) -> String { u }\n").unwrap();
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
        "fn line(s: String) -> String { s }\n",
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
        "fn line(s: String) -> String { s }\nfn beacon(net: Net, s: String) -> String { s }\n",
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
    sb.publish_lib("acme/base", "1.0.0", "fn b(s: String) -> String { s }\n");

    // left and right both depend on base.
    for side in ["left", "right"] {
        let dir = sb.work.join(side);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("witchy.toml"),
            format!("[rune]\nname = \"acme/{side}\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/base\" = \"^1.0.0\"\n"),
        )
        .unwrap();
        std::fs::write(dir.join(format!("src/{side}.witchy")), "fn x(s: String) -> String { s }\n").unwrap();
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
    sb.publish_lib("acme/a", "1.0.0", "fn f(s: String) -> String { s }\n");
    sb.publish_lib("acme/b", "1.0.0", "fn g(s: String) -> String { s }\n");
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
    let lib = sb.publish_lib("acme/old", "1.0.0", "fn f(s: String) -> String { s }\n");

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
    sb.publish_lib("acme/p", "1.0.0", "fn f(s: String) -> String { s }\n");
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
    sb.publish_lib("acme/x", "1.0.0", "fn f(s: String) -> String { s }\n");
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
    sb.publish_lib("acme/y", "1.0.0", "fn f(s: String) -> String { s }\n");
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
    std::fs::write(dir.join("src/list.witchy"), "fn range(n: Int) -> Int { 0 }\n").unwrap();
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
        std::fs::write(dir.join("src/util.witchy"), "fn helper(s: String) -> String { s }\n").unwrap();
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
    sb.publish_lib("acme/lib", "1.0.0", "fn f(s: String) -> String { s }\n");
    assert!(sb.run(&app, "dev", &["add", "acme/lib"]).status.success());
    std::fs::write(
        app.join("src/app.witchy"),
        "import lib\n\nfn main(console: Console) {\n  print(console, lib.f(\"ok\"))\n}\n",
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
    sb.publish_lib("acme/lib", "1.0.0", "fn f(s: String) -> String { s }\n");
    assert!(sb.run(&app, "dev", &["add", "acme/lib"]).status.success());
    std::fs::write(
        app.join("src/app.witchy"),
        "import lib\n\nfn main(console: Console) {\n  print(console, lib.f(\"vend\"))\n}\n",
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
    sb.publish_lib("acme/lib", "1.0.0", "fn f(s: String) -> String { s }\n");
    assert!(sb.run(&app, "dev", &["add", "acme/lib"]).status.success());

    // Up to date initially.
    let out = sb.run(&app, "dev", &["outdated"]);
    assert!(stdout(&out).contains("up to date") || stdout(&out).contains("latest"), "got: {}", stdout(&out));

    // Publish a newer version that demands Net.
    let dir = sb.work.join("lib");
    std::fs::write(dir.join("witchy.toml"), "[rune]\nname = \"acme/lib\"\nversion = \"1.1.0\"\n").unwrap();
    std::fs::write(
        dir.join("src/lib.witchy"),
        "fn f(s: String) -> String { s }\nfn net_f(net: Net, s: String) -> String { s }\n",
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
    sb.publish_lib("acme/url", "1.0.0", "fn parse(s: String) -> String { s }\n");

    let dir = sb.work.join("http");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("witchy.toml"),
        "[rune]\nname = \"acme/http\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/url\" = \"^1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/http.witchy"), "fn get(net: Net, u: String) -> String { u }\n").unwrap();
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
        "fn hi(s: String) -> String { \"hi \" <> s }\n",
    )
    .unwrap();

    // Add it as a path dependency, then use it.
    let out = sb.run(&app, "dev", &["add", "greet", "--path", "../greet"]);
    assert!(out.status.success(), "add --path failed: {}", stderr(&out));
    std::fs::write(
        app.join("src/app.witchy"),
        "import greet\n\nfn main(console: Console) {\n  print(console, greet.hi(\"witchy\"))\n}\n",
    )
    .unwrap();
    let out = sb.run(&app, "dev", &["run"]);
    assert!(out.status.success(), "run failed: {}", stderr(&out));
    assert!(stdout(&out).contains("hi witchy"), "got: {}", stdout(&out));
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
    std::fs::write(dir.join("src/sneaky.witchy"), "fn f(s: String) -> String { s }\n").unwrap();
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
        "fn fetch(net: Net, u: String) -> String { u }\n",
    )
    .unwrap();
    let out = sb.run(&app, "dev", &["build"]);
    assert!(!out.status.success(), "under-declared caps must fail build");
    assert!(stderr(&out).contains("under-declare"), "stderr: {}", stderr(&out));
}
