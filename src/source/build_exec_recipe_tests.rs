use super::{checked, Folder};
use std::collections::BTreeSet;
use witchy_wir::canonical_interpreter::{
    self as oracle, InterpreterConfig, InterpreterValue, OracleBuildExecCall,
    OracleBuildExecFixture, OracleFixture,
};
use witchy_wir::CanonicalEntryCapability::{BuildExec, BuildOut};

fn transcript(tool: &str, calls: &[(&str, &[u8], i32)]) -> OracleBuildExecFixture {
    OracleBuildExecFixture {
        tools: BTreeSet::from([tool.to_owned()]),
        expectations: calls.iter().map(|(stdin, stdout, exit_code)| OracleBuildExecCall {
            tool: tool.into(), stdin: (*stdin).into(), stdout: stdout.to_vec(),
            exit_code: *exit_code,
        }).collect(),
    }
}

#[cfg(unix)]
#[test]
fn build_exec_recipe_three_legs_capture_repeated_calls_and_argument_snapshot() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let path = std::fs::canonicalize("/bin/cat").expect("system cat fixture");
    let tool = path.to_str().unwrap();
    let recipe = checked(&folder, &format!(
        "fn mutate(var tool: String) -> String:\n    tool = \"/not-granted\"\n    \"first\\n\"\nfn build(tools: BuildExec, out: BuildOut):\n    let run = fn(text: String): tools.run_tool({tool:?}, text)\n    var tool = {tool:?}\n    let first = tools.run_tool(tool, mutate(tool))\n    let second = run(\"second\\n\")\n    out.write_out(\"first.txt\", first)\n    out.write_out(\"second.txt\", second)\n    out.write_out(\"tool.txt\", tool)\n"
    ));
    assert_eq!(recipe.identity().parameter_kinds(), &[15, 9]);
    assert!(recipe.canonical_source().launch_contract_payload().ends_with(&[2, 0, 0, 0, 15, 1, 9, 1]));
    let interpreted = folder.0.join("interpreted");
    crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
        out_dir: interpreted.clone(), exec_tools: vec![tool.into()], ..Default::default()
    }).expect("source capture and first-argument snapshot");
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("BuildExec admission");
    assert_eq!(artifact.entry().unwrap().capabilities(), &[BuildExec, BuildOut]);
    assert!(!artifact.owner_bridges().is_empty(), "captured BuildExec requires an authenticated owner bridge");
    let compiled = folder.0.join("compiled");
    std::fs::create_dir_all(&compiled).unwrap();
    let mut runtime = crate::runtime::Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
        build_out: Some(compiled.clone()), exec_allow: Some(vec![tool.into()]),
        build_exec_tools: vec![(tool.into(), path.clone())],
        ..crate::runtime::Capabilities::none()
    }, crate::RUN_MEMORY_PAGES).unwrap();
    vm.run().expect("admitted captured tool executes twice");
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
    assert!(vm.output().is_empty());
    let expected = [("first.txt", "first\n"), ("second.txt", "second\n"), ("tool.txt", "/not-granted")];
    for directory in [&interpreted, &compiled] {
        for (name, contents) in expected {
            assert_eq!(std::fs::read_to_string(directory.join(name)).unwrap(), contents);
        }
    }
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    for entry_api in [false, true] {
        let config = InterpreterConfig { fixture: OracleFixture {
            build_out: true,
            build_exec: Some(transcript(tool, &[("first\n", b"first\n", 0), ("second\n", b"second\n", 0)])),
            ..Default::default()
        }, ..Default::default() };
        let parameters = vec![InterpreterValue::entry_capability(BuildExec), InterpreterValue::entry_capability(BuildOut)];
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

#[cfg(unix)]
#[test]
fn build_exec_recipe_three_legs_stdout_only_lossy_nonzero_and_denial() {
    witchy_interp::compiler_natives::install();
    let path = std::fs::canonicalize("/bin/sh").expect("system shell fixture");
    let granted = path.to_str().unwrap();
    // Shell builtins only: this probe does not assume authority for child tools.
    let cases: [(&str, &str, &[u8], i32, Option<&str>); 3] = [
        (granted, "printf '\\377ok'; printf ignored >&2\n", b"\xffok", 0, None),
        (granted, "printf partial; printf ignored >&2; exit 7\n", b"partial", 7, Some("7")),
        ("/not-granted", "", b"", 0, Some("not in this BuildExec grant's allow-list")),
    ];
    for (tool, stdin, stdout, exit_code, failure) in cases {
        let folder = Folder::new();
        let recipe = checked(&folder, &format!(
            "fn build(out: BuildOut, tools: BuildExec):\n    out.write_out(\"before.txt\", \"before\")\n    out.write_out(\"result.txt\", tools.run_tool({tool:?}, {stdin:?}))\n"
        ));
        let interpreted = folder.0.join("interpreted");
        let source = crate::interpreter::run_build_step(recipe.original_module().module().clone(), crate::interpreter::BuildGrants {
            out_dir: interpreted.clone(), exec_tools: vec![granted.into()], ..Default::default()
        });
        let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("tool outcome does not affect admission");
        let compiled = folder.0.join("compiled");
        std::fs::create_dir_all(&compiled).unwrap();
        let mut runtime = crate::runtime::Runtime::batch().unwrap();
        let mut vm = runtime.spawn_admitted(&artifact, crate::runtime::Capabilities {
            build_out: Some(compiled.clone()), exec_allow: Some(vec![granted.into()]),
            build_exec_tools: vec![(granted.into(), path.clone())],
            ..crate::runtime::Capabilities::none()
        }, crate::RUN_MEMORY_PAGES).unwrap();
        let native = vm.run();
        if let Some(message) = failure {
            let error = source.unwrap_err();
            assert!(error.message.contains(message), "{error}");
            let error = native.unwrap_err();
            assert!(format!("{error:#}").contains(message), "{error:#}");
            // Aborted executions do not establish a successful zero-leak result.
        } else {
            source.unwrap();
            native.unwrap();
            assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
        }
        assert!(vm.output().is_empty());
        let expected = String::from_utf8_lossy(stdout);
        for directory in [&interpreted, &compiled] {
            assert_eq!(std::fs::read_to_string(directory.join("before.txt")).unwrap(), "before");
            if failure.is_some() {
                assert!(!directory.join("result.txt").exists());
            } else {
                assert_eq!(std::fs::read_to_string(directory.join("result.txt")).unwrap(), expected);
                assert_eq!(expected, "\u{fffd}ok");
            }
        }
        let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
        let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
        for entry_api in [false, true] {
            let fixture = if tool == granted { transcript(granted, &[(stdin, stdout, exit_code)]) }
                else { transcript(granted, &[]) };
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true, build_exec: Some(fixture), ..Default::default()
            }, ..Default::default() };
            let parameters = vec![InterpreterValue::entry_capability(BuildOut), InterpreterValue::entry_capability(BuildExec)];
            let result = if entry_api {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) };
            if let Some(message) = failure {
                let error = result.unwrap_err();
                assert!(error.to_string().contains(message), "{error}");
            } else {
                let run = result.unwrap();
                assert_eq!(run.leaked_objects, 0);
                assert_eq!(run.build_trace, vec![
                    oracle::OracleBuildWrite { path: "before.txt".into(), contents: "before".into() },
                    oracle::OracleBuildWrite { path: "result.txt".into(), contents: expected.to_string() },
                ]);
            }
        }
    }
}

