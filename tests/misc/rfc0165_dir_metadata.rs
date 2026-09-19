//! Directory metadata, including captured authority and aliases.
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};

#[test]
fn canonical_dir_metadata_oracle_wasm_and_missing_grants() {
    let root = crate::temp_dir::TempDir::new("canonical-dir-metadata");
    std::fs::create_dir_all(root.path().join("nested/leaf")).unwrap();
    std::fs::create_dir(root.path().join("\u{e9}-\u{1f980}")).unwrap();
    std::fs::write(root.path().join("file"), b"unchanged").unwrap();
    let outside = crate::temp_dir::TempDir::new("canonical-dir-metadata-outside");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("nested", root.path().join("inside-link")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("outside-link")).unwrap();
        std::os::unix::fs::symlink("missing", root.path().join("dangling-link")).unwrap();
    }
    let mut expected = vec!["true", "true", "true", "true", "true", "true", "true",
        "false", "false", "false", "false", "false", "false", "false"];
    let extra = if cfg!(unix) {
        expected.extend(["true", "false", "false"]);
        r#", "inside-link", "outside-link", "dangling-link""#
    } else { "" };
    let source = format!(r#"fn main(console: Console, root: Dir[Read]):
    let probe = fn(path: String) -> Bool: root.is_dir(path)
    for path in ["", ".", "nested", "nested/leaf", "nested//leaf/", "./nested", "UNICODE_DIR", "file", "missing", "file/child", "../", "nested/../", "/", "bad\0name"{extra}]:
        console.print("${{probe(path)}}")
"#).replace("UNICODE_DIR", "\u{e9}-\u{1f980}");
    let checked = witchy::resolve_std_only_checked(&source).unwrap();
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let tokens = || entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let mut config = InterpreterConfig::default();
        config.fixture.root = Some(root.path().to_owned());
        config.fixture.dir_grants = Some(1);
        let run = interpret_entry_program(validated, function, tokens(), &[], config.clone()).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
        config.fixture.dir_grants = Some(0);
        assert!(interpret_entry_program(validated, function, tokens(), &[], config).is_err());
    }).expect_lowered("Dir metadata uses an authenticated read-only slice import");
    let mut runtime = Runtime::new().unwrap();
    let caps = Capabilities { print: true, quiet: true, dir_root: Some(root.path().to_owned()), dir_read: true, ..Capabilities::none() };
    assert!(runtime.spawn_admitted(&artifact, Capabilities { dir_read: false, ..caps.clone() }, 256).is_err());
    let mut vm = runtime.spawn_admitted(&artifact, caps, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    assert_eq!(std::fs::read(root.path().join("file")).unwrap(), b"unchanged");
}

#[test]
fn canonical_dir_metadata_source_requires_read_right() {
    let source = "fn main(root: Dir[Write]):\n    let value = root.is_dir(\"nested\")\n    ()\n";
    let error = witchy::resolve_std_only_checked(source).err().expect("Write does not imply Read").to_string();
    assert!(error.contains("Read"), "{error}");
}
