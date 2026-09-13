//! Source-backed Exec admission and independent process transcript coverage.
use crate::temp_dir::TempDir;

#[test]
fn checked_entry_scope_preserves_calls_and_captures_across_three_legs() {
    use witchy::runtime::{Capabilities, Runtime};
    use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
    use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};
    let library = b"fn add(a: Int, b: Int) -> Int:\n    a + b\npub fn make(offset: Int) -> fn(Int) -> Int:\n    fn(value: Int) -> Int: add(offset, value)\npub fn apply(operation: fn(Int) -> Int, value: Int) -> Int:\n    operation(value)\npub fn unused(fetch: Fetch) -> Int:\n    1\n";
    let check = |entry: &[u8]| {
        // This fixture is the source loader: bind both exact raw modules to
        // workspace owners, then freshly check through the production expander.
        let package = PackageCoordinate::new(PackageSource::Workspace, "test/entry-scope", "0.1.0").unwrap();
        let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", env!("CARGO_PKG_VERSION")).unwrap();
        let mut assignments = ["entry", "library"].into_iter().map(|name| {
            (name.to_owned(), ModuleLoadIdentity::new(package.clone(), ["src", name]).unwrap())
        }).collect::<Vec<_>>();
        assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
            ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap())
        }));
        witchy_interp::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
            vec![("entry".into(), entry.to_vec()), ("library".into(), library.to_vec())],
            "entry", &["entry".to_owned(), "library".to_owned()].into_iter().collect(),
            AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap(),
        ).expect("all raw source checks, including unsupported declarations")
    };
    let entry = b"import library\nfn main(console: Console):\n    let operation = library.make(4)\n    console.print(\"${library.apply(operation, 3)}\")\n";
    let checked = check(entry);
    let root = TempDir::new("checked-entry-scope");
    let expected = ["7"];
    assert_eq!(witchy::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap(), expected);
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let run = interpret_entry_program(validated, function, tokens, &[], InterpreterConfig::default()).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
    }).expect_lowered("only the selected source entry and referenced library functions admit");
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, ..Capabilities::none() }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));

    for entry in [
        "import library\nfn main(fetch: Fetch) -> Int:\n    library.unused(fetch)\n",
        "import library\nfn main():\n    let callback = library.unused\n    Nil\n",
        "fn exported(fetch: Fetch) -> Int:\n    1\nfn main():\n    Nil\n",
    ] {
        let checked = check(entry.as_bytes());
        match witchy::codegen::compile_checked_module_artifact(&checked) {
            witchy::codegen::LoweringOutcome::Unsupported(error) => {
                assert!(error.to_string().contains("Fetch"), "{error}");
            }
            _ => panic!("reachable or entry-exported Fetch must still refuse admission"),
        }
    }
}

#[test]
#[cfg(unix)]
fn admitted_exec_source_oracle_and_wasm_preserve_effect_once_and_capture() {
    use std::os::unix::fs::PermissionsExt;
    use witchy::runtime::{Capabilities, Runtime};
    use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, OracleExecCall, OracleExecFixture, interpret_entry_program};
    let root = TempDir::new("canonical-exec");
    let probe = root.path().join("probe");
    std::fs::write(&probe, "#!/bin/sh\nprintf x >> effects\nread line\nprintf 'args=%s,%s,%s;stdin=%s\\n' \"$1\" \"$2\" \"$3\" \"$line\"\nprintf 'stderr\\n' >&2\nexit 7\n").unwrap();
    std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o700)).unwrap();
    // Absolute marker avoids depending on the process runner's cwd contract.
    let marker = root.path().join("effects");
    std::fs::write(&probe, format!("#!/bin/sh\nprintf x >> '{}'\nread line\nprintf 'args=%s,%s,%s;stdin=%s\\n' \"$1\" \"$2\" \"$3\" \"$line\"\nprintf 'stderr\\n' >&2\nexit 7\n", marker.display())).unwrap();
    let source = r#"
import exec
fn main(console: Console, runner: Exec, root: Dir):
    let run = fn() -> (Int, String): exec.run(runner, root, "probe", ["two words", "", "tail"], "input\n")
    let result = run()
    console.print("${result.0}")
    console.print("${result.1}<end>")
"#;
    let expected = ["7", "args=two words,,tail;stdin=input\nstderr\n<end>"];
    let checked = witchy::resolve_std_only_checked(source).unwrap();
    assert_eq!(witchy::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap(), expected);
    assert_eq!(std::fs::read(&marker).unwrap(), b"x");
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let mut config = InterpreterConfig::default();
        config.fixture.exec = Some(OracleExecFixture { allow: Some(["probe".into()].into_iter().collect()), dir_read: true, calls: vec![OracleExecCall {
            directory: "".into(), path: "probe".into(), arguments: "two words\0\0tail".into(), stdin: "input\n".into(), status: 7,
            stdout: b"args=two words,,tail;stdin=input\n".to_vec(), stderr: b"stderr\n".to_vec(),
        }] });
        config.fixture.dir_grants = Some(1);
        let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let tokens = || entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let run = interpret_entry_program(validated, function, tokens(), &[], config.clone()).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
        let granted = config.clone();
        config.fixture.exec.as_mut().unwrap().dir_read = false;
        assert!(interpret_entry_program(validated, function, tokens(), &[], config.clone()).unwrap_err().to_string().contains("read right"));
        config = granted.clone();
        config.fixture.exec.as_mut().unwrap().allow = Some(Default::default());
        assert!(interpret_entry_program(validated, function, tokens(), &[], config.clone()).unwrap_err().to_string().contains("allow-list"));
        config = granted.clone();
        let extra = config.fixture.exec.as_ref().unwrap().calls[0].clone();
        config.fixture.exec.as_mut().unwrap().calls.push(extra);
        assert!(interpret_entry_program(validated, function, tokens(), &[], config.clone()).unwrap_err().to_string().contains("unused required"));
        config = granted;
        config.fixture.exec.as_mut().unwrap().calls[0].stdin = "wrong".into();
        assert!(interpret_entry_program(validated, function, tokens(), &[], config.clone()).unwrap_err().to_string().contains("transcript mismatch"));
        config.fixture.exec = None;
        assert!(interpret_entry_program(validated, function, tokens(), &[], config).unwrap_err().to_string().contains("not granted"));
    }).expect_lowered("Exec source authenticates and admits");
    let caps = Capabilities { print: true, exec: true, exec_allow: Some(vec!["probe".into()]), dir_root: Some(root.path().to_owned()), dir_read: true, ..Capabilities::none() };
    let mut runtime = Runtime::new().unwrap();
    let denied = Capabilities { exec: false, ..caps.clone() };
    assert!(runtime.spawn_admitted(&artifact, denied, 256).is_err(), "missing Exec grant must refuse before effects");
    assert_eq!(std::fs::read(&marker).unwrap(), b"x");
    let mut vm = runtime.spawn_admitted(&artifact, caps, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    assert_eq!(std::fs::read(marker).unwrap(), b"xx");
}
