use std::collections::HashMap;
use std::path::PathBuf;

#[path = "../../tests/support/temp_dir.rs"]
mod temp_dir;

#[test]
fn project_resolver_retains_nonconventional_selected_sources() {
    let root = temp_dir::TempDir::new("project-resolver-selected-sources");
    for name in ["app", "helper"] {
        std::fs::create_dir_all(root.path().join(name).join("src")).unwrap();
    }
    std::fs::write(root.path().join("app/witchy.toml"),
        "[rune]\nname = \"app\"\nversion = \"1\"\n[dependencies]\nhelper = { path = \"../helper\" }\n").unwrap();
    std::fs::write(root.path().join("helper/witchy.toml"),
        "[rune]\nname = \"helper\"\nversion = \"1\"\n[dependencies]\n").unwrap();
    std::fs::write(root.path().join("app/src/entry.witchy"), "fn main(): ()\n").unwrap();
    std::fs::write(root.path().join("helper/src/alternate.witchy"), "pub fn value() -> Int: 1\n").unwrap();
    let probe = root.path().join("probe.witchy");
    std::fs::write(&probe, r#"from gm_engine import dep_flags_for_entry
fn main(console: Console, root: Dir):
    match dep_flags_for_entry(root, "app", "app/src/entry.witchy"):
        Ok(flags) ->
            for flag in flags:
                console.print(flag)
        Err(error) -> fail(error.show())
"#).unwrap();
    let repository = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
    );
    let mut dependencies = HashMap::from([("gm_engine".into(),
        repository.join("projects/grimoire/src/gm_engine.witchy"))]);
    for name in ["coven_proto", "coven_json", "coven_validate", "coven_artifact"] {
        dependencies.insert(name.into(), repository.join(format!("projects/coven/src/{name}.witchy")));
    }
    let (checked, _) = crate::source::link_file_checked_with_deps(probe.to_str().unwrap(), &dependencies).unwrap();
    let output = witchy_interp::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap();
    assert_eq!(output, ["--package-owner", "workspace", "app", "1", "src.entry",
        "--dep", "helper=helper/src/alternate.witchy", "--dep-owner", "helper", "workspace", "helper", "1", "src.alternate"]);
}

#[cfg(unix)]
#[test]
fn project_exec_source_oracle_preserves_process_contract() {
    use std::os::unix::fs::PermissionsExt;
    let root = temp_dir::TempDir::new("project-exec-source-contract");
    let executable = root.path().join("probe");
    std::fs::write(&executable, b"#!/bin/sh\nprintf x >> \"$1\"\nread line\nprintf 'args=%s,%s,%s;stdin=%s\\n' \"$2\" \"$3\" \"$4\" \"$line\"\nprintf 'stderr\\n' >&2\nexit 7\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let marker = root.path().join("effects");
    let probe = root.path().join("main.witchy");
    std::fs::write(&probe, format!(r#"import exec
fn main(console: Console, runner: Exec, root: Dir):
    let result = exec.run(runner, root, "probe", [{:?}, "two words", "", "tail"], "input\n")
    console.print("${{result.0}}")
    console.print("${{result.1}}<end>")
"#, marker.to_str().unwrap())).unwrap();
    let (checked, _) = crate::source::link_file_checked_with_deps(probe.to_str().unwrap(), &HashMap::new()).unwrap();
    let output = witchy_interp::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap();
    assert_eq!(output, ["7", "args=two words,,tail;stdin=input\nstderr\n<end>"]);
    assert_eq!(std::fs::read(marker).unwrap(), b"x");
}
