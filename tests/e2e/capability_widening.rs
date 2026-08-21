//! e2e: capability widening tests (extracted from tests/e2e.rs).


use super::support::coven::*;

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

    // Adding it to an empty-footprint app must BLOCK and write nothing.
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
/// rune with a capability-free public API (so its single-rune registry footprint shows nothing)
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

    // An innocent-looking rune: CAPABILITY-FREE public API, but it depends on sneaky.
    let innocent = fe.lib("acme/innocent", "1.0.0", "pub fn greet(s: String) -> String:\n    \"hi \" + s\n");
    std::fs::write(
        innocent.join("witchy.toml"),
        "[rune]\nname = \"acme/innocent\"\nversion = \"1.0.0\"\n\n[dependencies]\n\"acme/sneaky\" = { version = \"^1.0.0\" }\n",
    )
    .unwrap();
    fe.publish_promote(&innocent, "acme/innocent", "1.0.0");

    // Adding the capability-free-looking innocent must BLOCK on the transitive Net.
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
    // transitive `url` (no root capability demand) is pulled by the consented direct add without
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

    // v1.0.0 of a logger: empty root footprint.
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

/// BUG-232: `pm update` must not decide the widening gate from an unverified
/// incoming record. A hostile mirror could under-declare a wider release's
/// `runtime_footprint`; the update must block at the gate before trusting that
/// field or fetching source.
#[test]
fn update_widening_gate_requires_verified_incoming_footprint() {
    let server = RegistryServer::start();
    let fe = FrontEnd::new(&server, "bug232");
    let app = fe.new_app();

    fe.published_lib("acme/logger", "1.0.0", "pub fn line(s: String) -> String:\n    s\n");
    let out = fe.pm(&app, &["add", "acme/logger"], None);
    assert!(out.status.success(), "add v1 failed: {}\n{}", stderr(&out), stdout(&out));

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

    let record = server.regroot.join("registry/acme/logger/1.1.0/coven.json");
    let original = std::fs::read_to_string(&record).unwrap();
    let body = original.replace(r#""runtime_footprint":["Net"]"#, r#""runtime_footprint":[]"#);
    assert_ne!(body, original, "test must actually tamper the incoming record footprint");
    std::fs::write(&record, body).unwrap();

    let out = fe.pm(&app, &["update"], None);
    assert!(!out.status.success(), "tampered incoming footprint must block update");
    assert!(
        stdout(&out).contains("cannot verify the registry record") && stdout(&out).contains("not validly signed"),
        "update must fail at the verified-footprint gate: {}",
        stdout(&out)
    );
    let lock = std::fs::read_to_string(app.join("witchy.lock")).unwrap();
    assert!(lock.contains("1.0.0"), "blocked update must not move the lock: {lock}");
    let vendored = std::fs::read_to_string(app.join("vendor/logger/src/logger.witchy")).unwrap();
    assert!(!vendored.contains("beacon"), "blocked update must not vendor the tampered release: {vendored}");
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
