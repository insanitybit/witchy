//! Source-backed Exec admission and independent process transcript coverage.
use crate::temp_dir::TempDir;

#[path = "rfc0165_crypto.rs"]
mod crypto;

fn check_scoped_modules(entry: &[u8], library: &[u8]) -> witchy::pipeline::CheckedModule {
    use witchy_types::runtime_type::{AuthenticatedModuleOwners, ModuleLoadIdentity, PackageCoordinate, PackageSource};
    // This fixture is the source loader: bind exact raw user modules to
    // workspace owners and load imported std modules from compiler bytes only.
    let package = PackageCoordinate::new(PackageSource::Workspace, "test/entry-scope", "0.1.0").unwrap();
    let toolchain = PackageCoordinate::new(PackageSource::Toolchain, "witchy/std", env!("CARGO_PKG_VERSION")).unwrap();
    let mut assignments = ["entry", "library"].into_iter().map(|name| {
        (name.to_owned(), ModuleLoadIdentity::new(package.clone(), ["src", name]).unwrap())
    }).collect::<Vec<_>>();
    assignments.extend(witchy_syntax::linker::STD_MODULES.iter().map(|name| {
        ((*name).to_owned(), ModuleLoadIdentity::new(toolchain.clone(), ["std", *name]).unwrap())
    }));
    let user_modules = ["entry".to_owned(), "library".to_owned()].into_iter().collect::<std::collections::HashSet<_>>();
    let mut loaded = user_modules.clone();
    let mut sources = vec![("entry".to_owned(), entry.to_vec()), ("library".to_owned(), library.to_vec())];
    let mut cursor = 0;
    while cursor < sources.len() {
        let parsed = witchy::parser::parse_module(std::str::from_utf8(&sources[cursor].1).unwrap()).unwrap();
        for name in parsed.imports {
            if loaded.insert(name.clone()) {
                let source = witchy_syntax::linker::bundled_source(&name).expect("fixture imports only bundled std or its two user modules");
                sources.push((name, source.as_bytes().to_vec()));
            }
        }
        cursor += 1;
    }
    witchy_interp::pipeline::link_checked_authenticated_from_source_bundle_with_user_modules(
        sources, "entry", &user_modules,
        AuthenticatedModuleOwners::from_loader_assignments(assignments).unwrap(),
    ).expect("all raw source checks, including unsupported declarations")
}

#[test]
fn checked_entry_scope_preserves_calls_and_captures_across_three_legs() {
    use witchy::runtime::{Capabilities, Runtime};
    use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};
    let library = b"fn add(a: Int, b: Int) -> Int:\n    a + b\npub fn make(offset: Int) -> fn(Int) -> Int:\n    fn(value: Int) -> Int: add(offset, value)\npub fn apply(operation: fn(Int) -> Int, value: Int) -> Int:\n    operation(value)\npub fn unused(fetch: Fetch) -> Int:\n    1\n";
    let check = |entry: &[u8]| check_scoped_modules(entry, library);
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
fn checked_entry_scope_keeps_imported_metadata_only_dynamic_methods() {
    use witchy::runtime::{Capabilities, Runtime};
    use witchy_wir::canonical_interpreter::{InterpreterConfig, InterpreterValue, interpret_entry_program};
    let entry = b"import library\nfn main(console: Console):\n    library.inspect(console)\n";
    let library = br#"import dynamic
import reflect
type Counter derive(Reflect):
    value: Int
fn adjusted(value: Int, amount: Int) -> Int:
    value + amount
@dynamic
pub fn add(self: Counter, amount: Int) -> Counter:
    Counter(adjusted(self.value, amount))
pub fn unused(fetch: Fetch) -> Int:
    1
pub fn inspect(console: Console):
    let value = dynamic.dynamic(Counter(4))
    console.print(dynamic.type_name(dynamic.type_of(value)))
"#;
    let checked = check_scoped_modules(entry, library);
    let canonical = checked.canonical_source().unwrap();
    assert!(canonical.dynamic_runtime_plan().unwrap().methods().iter().any(|method| method.function == "library.add"));
    let module = canonical.typed_module().module();
    for name in ["library.add", "library.adjusted"] {
        assert!(module.items.iter().any(|item| matches!(item, witchy::ast::Item::Function(function) if function.name == name)), "retain the rechecked metadata dependency {name}");
    }
    assert!(!witchy_types::pipeline::canonical_scope(module).contains("library.unused"));
    let expected = ["library.Counter"];
    let root = TempDir::new("checked-entry-metadata");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, root.path(), Vec::new()).unwrap(), expected);
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(&checked, |validated, artifact| {
        let entry = artifact.entry().unwrap();
        let function = witchy_wir::canonical_safewir::FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
        let tokens = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
        let run = interpret_entry_program(validated, function, tokens, &[], InterpreterConfig::default()).unwrap();
        assert_eq!(run.output, expected);
        assert_eq!(run.leaked_objects, 0);
    }).expect_lowered("imported Dynamic metadata keeps its authenticated method declarations without calling them");
    let mut runtime = Runtime::new().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, ..Capabilities::none() }, 256).unwrap();
    vm.run().unwrap();
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
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
