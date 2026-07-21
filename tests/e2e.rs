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

#[path = "e2e/trust_and_publishing.rs"]
mod trust_and_publishing;
#[path = "e2e/capability_widening.rs"]
mod capability_widening;
#[path = "e2e/resolution.rs"]
mod resolution;
#[path = "e2e/build_steps.rs"]
mod build_steps;
#[path = "e2e/example_workspaces.rs"]
mod example_workspaces;
#[path = "e2e/coven_web.rs"]
mod coven_web;

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
