use super::link_build_file;
use std::path::PathBuf;
use witchy_wir::canonical_interpreter::{self as oracle, InterpreterConfig, InterpreterValue, OracleFixture};

#[path = "build_env_recipe_tests.rs"]
mod build_env_recipe_tests;

#[path = "build_exec_recipe_tests.rs"]
mod build_exec_recipe_tests;

struct Folder(PathBuf);
impl Folder {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("witchy-build-recipe-{}-{}",
            std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn source(&self, source: &str) -> PathBuf {
        let path = self.0.join("recipe.witchy");
        std::fs::write(&path, source).unwrap();
        path
    }
}
impl Drop for Folder { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

fn checked(folder: &Folder, source: &str) -> witchy_types::pipeline::CheckedBuildRecipe {
    link_build_file(folder.source(source).to_str().unwrap()).expect("source-backed build recipe")
}
fn native(recipe: &witchy_types::pipeline::CheckedBuildRecipe, directory: Option<PathBuf>) -> Result<(), String> {
    witchy_interp::compiler_natives::install();
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(recipe).expect_lowered("admitted build recipe");
    assert_eq!(artifact.entry().unwrap().capabilities(), &[witchy_wir::CanonicalEntryCapability::BuildOut]);
    if let Some(directory) = &directory { std::fs::create_dir_all(directory).unwrap(); }
    let caps = crate::runtime::Capabilities { build_out: directory, ..crate::runtime::Capabilities::none() };
    let mut runtime = crate::runtime::Runtime::batch().map_err(|error| error.to_string())?;
    let mut vm = runtime.spawn_admitted(&artifact, caps, crate::RUN_MEMORY_PAGES).map_err(|error| error.to_string())?;
    vm.run().map_err(|error| format!("{error:#}"))?;
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    Ok(())
}

#[test]
fn build_recipe_three_legs_capture_overwrite_and_explicit_entry() {
    let folder = Folder::new();
    let recipe = checked(&folder, r#"fn main(console: Console):
    console.print("must not run")
fn build(out: BuildOut):
    out.write_out("nested/result.txt", "first")
    let write = fn(): out.write_out("nested/result.txt", "final\n")
    write()
"#);
    assert_eq!(recipe.identity().recipe_version(), 2);
    assert_eq!(recipe.identity().parameter_kinds(), &[9]);
    assert_eq!(recipe.identity().selected_declaration(), "recipe.build");
    assert_ne!(recipe.identity().original_source_digest(), recipe.identity().derived_source_digest());
    let source = recipe.canonical_source();
    assert_eq!(source.entry_declaration(), "recipe.witchy_build_recipe_entry");
    assert_eq!(&source.launch_contract_payload()[..2], &[2, 2]);
    assert!(source.launch_contract_payload().ends_with(&[1, 0, 0, 0, 9, 1]));
    assert!(source.closed_callable_abi(source.entry_declaration()).is_some());
    assert!(source.closed_callable_abi("main").is_none());
    let interpreted = folder.0.join("interpreted");
    crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
        out_dir: interpreted.clone(), ..Default::default()
    }).expect("source build executes");
    let compiled = folder.0.join("compiled");
    native(&recipe, Some(compiled.clone())).expect("admitted build executes");
    for directory in [&interpreted, &compiled] {
        assert_eq!(std::fs::read_to_string(directory.join("nested/result.txt")).unwrap(), "final\n");
    }
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("artifact");
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    for argv_launcher in [false, true] {
        let config = InterpreterConfig { fixture: OracleFixture { build_out: true, ..Default::default() }, ..Default::default() };
        let parameters = vec![InterpreterValue::entry_capability(witchy_wir::CanonicalEntryCapability::BuildOut)];
        let run = if argv_launcher {
            oracle::interpret_entry_program(&validated, function, parameters, &[], config)
        } else { oracle::interpret_program(&validated, function, parameters, config) }.expect("independent build oracle");
        assert_eq!(run.leaked_objects, 0);
        assert!(run.output.is_empty());
        assert_eq!(run.build_trace, vec![
            oracle::OracleBuildWrite { path: "nested/result.txt".into(), contents: "first".into() },
            oracle::OracleBuildWrite { path: "nested/result.txt".into(), contents: "final\n".into() },
        ]);
    }
}

#[test]
fn build_recipe_rejects_wrong_source_binding_and_missing_grant() {
    let folder = Folder::new();
    let first = checked(&folder, "fn build(out: BuildOut):\n    out.write_out(\"x\", \"one\")\n");
    let second = checked(&folder, "fn build(out: BuildOut):\n    out.write_out(\"x\", \"two\")\n");
    assert_ne!(first.identity().original_source_digest(), second.identity().original_source_digest());
    assert_ne!(first.identity().derived_source_digest(), second.identity().derived_source_digest());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&first).unwrap();
    assert!(witchy_wir::admit_canonical(validated, second.canonical_source()).is_err());
    let error = native(&first, None).unwrap_err();
    assert!(error.contains("no BuildOut grant"), "{error}");
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&first).expect_lowered("artifact");
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&first).unwrap();
    for kind in [9, 0] {
        assert!(oracle::interpret_program(&validated, function, vec![InterpreterValue::capability(kind)], InterpreterConfig::default()).is_err());
    }
}

