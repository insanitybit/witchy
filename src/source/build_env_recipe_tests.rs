use super::{checked, Folder};
use std::collections::BTreeMap;
use witchy_wir::canonical_interpreter::{self as oracle, InterpreterConfig, InterpreterValue, OracleFixture};
use witchy_wir::CanonicalEntryCapability::{BuildEnv, BuildOut};

#[test]
fn build_env_empty_namespace_is_a_grant_not_an_absent_root() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let recipe = checked(&folder, "fn build(env: BuildEnv, out: BuildOut):\n    out.write_out(\"empty-grant.txt\", \"ran\")\n");
    let interpreted = folder.0.join("interpreted");
    crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
        out_dir: interpreted.clone(), env: BTreeMap::new(), ..Default::default()
    }).expect("source accepts an unused explicitly empty namespace");
    assert_eq!(std::fs::read_to_string(interpreted.join("empty-grant.txt")).unwrap(), "ran");
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("empty grant recipe");
    assert_eq!(artifact.entry().unwrap().capabilities(), &[BuildEnv, BuildOut]);
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    for granted in [false, true] {
        let compiled = folder.0.join(format!("compiled-{granted}"));
        std::fs::create_dir_all(&compiled).unwrap();
        let mut runtime = crate::runtime::Runtime::batch().unwrap();
        let result = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
            build_out: Some(compiled.clone()), build_env: granted.then(BTreeMap::new),
            ..crate::runtime::Capabilities::none()
        }, crate::RUN_MEMORY_PAGES);
        if granted {
            let mut vm = result.expect("Some(empty) is a grant");
            vm.run().unwrap();
            assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
            assert_eq!(std::fs::read_to_string(compiled.join("empty-grant.txt")).unwrap(), "ran");
        } else {
            let mut vm = result.expect("instantiation precedes entry grant minting");
            let error = vm.run().expect_err("None must refuse before entry execution");
            assert!(format!("{error:#}").contains(
                "build step demands `BuildEnv` but no environment grant was provided"
            ), "{error:#}");
            assert!(!compiled.join("empty-grant.txt").exists());
        }
        for entry_api in [false, true] {
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true,
                build_env: granted.then(|| oracle::OracleBuildEnvFixture { values: BTreeMap::new() }),
                ..Default::default()
            }, ..Default::default() };
            let parameters = vec![InterpreterValue::entry_capability(BuildEnv), InterpreterValue::entry_capability(BuildOut)];
            let result = if entry_api {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) };
            if granted {
                let run = result.expect("oracle Some(empty) is a grant");
                assert_eq!(run.leaked_objects, 0);
                assert_eq!(run.build_trace, vec![oracle::OracleBuildWrite { path: "empty-grant.txt".into(), contents: "ran".into() }]);
            } else { assert!(result.is_err(), "oracle None must refuse before entry execution"); }
        }
    }
}

#[test]
fn build_env_recipe_three_legs_preserve_presence_capture_order_and_zero_leaks() {
    witchy_interp::compiler_natives::install();
    for reversed in [false, true] {
        let folder = Folder::new();
        let parameters = if reversed { "env: BuildEnv, out: BuildOut" } else { "out: BuildOut, env: BuildEnv" };
        let recipe = checked(&folder, &format!("fn build({parameters}):\n    let read = fn(): env.get_build_env(\"present\")\n    out.write_out(\"present.txt\", read() ?? \"wrong\")\n    out.write_out(\"empty.txt\", env.get_build_env(\"empty\") ?? \"wrong\")\n    out.write_out(\"unset.txt\", env.get_build_env(\"unset\") ?? \"unset\")\n    out.write_out(\"again.txt\", read() ?? \"wrong\")\n"));
        let expected_caps = if reversed { vec![BuildEnv, BuildOut] } else { vec![BuildOut, BuildEnv] };
        let kinds = if reversed { vec![14, 9] } else { vec![9, 14] };
        assert_eq!(recipe.identity().parameter_kinds(), kinds);
        let mut abi = vec![2, 0, 0, 0];
        for kind in &kinds { abi.extend_from_slice(&[*kind, 1]); }
        assert!(recipe.canonical_source().launch_contract_payload().ends_with(&abi));
        let text = "caf\u{e9}".repeat(80);
        let values = BTreeMap::from([
            ("present".to_owned(), Some(text.clone())),
            ("empty".to_owned(), Some(String::new())), ("unset".to_owned(), None),
        ]);
        let interpreted = folder.0.join("interpreted");
        crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
            out_dir: interpreted.clone(), env: values.clone(), ..Default::default()
        }).expect("source BuildEnv snapshot");
        let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("BuildEnv admission");
        assert_eq!(artifact.entry().unwrap().capabilities(), expected_caps);
        assert!(!artifact.owner_bridges().is_empty(), "captured BuildEnv uses an authenticated owner bridge");
        let compiled = folder.0.join("compiled");
        std::fs::create_dir_all(&compiled).unwrap();
        let mut runtime = crate::runtime::Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
            build_out: Some(compiled.clone()), build_env: Some(values.clone()), ..crate::runtime::Capabilities::none()
        }, crate::RUN_MEMORY_PAGES).unwrap();
        vm.run().expect("admitted BuildEnv snapshot");
        assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
        assert!(vm.output().is_empty());
        let expected = [("present.txt", text.as_str()), ("empty.txt", ""), ("unset.txt", "unset"), ("again.txt", text.as_str())];
        for directory in [&interpreted, &compiled] {
            for (path, value) in expected {
                assert_eq!(std::fs::read_to_string(directory.join(path)).unwrap(), value);
            }
        }
        let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
        for entry_api in [false, true] {
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true,
                build_env: Some(oracle::OracleBuildEnvFixture { values: values.clone() }),
                // These unrelated Env inputs must never supply BuildEnv keys.
                environment: BTreeMap::from([("unset".into(), "must not be observed".into())]),
                process_environment: true,
                ..Default::default()
            }, ..Default::default() };
            let parameters = expected_caps.iter().copied().map(InterpreterValue::entry_capability).collect();
            let run = if entry_api {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) }.unwrap();
            assert_eq!(run.leaked_objects, 0);
            assert!(run.output.is_empty());
            assert_eq!(run.build_trace, expected.into_iter().map(|(path, contents)| oracle::OracleBuildWrite {
                path: path.into(), contents: contents.into(),
            }).collect::<Vec<_>>());
        }
    }
}

