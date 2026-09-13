//! Ordinary project builds must forward manifest grants to admitted build recipes.
use std::process::{Command, Output};

use super::temp_dir::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const GENERATED: &str = "pub fn value() -> String:\n    \"caf\u{e9} from granted input\"\n";

fn project(tag: &str, requested_path: &str) -> TempDir {
    let dir = TempDir::new(tag);
    dir.write("app/witchy.toml", r#"[rune]
name = "app"
version = "0.1.0"

[capabilities]
runtime = ["Console"]

[dependencies]
genlib = { path = "../genlib" }

[build.grants."genlib"]
read = ["inputs"]
"#);
    dir.write("genlib/witchy.toml", r#"[rune]
name = "genlib"
version = "0.1.0"

[dependencies]
"#);
    dir.write("genlib/src/genlib.witchy", "pub fn placeholder() -> String:\n    \"shipped\"\n");
    dir.write("genlib/src/build.witchy", format!(
        "fn build(input: BuildRead, out: BuildOut):\n    let read = fn(): input.read_build(\"{requested_path}\")\n    let source = read()\n    out.write_out(\"genmod.witchy\", source)\n"
    ));
    // genmod exists nowhere in shipped source. Successful project compilation
    // must resolve the build child's output and check this actual call.
    dir.write("app/src/app.witchy", "import genmod\nfn main(console: Console):\n    console.print(genmod.value())\n");
    dir.write("app/inputs/template.txt", GENERATED);
    // The grant is relative to the consuming app, not cwd or the dependency.
    dir.write("inputs/template.txt", "pub fn value() -> String:\n    \"wrong workspace root\"\n");
    dir.write("genlib/inputs/template.txt", "pub fn value() -> String:\n    \"wrong dependency root\"\n");
    dir
}

fn build(dir: &TempDir) -> Output {
    Command::new(BIN)
        .current_dir(dir)
        .env("WITCHY_HOME", dir.join("home"))
        .args(["build", "app"])
        .output()
        .expect("spawn ordinary witchy build")
}

fn diagnostic(output: &Output) -> String {
    format!("status={:?}\nstdout={}\nstderr={}", output.status.code(),
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn ordinary_project_build_forwards_build_read_and_compiles_generated_source() {
    let dir = project("project-build-read", "template.txt");
    let generated = dir.join("app/build-out/genlib/genmod.witchy");
    assert!(!generated.exists(), "the build must not reuse preexisting generated source");

    // This invokes the project frontend, not build-step directly: resolver and
    // grant forwarding, admitted recipe child, generated-source audit, then
    // consumer compilation must all succeed. It is not a source/oracle parity run.
    let output = build(&dir);
    assert_eq!(output.status.code(), Some(0), "{}", diagnostic(&output));
    assert_eq!(std::fs::read_to_string(&generated).expect("read generated module"), GENERATED);
    assert!(dir.join("app/witchy.lock").is_file(), "ordinary project resolution must create its lock");
    assert!(dir.join("app/build-out/genlib/.witchy-build-cache").is_file(),
        "a successful recipe must record its input-dependent cache key");
}

#[test]
fn ordinary_project_build_read_cannot_escape_manifest_granted_root() {
    let dir = project("project-build-read-escape", "../outside.txt");
    let secret = "pub fn value() -> String:\n    \"outside-build-read-authority\"\n";
    dir.write("app/outside.txt", secret);

    let output = build(&dir);
    assert_eq!(output.status.code(), Some(1), "{}", diagnostic(&output));
    let trace = diagnostic(&output);
    assert!(trace.contains("read_build") && trace.contains("escapes"),
        "must reach the admitted read's confinement refusal, not fail in unrelated setup: {trace}");
    assert!(!trace.contains("outside-build-read-authority"), "the denied file body must not escape: {trace}");
    assert_eq!(std::fs::read_to_string(dir.join("app/outside.txt")).unwrap(), secret);
    assert!(!dir.join("app/build-out/genlib/genmod.witchy").exists(),
        "a failed read must not publish generated source");
    assert!(!dir.join("app/build-out/genlib/.witchy-build-cache").exists(),
        "a failed recipe must not publish a successful cache entry");
}
