//! e2e: resolution tests (extracted from tests/e2e.rs).

use std::process::Command;

use super::support::coven::*;

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

/// A fetched rune carries its signed provenance and every consumption path
/// re-verifies it offline. `verify`, `build`, and the narrow `verify-rune` command
/// all bind the vendored source to the signed record, lock coordinate, and pinned
/// registry root key without contacting the registry.
#[test]
fn vendored_rune_reverifies_offline_against_the_root_key() {
    let (app, rootpub) = {
        let server = RegistryServer::start();
        let fe = FrontEnd::new(&server, "pin");
        let app = fe.new_app();
        fe.published_lib("acme/yankee", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
        let out = fe.pm(&app, &["add", "acme/yankee"], None);
        assert!(out.status.success(), "add failed: {}", stderr(&out));
        std::fs::write(
            app.join("src/app.witchy"),
            "import yankee\n\nfn main(console: Console):\n    console.print(yankee.f(\"vend\"))\n",
        )
        .unwrap();
        let rootpub = server.rootpub();
        // Keep the vendored fixture after the front-end and registry are dropped.
        std::mem::forget(fe);
        (app, rootpub)
    };

    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("hash = \"sha256:"), "lock must pin the content hash: {lock}");

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

    // The registry is gone: all consumption uses only the vendored source,
    // signed record, lock coordinate, and copied root key.
    let out = offline(&["verify-rune", "vendor/yankee", &rootpub]);
    assert!(out.status.success(), "verify-rune failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("verified"), "verify-rune: {}", stdout(&out));
    for (args, expected) in [
        (&["verify"][..], Some("every locked hash matches")),
        (&["build", "."][..], None),
        (&["run", "."][..], Some("vend")),
    ] {
        let out = offline(args);
        assert!(out.status.success(), "offline {args:?} failed: {}\n{}", stderr(&out), stdout(&out));
        if let Some(expected) = expected {
            assert!(stdout(&out).contains(expected), "{args:?}: {}", stdout(&out));
        }
    }

    // Tampering the vendored source breaks the content check.
    let source_path = app.join("vendor/yankee/src/yankee.witchy");
    let original_source = std::fs::read_to_string(&source_path).unwrap();
    std::fs::write(&source_path, "pub fn f(s: String) -> String:\n    \"evil\"\n").unwrap();
    let out = offline(&["verify-rune", "vendor/yankee", &rootpub]);
    assert!(!out.status.success(), "tampered source must fail re-verification");
    assert!(stdout(&out).contains("BLOCK"), "verify-rune: {}", stdout(&out));
    for args in [&["verify"][..], &["build", "."][..], &["run", "."][..]] {
        let out = offline(args);
        assert!(!out.status.success(), "tampered source must fail {args:?}");
        assert!(stdout(&out).contains("source no longer matches"), "{args:?}: {}", stdout(&out));
    }

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
    let _ = std::fs::remove_dir_all(app.parent().unwrap());
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

/// `pm outdated <dir> <host:port>` reports, for each registry dependency the
/// manifest declares, its requirement and the latest released version available.
/// Initially the latest matches the requirement floor; after a newer version is
/// published + promoted, `outdated` surfaces it.
#[test]
fn outdated_reports_newer_versions() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "outdated");
    fe.published_lib("acme/lib", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    // CJ-06: a second dep declared in the BARE-STRING form (`"acme/bare" = "^1.0.0"`),
    // which the dep walk previously skipped silently.
    fe.published_lib("acme/bare", "1.0.0", "pub fn g(s: String) -> String:\n    s\n");

    // A consumer that declares registry dependencies on acme/lib (inline-table form)
    // and acme/bare (bare-string form) — `outdated` must report BOTH.
    let app = fe.new_app();
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"acme/lib\" = { version = \"^1.0.0\" }\n\"acme/bare\" = \"^1.0.0\"\n",
    )
    .unwrap();
    let hostport = format!("127.0.0.1:{}", server.port);

    // Only 1.0.0 exists so far.
    let out = fe.pm(&app, &["--net", &hostport, "outdated", ".", &hostport], None);
    assert!(out.status.success(), "outdated failed: {}", stderr(&out));
    assert!(stdout(&out).contains("acme/lib: req ^1.0.0, latest 1.0.0"), "outdated: {}", stdout(&out));
    assert!(
        stdout(&out).contains("acme/bare: req ^1.0.0, latest 1.0.0"),
        "outdated must walk a bare-string dep too (CJ-06): {}",
        stdout(&out)
    );

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
    // CJ-01: the refusal must be actionable — it states how long remains before
    // the release becomes resolvable (the 3600s window minus its just-elapsed age,
    // rendered as a compact duration like `59m`/`1h`). The window is one hour, so
    // the remaining wait renders in hours or minutes.
    assert!(
        msg.contains("for another ") && (msg.contains("m ") || msg.contains("h ")),
        "the refusal should state the remaining wait time: {msg}"
    );
    assert!(!app.join("witchy.lock").exists(), "a cooled-out add must write nothing");

    // …and `--allow-fresh` is the explicit acceptance.
    let out = run_cooled(&["add", "acme/fresh", "--allow-fresh"]);
    assert!(out.status.success(), "--allow-fresh should accept: {}", stderr(&out));
    assert!(app.join("vendor/fresh/src/fresh.witchy").exists(), "accepted add must vendor");
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

/// CJ-03: `witchy pm --help` / `-h` / `help` print the usage table and exit 0
/// with no `unknown command` error line — a help request is not an error.
#[test]
fn pm_help_flag_prints_usage_and_exits_zero() {
    for flag in ["--help", "-h", "help"] {
        let out = Command::new(BIN).args(["pm", flag]).output().expect("spawn witchy pm");
        assert!(out.status.success(), "`pm {flag}` should exit 0: {}", stderr(&out));
        let s = format!("{}{}", stdout(&out), stderr(&out));
        assert!(s.contains("usage: pm <command>"), "`pm {flag}` should print usage: {s}");
        assert!(!s.contains("unknown command"), "`pm {flag}` must not print an error: {s}");
    }
}

/// CJ-05: a diamond/version conflict is REPORTED, not silently resolved to the
/// first-vendored version. `acme/foo` depends on `acme/base ^1`, `acme/bar` on
/// `acme/base ^2` (incompatible). Adding foo vendors base@1.0.0; a later add of
/// bar needs a base that 1.x does not satisfy, so pm BLOCKS with an actionable
/// conflict message naming the package, the vendored version, and the unmet
/// requirement — instead of keeping base@1 (pm does no unification/backtracking).
#[test]
fn diamond_version_conflict_is_reported() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "diamond");

    // Two incompatible majors of the shared dep.
    fe.published_lib("acme/base", "1.0.0", "pub fn f(s: String) -> String:\n    s\n");
    fe.published_lib("acme/base", "2.0.0", "pub fn f(s: String) -> String:\n    s\n");

    // foo -> base ^1, bar -> base ^2.
    let publish_dep = |name: &str, base_req: &str| {
        let stem = name.rsplit('/').next().unwrap();
        let dir = fe.base.join(stem);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("witchy.toml"),
            format!(
                "[rune]\nname = \"{name}\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/base\" = {{ version = \"{base_req}\" }}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("src").join(format!("{stem}.witchy")),
            "pub fn h(s: String) -> String:\n    s\n",
        )
        .unwrap();
        fe.publish_promote(&dir, name, "1.0.0");
    };
    publish_dep("acme/foo", "^1.0.0");
    publish_dep("acme/bar", "^2.0.0");

    let app = fe.new_app();

    // Adding foo vendors base@1.0.0 (foo's ^1 requirement resolves within it).
    let out = fe.pm(&app, &["add", "acme/foo"], None);
    assert!(out.status.success(), "add foo failed: {}\n{}", stdout(&out), stderr(&out));
    assert!(app.join("vendor/base/coven.json").exists(), "foo's add should vendor base");

    // Adding bar needs base ^2, but base is already vendored at 1.0.0 → CONFLICT.
    let out = fe.pm(&app, &["add", "acme/bar"], None);
    assert!(
        !out.status.success(),
        "a version conflict must fail the add, not silently keep base@1: {}\n{}",
        stdout(&out),
        stderr(&out)
    );
    let msg = format!("{}{}", stdout(&out), stderr(&out));
    assert!(
        msg.contains("version conflict")
            && msg.contains("acme/base")
            && msg.contains("1.0.0")
            && msg.contains("^2.0.0"),
        "the conflict must name the package, vendored version, and unmet requirement (CJ-05): {msg}"
    );
}
