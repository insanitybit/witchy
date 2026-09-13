//! BuildEnv preserves present/empty/unset/denied through ordinary project builds.
use std::process::{Command, Output};

use super::temp_dir::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const ALLOWED: &str = "WITCHY_BUILD_ENV_PROJECT_LABEL";
const DENIED: &str = "WITCHY_BUILD_ENV_PROJECT_DENIED";

fn project(tag: &str, requested_key: &str) -> TempDir {
    let dir = TempDir::new(tag);
    dir.write("app/witchy.toml", format!(r#"[rune]
name = "env-app"
version = "0.1.0"

[capabilities]
runtime = ["Console"]

[dependencies]
generator = {{ path = "../generator" }}

[build.grants."generator"]
env = ["{ALLOWED}"]
"#));
    // Path dependencies must use their manifest's package identity.
    dir.write("generator/witchy.toml",
        "[rune]\nname = \"generator\"\nversion = \"0.1.0\"\n[dependencies]\n");
    dir.write("generator/src/generator.witchy",
        "pub fn placeholder() -> String:\n    \"shipped\"\n");
    dir.write("generator/src/build.witchy", format!(
        "fn build(env: BuildEnv, out: BuildOut):\n    let get = fn(): env.get_build_env(\"{requested_key}\")\n    let text = get() ?? \"unset\"\n    out.write_out(\"genmod.witchy\", \"pub fn value() -> String:\\n    \\\"\" + text + \"\\\"\\n\")\n"
    ));
    dir.write("app/src/app.witchy",
        "import genmod\nfn main(console: Console):\n    console.print(genmod.value())\n");
    dir
}

fn build(dir: &TempDir, value: Option<&str>) -> Output {
    let mut command = Command::new(BIN);
    command.current_dir(dir).env("WITCHY_HOME", dir.join("home"))
        .env(DENIED, "ungranted-environment-secret")
        .args(["build", "app"]);
    match value {
        Some(value) => { command.env(ALLOWED, value); }
        None => { command.env_remove(ALLOWED); }
    }
    command.output().expect("spawn ordinary project build")
}

fn diagnostic(output: &Output) -> String {
    format!("status={:?}\nstdout={}\nstderr={}", output.status.code(),
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn ordinary_project_build_env_preserves_present_empty_and_unset() {
    for (tag, value, expected) in [
        ("project-build-env-present", Some("caf\u{e9} build environment"), "caf\u{e9} build environment"),
        ("project-build-env-empty", Some(""), ""),
        ("project-build-env-unset", None, "unset"),
    ] {
        // Fresh output per case: this does not claim that the existing build
        // cache tracks environment value changes across otherwise identical runs.
        let dir = project(tag, ALLOWED);
        let output = build(&dir, value);
        assert_eq!(output.status.code(), Some(0), "{}", diagnostic(&output));
        let generated = std::fs::read_to_string(dir.join("app/build-out/generator/genmod.witchy"))
            .expect("generated source from captured environment");
        assert_eq!(generated, format!("pub fn value() -> String:\n    \"{expected}\"\n"));
        assert!(dir.join("app/witchy.lock").is_file());
        assert!(dir.join("app/build-out/generator/.witchy-build-cache").is_file());
    }
}

#[test]
fn ordinary_project_build_env_denies_unlisted_key_instead_of_returning_none() {
    let dir = project("project-build-env-denied", DENIED);
    let output = build(&dir, Some("allowed"));
    let trace = diagnostic(&output);
    assert_eq!(output.status.code(), Some(1), "{trace}");
    assert!(trace.contains(&format!(
        "get_build_env: `{DENIED}` is not in this BuildEnv grant's allow-list")),
        "must reach the key authorization refusal: {trace}");
    assert!(!trace.contains("ungranted-environment-secret"), "{trace}");
    assert!(!dir.join("app/build-out/generator/genmod.witchy").exists());
    assert!(!dir.join("app/build-out/generator/.witchy-build-cache").exists());
}
