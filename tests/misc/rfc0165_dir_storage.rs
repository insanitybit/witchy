//! Source-backed Dir storage: admitted native execution and independent virtual
//! filesystem execution. Oracle reads never fall back to the native test root.
use crate::temp_dir::TempDir;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{
    interpret_entry_program, interpret_program, InterpretError, InterpreterConfig,
    InterpreterValue,
};
use witchy_wir::canonical_safewir::FunctionId;

fn fixture() -> InterpreterConfig {
    let mut config = InterpreterConfig::default();
    config.fixture.dir_grants = Some(1);
    config.fixture.root = None;
    config.fixture.process_environment = false;
    config.fixture.files.insert("sentinel.txt".into(), b"untouched".to_vec());
    config
}

#[test]
fn dir_storage_checked_create_replace_binary_snapshot_agree() {
    let source = r#"import bytes
fn main(console: Console, root: Dir):
    console.print("${root.create_new("state.txt", "first")}")
    console.print("${root.create_new("state.txt", "must not replace")}")
    let original = root.read_bytes("state.txt")
    console.print(bytes.to_string(original))
    let replaced = root.replace("state.txt", "complete replacement")
    replaced
    console.print(bytes.to_string(original))
    console.print(bytes.to_string(root.read_bytes("state.txt")))
    let binary = bytes.concat(bytes.repeat(255, 1), bytes.repeat(0, 1))
    let written = root.write_bytes("binary.bin", binary)
    written
    let snapshot = root.read_bytes("binary.bin")
    root.write_bytes("binary.bin", bytes.from_string("later"))
    console.print("${bytes.length(snapshot)}")
    console.print("${bytes.at(snapshot, 0)}")
    console.print("${bytes.at(snapshot, 1)}")
    root.write_bytes("copy.bin", snapshot)
    console.print(bytes.to_string(root.read_bytes("binary.bin")))
    root.append_bytes("staged.bin", bytes.repeat(255, 1))
    root.append_bytes("staged.bin", bytes.repeat(0, 1))
    root.set_executable("staged.bin")
    console.print(bytes.to_string(root.read_bytes("binary.bin")))
    root.rename("staged.bin", "binary.bin")
    console.print("${root.exists("staged.bin")}")
    let installed = root.read_bytes("binary.bin")
    console.print("${bytes.length(installed)}:${bytes.at(installed, 0)}:${bytes.at(installed, 1)}")
"#;
    let expected = [
        "true", "false", "first", "first", "complete replacement",
        "2", "255", "0", "later", "later", "false", "2:255:0",
    ];
    let checked = witchy::resolve_std_only_checked(source).expect("checked Dir storage source");
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("source-bound entry");
            let function = FunctionId::new(
                entry.export().strip_prefix('f').unwrap().parse().unwrap(),
            );
            for with_argv in [false, true] {
                let arguments = entry.capabilities().iter().copied()
                    .map(InterpreterValue::entry_capability).collect();
                let result = if with_argv {
                    interpret_entry_program(validated, function, arguments, &[], fixture())
                } else {
                    interpret_program(validated, function, arguments, fixture())
                }.expect("explicit virtual Dir workflow executes");
                assert_eq!(result.output, expected);
                assert_eq!(result.leaked_objects, 0);
            }
        },
    ).expect_lowered("complete Dir storage source independently admits");

    let root = TempDir::new("canonical-dir-storage");
    root.write("sentinel.txt", b"untouched");
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true,
        dir_root: Some(root.path().to_owned()), dir_read: true, dir_write: true,
        ..Capabilities::none()
    }, 256).expect("instantiate admitted Dir storage artifact");
    vm.run().expect("native Dir storage workflow executes");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    assert_eq!(std::fs::read(root.join("state.txt")).unwrap(), b"complete replacement");
    assert_eq!(std::fs::read(root.join("binary.bin")).unwrap(), [255, 0]);
    assert!(!root.join("staged.bin").exists());
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(root.join("binary.bin")).unwrap().permissions().mode() & 0o111, 0o111);
    }
    assert_eq!(std::fs::read(root.join("copy.bin")).unwrap(), [255, 0]);
    assert_eq!(std::fs::read(root.join("sentinel.txt")).unwrap(), b"untouched");
}

#[test]
fn dir_storage_checked_parent_escape_refuses_without_mutation() {
    let source = r#"fn main(console: Console, root: Dir):
    root.replace("../outside.txt", "corrupted")
    console.print("unexpected success")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("checked escape attempt");
    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("source-bound entry");
            let function = FunctionId::new(
                entry.export().strip_prefix('f').unwrap().parse().unwrap(),
            );
            for with_argv in [false, true] {
                let arguments = entry.capabilities().iter().copied()
                    .map(InterpreterValue::entry_capability).collect();
                let result = if with_argv {
                    interpret_entry_program(validated, function, arguments, &[], fixture())
                } else {
                    interpret_program(validated, function, arguments, fixture())
                };
                // Existing canonical Dir path checks reject traversal before
                // any file lookup or mutation, not as an unsupported import.
                assert_eq!(result.expect_err("parent traversal must refuse"), InterpretError::ExplicitTrap);
            }
        },
    ).expect_lowered("path refusal occurs after source-backed admission");

    let workspace = TempDir::new("canonical-dir-storage-escape");
    let root = workspace.mkdir("granted");
    workspace.write("outside.txt", b"outside remains intact");
    workspace.write("granted/sentinel.txt", b"untouched");
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true,
        dir_root: Some(root.clone()), dir_read: true, dir_write: true,
        ..Capabilities::none()
    }, 256).expect("instantiate admitted confined Dir artifact");
    vm.run().expect_err("native parent traversal must refuse");
    assert!(vm.output().is_empty());
    assert_eq!(std::fs::read(workspace.join("outside.txt")).unwrap(), b"outside remains intact");
    assert_eq!(std::fs::read(root.join("sentinel.txt")).unwrap(), b"untouched");
    // Failed execution is not evidence of successful zero-leak cleanup.
}