#[test]
fn build_recipe_rejects_unsupported_and_imported_targets() {
    let folder = Folder::new();
    for source in [
        "fn ordinary() -> Nil:\n    Nil\n",
        "fn build(out: BuildOut, net: BuildNet) -> Nil:\n    Nil\n",
        "fn build(var out: BuildOut) -> Nil:\n    Nil\n",
        "fn build(own out: BuildOut) -> Nil:\n    Nil\n",
        "fn build(value: String) -> Nil:\n    Nil\n",
        "fn build(out: BuildOut) -> Int:\n    7\n",
        "async fn build(out: BuildOut) -> Nil:\n    Nil\n",
    ] {
        assert!(link_build_file(folder.source(source).to_str().unwrap()).is_err(), "must refuse {source}");
    }
    std::fs::write(folder.0.join("helper.witchy"), "pub fn build(out: BuildOut) -> Nil:\n    Nil\n").unwrap();
    let error = link_build_file(folder.source("import helper\n").to_str().unwrap()).unwrap_err();
    assert!(error.contains("own entry source"), "{error}");
    // A source declaration with the generated spelling never becomes the
    // selected entry. It is refused, not overwritten, renamed, or invoked.
    for declaration in ["fn", "pub fn"] {
        let source = format!("{declaration} witchy_build_recipe_entry(out: BuildOut) -> Nil:\n    out.write_out(\"forged.txt\", \"never\")\nfn build(out: BuildOut) -> Nil:\n    Nil\n");
        let error = link_build_file(folder.source(&source).to_str().unwrap()).unwrap_err();
        assert!(error.contains("collides with the compiler recipe entry"), "{error}");
    }
    let error = link_build_file(folder.source(
        "fn witchy_build_recipe_entry(out: BuildOut) -> Nil:\n    Nil\n",
    ).to_str().unwrap()).unwrap_err();
    assert!(error.contains("collides with the compiler recipe entry"), "{error}");
}

#[test]
fn build_recipe_traversal_refuses_on_three_legs() {
    let folder = Folder::new();
    let recipe = checked(&folder, "fn build(out: BuildOut):\n    out.write_out(\"../escape.txt\", \"never\")\n");
    let interpreted = folder.0.join("interpreted");
    assert!(crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
        out_dir: interpreted, ..Default::default()
    }).is_err());
    assert!(native(&recipe, Some(folder.0.join("compiled"))).is_err());
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("artifact");
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    let error = oracle::interpret_program(&validated, function, vec![InterpreterValue::capability(9)], InterpreterConfig {
        fixture: OracleFixture { build_out: true, ..Default::default() }, ..Default::default()
    }).unwrap_err();
    assert!(error.to_string().contains("escapes"), "{error}");
    assert!(!folder.0.join("escape.txt").exists());
}

#[cfg(unix)]
#[test]
fn build_recipe_symlink_escape_preserves_handle_confinement() {
    let folder = Folder::new();
    let outside = folder.0.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let recipe = checked(&folder, "fn build(out: BuildOut):\n    out.write_out(\"redirect/escape.txt\", \"never\")\n");
    for backend in ["interpreted", "compiled"] {
        let directory = folder.0.join(backend);
        std::fs::create_dir_all(&directory).unwrap();
        std::os::unix::fs::symlink(&outside, directory.join("redirect")).unwrap();
        if backend == "compiled" { assert!(native(&recipe, Some(directory)).is_err()); }
        else { assert!(crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
            out_dir: directory, ..Default::default()
        }).is_err()); }
    }
    assert!(!outside.join("escape.txt").exists());
}

