use super::support::coven::*;
use super::json_str;

use std::process::Command;

pub(crate) fn witchy_pm_add_resolves_and_fetches_from_coven() {
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

pub(crate) fn witchy_coven_yank_excludes_from_resolution() {
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

pub(crate) fn witchy_pm_add_resolves_transitive_dependencies() {
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

pub(crate) fn witchy_pm_check_accepts_net_axis_omission() {
    let work = unique("witchy-pm-rights-net");
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(
        work.join("witchy.toml"),
        "[rune]\nname = \"acme/net-axis\"\nversion = \"1.0.0\"\n\n[capabilities]\nruntime = [\"Net[Connect]\"]\n",
    )
    .unwrap();
    std::fs::write(
        work.join("src/net_axis.witchy"),
        "pub fn fetch(net: Net[Connect, Tcp]) -> Int:\n    1\n",
    )
    .unwrap();

    let out = Command::new(BIN)
        .args(["pm", "check", "."])
        .current_dir(&work)
        .output()
        .expect("run pm check");
    let _ = std::fs::remove_dir_all(&work);

    assert!(
        out.status.success() && stdout(&out).contains("OK: declared footprint admits the code"),
        "pm check should accept Net[Connect] covering Net[Connect, Tcp]: out={} err={}",
        stdout(&out),
        stderr(&out)
    );
}

pub(crate) fn witchy_pm_rejects_uninspectable_source_footprints() {
    let work = unique("witchy-pm-bad-footprint");
    let app = work.join("app");
    let bad = work.join("bad");
    std::fs::create_dir_all(app.join("src")).unwrap();
    std::fs::create_dir_all(bad.join("src")).unwrap();
    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nbad = { path = \"../bad\" }\n",
    )
    .unwrap();
    std::fs::write(app.join("src/app.witchy"), "fn main(console: Console):\n    console.print(\"app\")\n").unwrap();
    std::fs::write(bad.join("witchy.toml"), "[rune]\nname = \"bad\"\nversion = \"0.1.0\"\n").unwrap();
    std::fs::write(bad.join("src/bad.witchy"), "pub fn broken() -> Int:\n    missing()\n").unwrap();
    std::fs::write(work.join("old.witchy"), "fn main(console: Console):\n    console.print(\"ok\")\n").unwrap();
    std::fs::write(work.join("new.witchy"), "fn main(console: Console):\n    missing(console)\n").unwrap();

    let check = Command::new(BIN)
        .args(["pm", "check", "bad"])
        .current_dir(&work)
        .output()
        .expect("run pm check");
    let lock = Command::new(BIN)
        .args(["pm", "lock", "app"])
        .current_dir(&work)
        .output()
        .expect("run pm lock");
    let guard = Command::new(BIN)
        .args(["pm", "guard", "old.witchy", "new.witchy"])
        .current_dir(&work)
        .output()
        .expect("run pm guard");

    assert!(!check.status.success(), "pm check must reject uninspectable source");
    assert!(
        stdout(&check).contains("cannot compute the code's capability footprint")
            && stdout(&check).contains("compiler rejected the source"),
        "pm check erased the compiler error: out={} err={}",
        stdout(&check),
        stderr(&check)
    );
    assert!(!lock.status.success(), "pm lock must reject an uninspectable path dependency");
    assert!(
        stdout(&lock).contains("cannot lock an uninspectable dependency")
            && stdout(&lock).contains("compiler rejected the source"),
        "pm lock erased the compiler error: out={} err={}",
        stdout(&lock),
        stderr(&lock)
    );
    assert!(!app.join("witchy.lock").exists(), "an uninspectable dependency must not be pinned");
    assert!(!guard.status.success(), "pm guard must reject an uninspectable update");
    assert!(
        stdout(&guard).contains("compiler rejected the source"),
        "pm guard erased the compiler error: out={} err={}",
        stdout(&guard),
        stderr(&guard)
    );
    std::fs::remove_dir_all(work).unwrap();
}

pub(crate) fn witchy_pm_local_lifecycle_new_lock_verify_gate() {
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
            && stdout(&verify_ok).contains("every locked hash matches"),
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
        "fn main(console: Console):\n    console.print(\"hello from util\")\n\npub fn fetch(net: Net) -> Int:\n    0\n",
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

pub(crate) fn witchy_coven_promote_delta_immutability_and_error_paths() {
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
    let console_module = "pub fn run(console: Console):\n    console.print(\"hi\")\n";
    let pub_v1 = publish("1.0.0", "\"Console\"", console_module);
    let promote_v1 = promote("1.0.0");
    // Immutability: the released version cannot be re-published.
    let republish = publish("1.0.0", "\"Console\"", console_module);
    // An upgrade that widens: only the NEW authority appears in the delta.
    let net_module = "pub fn run(console: Console):\n    console.print(\"hi\")\n\npub fn fetch(net: Net) -> Int:\n    0\n";
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