#[test]
fn build_env_recipe_denied_and_missing_grant_refuse_without_output() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let recipe = checked(&folder, "fn build(out: BuildOut, env: BuildEnv):\n    let read = fn(): env.get_build_env(\"denied\")\n    out.write_out(\"never.txt\", read() ?? \"not an unset key\")\n");
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("denied-key recipe still admits");
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    for missing in [false, true] {
        let values = if missing { BTreeMap::new() } else { BTreeMap::from([("allowed".to_owned(), None)]) };
        let interpreted = folder.0.join(format!("source-{missing}"));
        let error = crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
            out_dir: interpreted.clone(), env: values.clone(), ..Default::default()
        }).unwrap_err();
        assert!(error.message.contains("not in this BuildEnv grant's allow-list"), "{error}");
        assert!(!interpreted.join("never.txt").exists());
        let compiled = folder.0.join(format!("compiled-{missing}"));
        std::fs::create_dir_all(&compiled).unwrap();
        let mut runtime = crate::runtime::Runtime::batch().unwrap();
        let result = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
            build_out: Some(compiled.clone()), build_env: (!missing).then(|| values.clone()),
            ..crate::runtime::Capabilities::none()
        }, crate::RUN_MEMORY_PAGES).and_then(|mut vm| vm.run());
        let error = result.unwrap_err();
        if !missing {
            assert!(format!("{error:#}").contains("not in this BuildEnv grant's allow-list"), "{error:#}");
        }
        assert!(!compiled.join("never.txt").exists());
        for entry_api in [false, true] {
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true,
                build_env: (!missing).then(|| oracle::OracleBuildEnvFixture { values: values.clone() }),
                environment: BTreeMap::from([("denied".into(), "must not authorize".into())]),
                process_environment: true,
                ..Default::default()
            }, ..Default::default() };
            let parameters = vec![InterpreterValue::entry_capability(BuildOut), InterpreterValue::entry_capability(BuildEnv)];
            let result = if entry_api {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) };
            let error = result.unwrap_err();
            if !missing { assert!(error.to_string().contains("not in this BuildEnv grant's allow-list"), "{error}"); }
            // An aborted oracle run supplies no successful trace/leak result.
        }
    }
}

#[test]
fn build_env_recipe_binds_order_and_refuses_ordinary_main() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let first = checked(&folder, "fn build(env: BuildEnv, out: BuildOut):\n    out.write_out(\"x\", env.get_build_env(\"key\") ?? \"unset\")\n");
    let second = checked(&folder, "fn build(out: BuildOut, env: BuildEnv):\n    out.write_out(\"x\", env.get_build_env(\"key\") ?? \"unset\")\n");
    assert_eq!(first.identity().parameter_kinds(), &[14, 9]);
    assert_eq!(second.identity().parameter_kinds(), &[9, 14]);
    assert_ne!(first.identity().derived_source_digest(), second.identity().derived_source_digest());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&first).unwrap();
    assert!(witchy_wir::admit_canonical(validated, second.canonical_source()).is_err());
    let main = folder.source("fn main(env: BuildEnv) -> ():\n    ()\n");
    assert!(crate::link_file_checked(main.to_str().unwrap()).is_err(), "ordinary main cannot demand BuildEnv");
    for source in [
        "fn build(own env: BuildEnv, out: BuildOut) -> ():\n    ()\n",
        "fn build(var env: BuildEnv, out: BuildOut) -> ():\n    ()\n",
    ] {
        assert!(crate::source::link_build_file(folder.source(source).to_str().unwrap()).is_err());
    }
}