#[test]
fn build_recipe_trailing_separators_preserve_file_and_directory_meaning() {
    // ConfinedDir resolves the final basename, rather than passing a trailing
    // slash to open(2). These spellings name files in the existing BuildOut
    // contract. A name that actually resolves to a directory must still fail.
    for directory_target in [false, true] {
        let folder = Folder::new();
        let source = if directory_target {
            "fn build(out: BuildOut):\n    out.write_out(\"nested/file\", \"retained\")\n    out.write_out(\"nested/.\", \"must not replace directory\")\n"
        } else {
            "fn build(out: BuildOut):\n    out.write_out(\"slash/\", \"slash\")\n    out.write_out(\"dot/.\", \"dot\")\n    out.write_out(\"nested//value/\", \"nested\")\n"
        };
        let recipe = checked(&folder, source);
        let interpreted = folder.0.join("interpreted");
        let source_result = crate::interpreter::run_build_step(
            recipe.original_module().module().clone(),
            crate::interpreter::BuildGrants {
                out_dir: interpreted.clone(), ..Default::default()
            },
        );
        let compiled = folder.0.join("compiled");
        let native_result = native(&recipe, Some(compiled.clone()));
        assert_eq!(source_result.is_err(), directory_target, "source: {source_result:?}");
        assert_eq!(native_result.is_err(), directory_target, "native: {native_result:?}");
        for directory in [&interpreted, &compiled] {
            if directory_target {
                assert!(directory.join("nested").is_dir());
                assert_eq!(std::fs::read_to_string(directory.join("nested/file")).unwrap(), "retained");
            } else {
                for (path, contents) in [("slash", "slash"), ("dot", "dot"), ("nested/value", "nested")] {
                    assert_eq!(std::fs::read_to_string(directory.join(path)).unwrap(), contents);
                }
            }
        }
        let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("artifact");
        let function = witchy_wir::canonical_safewir::FunctionId::new(
            artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap(),
        );
        let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
        for argv_launcher in [false, true] {
            let config = InterpreterConfig {
                fixture: OracleFixture { build_out: true, ..Default::default() }, ..Default::default()
            };
            let parameters = vec![InterpreterValue::entry_capability(witchy_wir::CanonicalEntryCapability::BuildOut)];
            let result = if argv_launcher {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else {
                oracle::interpret_program(&validated, function, parameters, config)
            };
            if directory_target {
                let error = result.unwrap_err();
                assert!(error.to_string().contains("target is a directory"), "{error}");
                // An aborted run does not supply a success trace or leak result.
            } else {
                let run = result.expect("independent oracle accepts file spellings");
                assert_eq!(run.leaked_objects, 0);
                assert!(run.output.is_empty());
                assert_eq!(run.build_trace, vec![
                    oracle::OracleBuildWrite { path: "slash/".into(), contents: "slash".into() },
                    oracle::OracleBuildWrite { path: "dot/.".into(), contents: "dot".into() },
                    oracle::OracleBuildWrite { path: "nested//value/".into(), contents: "nested".into() },
                ]);
            }
        }
    }
}

#[test]
fn build_read_recipe_three_legs_capture_order_utf8_fallback_and_zero_leaks() {
    use std::collections::BTreeMap;
    use witchy_wir::CanonicalEntryCapability::{BuildOut, BuildRead};
    witchy_interp::compiler_natives::install();
    for reversed in [false, true] {
        let folder = Folder::new();
        let parameters = if reversed { "input: BuildRead, out: BuildOut" } else { "out: BuildOut, input: BuildRead" };
        let recipe = checked(&folder, &format!("fn build({parameters}):\n    let read = fn(): input.read_build(\"input.txt\")\n    let value = read()\n    out.write_out(\"value.txt\", value)\n    out.write_out(\"first.txt\", input.read_build(\"first.txt\"))\n"));
        let kinds = if reversed { vec![12, 9] } else { vec![9, 12] };
        assert_eq!(recipe.identity().parameter_kinds(), kinds);
        let mut expected_abi = vec![2, 0, 0, 0];
        for kind in &kinds { expected_abi.extend_from_slice(&[*kind, 1]); }
        assert!(recipe.canonical_source().launch_contract_payload().ends_with(&expected_abi));
        let contents = "caf\u{e9} ".repeat(70);
        let roots = vec![
            BTreeMap::from([("input.txt".to_owned(), vec![0xff]), ("first.txt".to_owned(), b"first wins".to_vec())]),
            BTreeMap::from([("input.txt".to_owned(), contents.as_bytes().to_vec()), ("first.txt".to_owned(), b"second loses".to_vec())]),
        ];
        let read_roots = roots.iter().enumerate().map(|(index, files)| {
            let root = folder.0.join(format!("root{index}"));
            std::fs::create_dir_all(&root).unwrap();
            for (path, bytes) in files { std::fs::write(root.join(path), bytes).unwrap(); }
            root
        }).collect::<Vec<_>>();
        let interpreted = folder.0.join("interpreted");
        crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
            out_dir: interpreted.clone(), read_roots: read_roots.clone(), ..Default::default()
        }).expect("source BuildRead preserves ordered strict-UTF8 fallback");
        let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("BuildRead admission");
        let expected_caps = if reversed { vec![BuildRead, BuildOut] } else { vec![BuildOut, BuildRead] };
        assert_eq!(artifact.entry().unwrap().capabilities(), expected_caps);
        let compiled = folder.0.join("compiled");
        std::fs::create_dir_all(&compiled).unwrap();
        let mut runtime = crate::runtime::Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
            build_out: Some(compiled.clone()), build_read_roots: read_roots, ..crate::runtime::Capabilities::none()
        }, crate::RUN_MEMORY_PAGES).unwrap();
        vm.run().expect("native BuildRead executes");
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
        assert!(vm.output().is_empty());
        for directory in [&interpreted, &compiled] {
            assert_eq!(std::fs::read_to_string(directory.join("value.txt")).unwrap(), contents);
            assert_eq!(std::fs::read_to_string(directory.join("first.txt")).unwrap(), "first wins");
        }
        let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
        for argv_launcher in [false, true] {
            let parameters = expected_caps.iter().copied().map(InterpreterValue::entry_capability).collect();
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true,
                build_read: Some(oracle::OracleBuildReadFixture { roots: roots.clone() }),
                ..Default::default()
            }, ..Default::default() };
            let run = if argv_launcher {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) }.unwrap();
            assert_eq!(run.leaked_objects, 0);
            assert!(run.output.is_empty());
            assert_eq!(run.build_trace, vec![
                oracle::OracleBuildWrite { path: "value.txt".into(), contents: contents.clone() },
                oracle::OracleBuildWrite { path: "first.txt".into(), contents: "first wins".into() },
            ]);
        }
    }
}

