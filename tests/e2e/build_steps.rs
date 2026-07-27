//! e2e: build steps tests (extracted from tests/e2e.rs).

use std::process::Command;
use std::path::PathBuf;

use super::support::coven::*;

struct BuildWorkspace {
    work: PathBuf,
    app: PathBuf,
    lib: PathBuf,
}

impl BuildWorkspace {
    fn new(tag: &str, lib_manifest: &str, app_manifest: &str) -> Self {
        let work = unique(tag);
        let lib = work.join("genlib");
        let app = work.join("app");
        std::fs::create_dir_all(lib.join("src")).unwrap();
        std::fs::create_dir_all(app.join("src")).unwrap();
        std::fs::write(lib.join("witchy.toml"), lib_manifest).unwrap();
        std::fs::write(app.join("witchy.toml"), app_manifest).unwrap();
        Self { work, app, lib }
    }

    fn write_lib(&self, source: &str, build: &str) {
        std::fs::write(self.lib.join("src/genlib.witchy"), source).unwrap();
        std::fs::write(self.lib.join("src/build.witchy"), build).unwrap();
    }

    fn write_app(&self, source: &str) {
        std::fs::write(self.app.join("src/app.witchy"), source).unwrap();
    }

    fn finish(self) {
        let _ = std::fs::remove_dir_all(self.work);
    }
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
    let fixture = BuildWorkspace::new(
        "builddeny",
        "[rune]\nname = \"safegen\"\nversion = \"0.1.0\"\n",
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n\n[dependencies]\n\"safegen\" = { path = \"../genlib\" }\n",
    );
    fixture.write_lib(
        "pub fn shout(s: String) -> String:\n    \"HEY \" + s\n",
        "fn build(out: BuildOut):\n    out.write_out(\"gen.witchy\", \"// generated\")\n",
    );
    fixture.write_app("fn main(console: Console):\n    console.print(\"ok\")\n");
    let work = &fixture.work;
    let app = &fixture.app;

    // Default-deny: the build refuses while no [build.grants."safegen"] section
    // exists at all — you consent to ANY build-time code execution first.
    let out = pm_fe(work, &["build", "app"]);
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
    let out = pm_fe(work, &["build", "app"]);
    assert!(
        out.status.success(),
        "an empty grants section accepts a BuildOut-only step: {}\n{}",
        stderr(&out),
        stdout(&out)
    );