#[test]
fn build_exec_recipe_oracle_refuses_missing_mismatched_and_unused_expectations() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let recipe = checked(&folder, "fn build(tools: BuildExec, out: BuildOut):\n    out.write_out(\"x\", tools.run_tool(\"fixture-tool\", \"input\"))\n");
    let artifact = crate::codegen::compile_checked_build_recipe_artifact(&recipe).expect_lowered("fixture source admits without executing an ambient tool");
    let function = witchy_wir::canonical_safewir::FunctionId::new(artifact.entry().unwrap().export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&recipe).unwrap();
    let fixtures = [
        (None, "not granted"),
        (Some(transcript("fixture-tool", &[("different", b"must not be returned", 0)])), "transcript mismatch"),
        (Some(transcript("fixture-tool", &[("input", b"first", 0), ("extra", b"unused", 0)])), "unused transcript expectations"),
        (Some(transcript("fixture-tool", &[])), "unexpected invocation"),
    ];
    for (fixture, message) in fixtures {
        for entry_api in [false, true] {
            let config = InterpreterConfig { fixture: OracleFixture {
                build_out: true, build_exec: fixture.clone(), ..Default::default()
            }, ..Default::default() };
            let parameters = vec![InterpreterValue::entry_capability(BuildExec), InterpreterValue::entry_capability(BuildOut)];
            let result = if entry_api {
                oracle::interpret_entry_program(&validated, function, parameters, &[], config)
            } else { oracle::interpret_program(&validated, function, parameters, config) };
            let error = result.unwrap_err();
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}

#[test]
fn build_exec_recipe_binds_order_and_refuses_ordinary_main() {
    witchy_interp::compiler_natives::install();
    let folder = Folder::new();
    let first = checked(&folder, "fn build(tools: BuildExec, out: BuildOut):\n    out.write_out(\"x\", tools.run_tool(\"fixture-tool\", \"input\"))\n");
    let second = checked(&folder, "fn build(out: BuildOut, tools: BuildExec):\n    out.write_out(\"x\", tools.run_tool(\"fixture-tool\", \"input\"))\n");
    assert_eq!(first.identity().parameter_kinds(), &[15, 9]);
    assert_eq!(second.identity().parameter_kinds(), &[9, 15]);
    assert_ne!(first.identity().derived_source_digest(), second.identity().derived_source_digest());
    let validated = crate::codegen::validate_checked_build_recipe_for_oracle(&first).unwrap();
    assert!(witchy_wir::admit_canonical(validated, second.canonical_source()).is_err());
    let main = folder.source("fn main(tools: BuildExec) -> ():\n    ()\n");
    assert!(crate::link_file_checked(main.to_str().unwrap()).is_err(), "ordinary main cannot demand BuildExec");
    for source in [
        "fn build(own tools: BuildExec, out: BuildOut) -> ():\n    ()\n",
        "fn build(var tools: BuildExec, out: BuildOut) -> ():\n    ()\n",
    ] {
        assert!(crate::source::link_build_file(folder.source(source).to_str().unwrap()).is_err());
    }
}