#[test]
fn build_read_recipe_missing_grant_and_escape_refuse_across_three_legs() {
    use std::collections::BTreeMap;
    witchy_interp::compiler_natives::install();
    for escape in [false, true] {
        let folder = Folder::new();
        let path = if escape { "../outside.txt" } else { "inside.txt" };
        let recipe = checked(&folder, &format!("fn build(out: BuildOut, input: BuildRead):\n    out.write_out(\"result.txt\", input.read_build(\"{path}\"))\n"));
        let root = folder.0.join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(folder.0.join("outside.txt"), "secret").unwrap();
        std::fs::write(root.join("inside.txt"), "inside").unwrap();
        let read_roots = if escape { vec![root] } else { Vec::new() };
        let interpreted = folder.0.join("interpreted");
        assert!(crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
            out_dir: interpreted.clone(), read_roots: read_roots.clone(), ..Default::default()
        }).is_err());
        let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("checked read recipe");
        let compiled = folder.0.join("compiled");
        std::fs::create_dir_all(&compiled).unwrap();
        let mut runtime = crate::runtime::Runtime::batch().unwrap();
        let result = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
            build_out: Some(compiled.clone()), build_read_roots: read_roots, ..crate::runtime::Capabilities::none()
        }, crate::RUN_MEMORY_PAGES).and_then(|mut vm| vm.run());
        assert!(result.is_err(), "missing grant or confined escape must not execute a read");
        assert!(!compiled.join("result.txt").exists());
        assert!(!interpreted.join("result.txt").exists());
        let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
        for argv_launcher in [false, true] {
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true,
                build_read: escape.then(|| oracle::OracleBuildReadFixture {
                    roots: vec![BTreeMap::from([("../outside.txt".into(), b"secret".to_vec())])],
                }),
                ..Default::default()
            }, ..Default::default() };
            let parameters = vec![InterpreterValue::capability(9), InterpreterValue::capability(12)];
            let result = if argv_launcher {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) };
            assert!(result.is_err(), "fixture data must not authorize escape or mint missing grants");
        }
    }
}

#[test]
fn build_read_recipe_binds_order_and_remains_unavailable_to_ordinary_main() {
    let folder = Folder::new();
    let first = checked(&folder, "fn build(input: BuildRead, out: BuildOut):\n    out.write_out(\"x\", input.read_build(\"input\"))\n");
    let second = checked(&folder, "fn build(out: BuildOut, input: BuildRead):\n    out.write_out(\"x\", input.read_build(\"input\"))\n");
    assert_eq!(first.identity().parameter_kinds(), &[12, 9]);
    assert_eq!(second.identity().parameter_kinds(), &[9, 12]);
    assert_ne!(first.identity().derived_source_digest(), second.identity().derived_source_digest());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&first).unwrap();
    assert!(witchy_wir::admit_canonical(validated, second.canonical_source()).is_err());
    let path = folder.source("fn main(input: BuildRead) -> Nil:\n    Nil\n");
    assert!(crate::link_file_checked(path.to_str().unwrap()).is_err(), "ordinary main cannot request BuildRead");
}
