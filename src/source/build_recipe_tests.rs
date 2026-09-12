use super::link_build_file;
use std::path::PathBuf;
use witchy_wir::canonical_interpreter::{self as oracle, InterpreterConfig, InterpreterValue, OracleFixture};

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
    assert_eq!(recipe.identity().recipe_version(), 1);
    assert_eq!(recipe.identity().selected_declaration(), "recipe.build");
    assert_ne!(recipe.identity().original_source_digest(), recipe.identity().derived_source_digest());
    let source = recipe.canonical_source();
    assert_eq!(source.entry_declaration(), "recipe.witchy_build_recipe_entry");
    assert_eq!(&source.launch_contract_payload()[..2], &[2, 1]);
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
        "fn build(out: BuildOut, env: BuildEnv) -> Nil:\n    Nil\n",
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
