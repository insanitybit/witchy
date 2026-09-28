//! BuildEnv preserves present/empty/unset/denied through ordinary project builds.
use std::process::{Command, Output};

use super::temp_dir::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const PRESENT: &str = "WITCHY_BUILD_ENV_PROJECT_PRESENT";
const EMPTY: &str = "WITCHY_BUILD_ENV_PROJECT_EMPTY";
const UNSET: &str = "WITCHY_BUILD_ENV_PROJECT_UNSET";
const DENIED: &str = "WITCHY_BUILD_ENV_PROJECT_DENIED";

fn project(tag: &str, allowed_keys: &[&str], build_script: &str) -> TempDir {
    let dir = TempDir::new(tag);
    let env_grant = allowed_keys.iter()
        .map(|key| format!("\"{key}\""))
        .collect::<Vec<_>>()
        .join(", ");
    dir.write("app/witchy.toml", format!(r#"[rune]
name = "env-app"
version = "0.1.0"

[capabilities]
runtime = ["Console"]

[dependencies]
generator = {{ path = "../generator" }}

[build.grants."generator"]
env = [{env_grant}]
"#));
    // Path dependencies must use their manifest's package identity.
    dir.write("generator/witchy.toml",
        "[rune]\nname = \"generator\"\nversion = \"0.1.0\"\n[dependencies]\n");
    dir.write("generator/src/generator.witchy",
        "pub fn placeholder() -> String:\n    \"shipped\"\n");
    dir.write("generator/src/build.witchy", build_script);
    dir.write("app/src/app.witchy",
        "import genmod\nfn main(console: Console):\n    console.print(genmod.present())\n    console.print(genmod.empty())\n    console.print(genmod.unset())\n");
    dir
}

fn build(dir: &TempDir, values: &[(&str, Option<&str>)]) -> Output {
    let mut command = Command::new(BIN);
    command.current_dir(dir).env("WITCHY_HOME", dir.join("home"))
        .env(DENIED, "ungranted-environment-secret")
        .args(["build", "app"]);
    for (key, value) in values {
        match value {
            Some(value) => { command.env(key, value); }
            None => { command.env_remove(key); }
        }
    }
    command.output().expect("spawn ordinary project build")
}

fn diagnostic(output: &Output) -> String {
    format!("status={:?}\nstdout={}\nstderr={}", output.status.code(),
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn ordinary_project_build_env_preserves_present_empty_and_unset() {
    // Read all three states in one build invocation. The values remain
    // independently sourced from the real process environment.
    let build_script = format!(r#"fn build(env: BuildEnv, out: BuildOut):
    let present = env.get_build_env("{PRESENT}") ?? "unset"
    let empty = env.get_build_env("{EMPTY}") ?? "unset"
    let unset = env.get_build_env("{UNSET}") ?? "unset"
    let generated = "pub fn present() -> String:\n    \"" + present + "\"\n\npub fn empty() -> String:\n    \"" + empty + "\"\n\npub fn unset() -> String:\n    \"" + unset + "\"\n"
    out.write_out("genmod.witchy", generated)
"#);
    let dir = project("project-build-env-three-states", &[PRESENT, EMPTY, UNSET], &build_script);
    let output = build(&dir, &[
        (PRESENT, Some("caf\u{e9} build environment")),
        (EMPTY, Some("")),
        (UNSET, None),
    ]);
    assert_eq!(output.status.code(), Some(0), "{}", diagnostic(&output));
    let generated = std::fs::read_to_string(dir.join("app/build-out/generator/genmod.witchy"))
        .expect("generated source from captured environment");
    assert_eq!(generated,
        "pub fn present() -> String:\n    \"caf\u{e9} build environment\"\n\n\
         pub fn empty() -> String:\n    \"\"\n\n\
         pub fn unset() -> String:\n    \"unset\"\n");
    assert!(dir.join("app/witchy.lock").is_file());
    assert!(dir.join("app/build-out/generator/.witchy-build-cache").is_file());
}

#[test]
fn ordinary_project_build_env_denies_unlisted_key_instead_of_returning_none() {
    let build_script = format!("fn build(env: BuildEnv, out: BuildOut):\n    let text = env.get_build_env(\"{DENIED}\") ?? \"unset\"\n    out.write_out(\"genmod.witchy\", text)\n");
    let dir = project("project-build-env-denied", &[PRESENT], &build_script);
    let output = build(&dir, &[(PRESENT, Some("allowed"))]);
    let trace = diagnostic(&output);
    assert_eq!(output.status.code(), Some(1), "{trace}");
    assert!(trace.contains(&format!(
        "get_build_env: `{DENIED}` is not in this BuildEnv grant's allow-list")),
        "must reach the key authorization refusal: {trace}");
    assert!(!trace.contains("ungranted-environment-secret"), "{trace}");
    assert!(!dir.join("app/build-out/generator/genmod.witchy").exists());
    assert!(!dir.join("app/build-out/generator/.witchy-build-cache").exists());
}
