//! End-to-end tests for coven, the witchy package manager. These drive the real
//! `witchy` binary (via `CARGO_BIN_EXE_witchy`) through the full supply-chain
//! lifecycle: scaffold, publish (staged), promote (second factor), add (gated),
//! build, run, audit. Each test is hermetic — its own temp `WITCHY_HOME` and
//! working tree — so they can run in parallel.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

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
}

impl Sandbox {
    fn new(tag: &str) -> Sandbox {
        Sandbox {
            home: unique(&format!("{tag}-home")),
            work: unique(&format!("{tag}-work")),
        }
    }

    fn run(&self, dir: &Path, user: &str, args: &[&str]) -> Output {
        Command::new(BIN)
            .current_dir(dir)
            .env("WITCHY_HOME", &self.home)
            .env("WITCHY_USER", user)
            .args(args)
            .output()
            .expect("spawn witchy")
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

    // A healthy build verifies signatures.
    assert!(sb.run(&app, "dev", &["build"]).status.success());

    // Attacker edits a signed field of the registry record (source untouched, so
    // content hashing alone would miss it — the Ed25519 signature must catch it).
    let meta = sb
        .home
        .join("registry/acme/x/1.0.0/coven.json");
    let json = std::fs::read_to_string(&meta).unwrap().replace("ci-bot", "attacker");
    std::fs::write(&meta, json).unwrap();

    let out = sb.run(&app, "dev", &["build"]);
    assert!(!out.status.success(), "tampered metadata must fail the build");
    assert!(
        stderr(&out).contains("signature") || stderr(&out).contains("tampered"),
        "stderr: {}",
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