    fixture.finish();
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
    let fixture = BuildWorkspace::new(
        "autorun",
        "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n",
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n\n[dependencies]\n\"genlib\" = { path = \"../genlib\" }\n\n[build.grants.\"genlib\"]\n",
    );
    fixture.write_lib(
        "pub fn id(s: String) -> String:\n    s\n",
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    out.write_out(\"greet.witchy\", \"pub fn greeting() -> String:\" + nl + \"    \\\"hi from generated code\\\"\" + nl)\n",
    );
    fixture.write_app("import greet\n\nfn main(console: Console):\n    console.print(greet.greeting())\n");
    let work = &fixture.work;
    let lib = &fixture.lib;
    let out = pm_fe(work, &["run", "app"]);
    assert!(out.status.success(), "auto-run build step + import failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("hi from generated code"), "got: {}", stdout(&out));

    // Now the build step turns malicious: it *generates* capability-hungry
    // source. The step itself still demands only BuildOut — but the
    // post-generation audit (footprint over shipped + generated) refuses the
    // smuggle of new runtime authority (Net) into the consumer's link.
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    out.write_out(\"greet.witchy\", \"pub fn evil(n: Net, addr: String) -> Socket:\" + nl + \"    n.connect(addr)\" + nl)\n",
    )
    .unwrap();
    // This is an intentional path-dependency edit, so refresh its content lock.
    // The refreshed shipped footprint still contains only BuildOut; the dynamic
    // generated Net demand is what the post-generation gate must catch.
    let relock = pm_fe(work, &["lock", "app"]);
    assert!(relock.status.success(), "relock malicious build-step fixture: {}", stdout(&relock));
    let out = pm_fe(work, &["run", "app"]);
    assert!(!out.status.success(), "generated widening must be refused");
    assert!(
        stdout(&out).contains("WIDENS its footprint"),
        "the refusal should explain the smuggle: {}",
        stdout(&out)
    );

    fixture.finish();
}

/// A deterministic build step (no BuildExec/BuildNet) is cached by its inputs
/// (§7.2): a second build with unchanged inputs reuses the prior output instead
/// of re-running. We prove the *hit* by corrupting the cached output and leaving
/// the cache key intact — a cache hit serves the corrupted bytes, a miss would
/// regenerate the original. Driven through the front-end (`witchy pm run`).
#[test]
fn deterministic_build_output_is_cached() {
    let fixture = BuildWorkspace::new(
        "buildcache",
        "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n",
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[capabilities]\nruntime = [\"Console\"]\n\n[dependencies]\n\"genlib\" = { path = \"../genlib\" }\n\n[build.grants.\"genlib\"]\n",
    );
    fixture.write_lib(
        "pub fn id(s: String) -> String:\n    s\n",
        "fn build(out: BuildOut):\n    let nl = \"\\n\"\n    out.write_out(\"greet.witchy\", \"pub fn greeting() -> String:\" + nl + \"    \\\"V1\\\"\" + nl)\n",
    );
    fixture.write_app("import greet\n\nfn main(console: Console):\n    console.print(greet.greeting())\n");
    let work = &fixture.work;
    let app = &fixture.app;

    let out = pm_fe(work, &["run", "app"]);
    assert!(out.status.success(), "first run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(stdout(&out).contains("V1"), "got: {}", stdout(&out));

    // Corrupt the generated output, keep the cache key.
    let gen_file = app.join("build-out/genlib/greet.witchy");
    let body = std::fs::read_to_string(&gen_file).unwrap().replace("V1", "CACHED");
    std::fs::write(&gen_file, body).unwrap();

    let out = pm_fe(work, &["run", "app"]);
    assert!(out.status.success(), "cached run failed: {}\n{}", stderr(&out), stdout(&out));
    assert!(
        stdout(&out).contains("CACHED"),
        "a deterministic build step should be cached (got: {})",
        stdout(&out)
    );

    fixture.finish();
}

/// (BUG-100) A dependency's build step generates source that the consumer imports;
/// the front-end audits each generated file and emits one `--dep <module>=<path>`
/// per module (`audit_then_flags`). Those flags must be de-duped by module name —
/// `dep_flag` is idempotent — so a build that emits several modules yields exactly
/// one flag each, and the consumer links and runs against the generated code. This
/// exercises the build-step → audit → compile path end to end.
#[test]
fn build_step_generated_deps_link_and_run() {
    let fixture = BuildWorkspace::new(
        "build-step-deps",
        "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n",
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"genlib\" = { path = \"../genlib\" }\n\n[build.grants.\"genlib\"]\n",
    );
    let work = &fixture.work;
    let app = &fixture.app;
    let lib = &fixture.lib;

    // The app depends on `genlib` and accepts its build step (empty grants section
    // permits only the confined BuildOut sandbox).
    std::fs::write(
        app.join("src/app.witchy"),
        "import genmod\nimport genmod2\n\nfn main(console: Console):\n    console.print(\"${genmod.value() + genmod2.value()}\")\n",
    )
    .unwrap();

    std::fs::write(lib.join("src/genlib.witchy"), "pub fn placeholder() -> Int:\n    0\n").unwrap();
    // The build step emits TWO modules; audit_then_flags must produce one --dep per
    // module (deduped by name), not duplicates.
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut):\n    out.write_out(\"genmod.witchy\", \"pub fn value() -> Int:\\n    40\\n\")\n    out.write_out(\"genmod2.witchy\", \"pub fn value() -> Int:\\n    2\\n\")\n",
    )
    .unwrap();

    let out = Command::new(BIN)
        .current_dir(work)
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
    fixture.finish();
}

/// RFC-0103: the package-manager grant accepts the richer BuildExec form,
/// forwards its child-only paths to the isolated build child, and does not turn
/// them into BuildRead authority for the Witchy build program.
#[test]
fn build_exec_child_paths_flow_through_the_package_manager() {
    let fixture = BuildWorkspace::new(
        "build-exec-child-paths",
        "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n",
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n",
    );
    let work = &fixture.work;
    let app = &fixture.app;
    let lib = &fixture.lib;
    std::fs::create_dir_all(work.join("child-config")).unwrap();
    std::fs::write(work.join("child-config/tool.conf"), "tool-only").unwrap();

    std::fs::write(
        app.join("witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"0.1.0\"\n\n\
         [capabilities]\nruntime = [\"Console\"]\n\n\
         [dependencies]\n\"genlib\" = { path = \"../genlib\" }\n\n\
         [build.grants.\"genlib\"]\n\
         exec = { programs = [\"cat\"], child-paths = [\"../child-config\"] }\n",
    )
    .unwrap();
    std::fs::write(
        app.join("src/app.witchy"),
        "import genmod\nfn main(console: Console):\n    console.print(\"${genmod.value()}\")\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("witchy.toml"),
        "[rune]\nname = \"genlib\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("src/genlib.witchy"),
        "pub fn placeholder() -> Int:\n    0\n",
    )
    .unwrap();
    std::fs::write(
        lib.join("src/build.witchy"),
        "fn build(out: BuildOut, tool: BuildExec):\n    out.write_out(\"genmod.witchy\", tool.run_tool(\"cat\", \"pub fn value() -> Int:\\n    42\\n\"))\n",
    )
    .unwrap();

    let out = Command::new(BIN)
        .current_dir(work)
        .args(["run", "app"])
        .output()
        .expect("spawn witchy run");
    assert!(
        out.status.success() && stdout(&out).contains("42"),
        "richer BuildExec grant must build and run: status {:?} stdout {} stderr {}",
        out.status.code(),
        stdout(&out),
        stderr(&out),
    );
    fixture.finish();
}
