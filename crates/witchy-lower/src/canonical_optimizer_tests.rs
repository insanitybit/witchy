use witchy_syntax::origin::OriginTable;
use witchy_wir::canonical_interpreter::{InterpretError, InterpreterValue, interpret};
use witchy_wir::canonical_safewir::optimize::{
    CheckedPassOutcome, PassId, run_checked_pass,
    eliminate_same_block_redundant_bounds_guards_checked,
};
use witchy_wir::canonical_safewir::{
    GuardKind, Operation, check_initial_with_witness, validate_final, validate_optimized_final,
};

#[test]
fn source_bounds_guard_optimization_admits_and_matches_execution() {
    let checked = witchy_types::pipeline::link_checked_from_source_bundle(
        vec![(
            "main".to_string(),
            b"fn main() -> Int:\n    let xs = [41]\n    xs[0]\n".to_vec(),
        )],
        "main",
        |_, _, _| Ok(OriginTable::default()),
    )
    .expect("source fixture checks");
    let source = checked
        .canonical_source()
        .expect("canonical checked source");
    let layouts = crate::canonical_layouts::derive_layout_bundle(source).expect("layouts");
    let candidate = super::lower_typed_module(source, layouts)
        .expect("source fixture lowers")
        .with_launch_contract_payload(source.launch_contract_payload().to_vec())
        .transform_functions(|functions| {
            let block = functions
                .iter_mut()
                .flat_map(|function| &mut function.blocks)
                .find(|block| {
                    block.operations.iter().any(|operation| {
                        matches!(
                            operation,
                            Operation::Guard {
                                kind: GuardKind::Bounds,
                                ..
                            }
                        )
                    })
                })
                .expect("lowered indexed read has a bounds guard");
            let position = block
                .operations
                .iter()
                .position(|operation| {
                    matches!(
                        operation,
                        Operation::Guard {
                            kind: GuardKind::Bounds,
                            ..
                        }
                    )
                })
                .unwrap();
            let duplicate = block.operations[position].clone();
            block.operations.insert(position + 1, duplicate);
        });
    let main = candidate.functions()[0].id;
    let initial_revision = candidate.revision();
    let initial_guard_count = candidate
        .functions()
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.operations)
        .filter(|operation| {
            matches!(
                operation,
                Operation::Guard {
                    kind: GuardKind::Bounds,
                    ..
                }
            )
        })
        .count();

    let zero_pass = validate_final(
        check_initial_with_witness(candidate.clone(), source).expect("zero-pass initial authority"),
        source,
    )
    .expect("zero-pass final authority");
    let zero_pass_artifact =
        witchy_wir::admit_canonical(zero_pass, source).expect("zero-pass module admits");

    let checked = check_initial_with_witness(candidate, source).expect("initial authority");
    let CheckedPassOutcome::Applied(optimized) = run_checked_pass(checked, PassId::SameBlockBoundsGuards) else {
        panic!("the production driver must apply this valid optimization");
    };
    assert_eq!(
        optimized.module().revision(),
        initial_revision.checked_next().unwrap()
    );
    let optimized_guard_count = optimized
        .module()
        .functions()
        .iter()
        .flat_map(|function| &function.blocks)
        .flat_map(|block| &block.operations)
        .filter(|operation| {
            matches!(
                operation,
                Operation::Guard {
                    kind: GuardKind::Bounds,
                    ..
                }
            )
        })
        .count();
    assert!(
        optimized_guard_count < initial_guard_count,
        "the assured pass must remove at least the injected duplicate bounds guard"
    );
    let validated = validate_optimized_final(optimized, source).expect("optimized final authority");

    let (optimized_artifact, (interpreted, compiled)) =
        witchy_wir::admit_canonical_with_observer(validated, source, |validated, artifact| {
            let interpreted = interpret(validated, main, Vec::new()).expect("interpreter run");
            let engine = wasmtime::Engine::default();
            let module = wasmtime::Module::new(&engine, artifact.wasm()).expect("Wasm module");
            let mut store = wasmtime::Store::new(&engine, ());
            let instance = crate::canonical_pipeline::tests::fixture_diagnostic_linker(&engine)
                .instantiate(&mut store, &module)
                .expect("Wasm instance");
            let export = artifact.entry().expect("program entry").export();
            let compiled = instance
                .get_typed_func::<(), i64>(&mut store, export)
                .expect("typed entry")
                .call(&mut store, ())
                .expect("Wasm run");
            (interpreted, compiled)
        })
        .expect("optimized module admits");
    assert_ne!(
        optimized_artifact.safe_wir_digest(),
        zero_pass_artifact.safe_wir_digest()
    );
    assert_ne!(
        optimized_artifact.authority_digest(),
        zero_pass_artifact.authority_digest()
    );
    assert_ne!(optimized_artifact.wasm(), zero_pass_artifact.wasm());
    assert_eq!(interpreted, vec![InterpreterValue::Int(compiled)]);
    assert_eq!(compiled, 41);
}

#[test]
fn generated_bounds_guard_cases_preserve_interpreter_observations() {
    for length in 1..=8 {
        let elements = (0..length)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        for index in -1..=length + 1 {
            let source_text = format!(
                "fn main() -> Int:\n    let xs = [{elements}]\n    xs[{index}]\n"
            );
            let checked = witchy_types::pipeline::link_checked_from_source_bundle(
                vec![("main".to_string(), source_text.into_bytes())],
                "main",
                |_, _, _| Ok(OriginTable::default()),
            )
            .expect("generated source checks");
            let source = checked
                .canonical_source()
                .expect("canonical checked source");
            let layouts = crate::canonical_layouts::derive_layout_bundle(source)
                .expect("generated layouts");
            let candidate = super::lower_typed_module(source, layouts)
                .expect("generated source lowers")
                .with_launch_contract_payload(source.launch_contract_payload().to_vec())
                .transform_functions(|functions| {
                    let block = functions
                        .iter_mut()
                        .flat_map(|function| &mut function.blocks)
                        .find(|block| {
                            block.operations.iter().any(|operation| {
                                matches!(
                                    operation,
                                    Operation::Guard {
                                        kind: GuardKind::Bounds,
                                        ..
                                    }
                                )
                            })
                        })
                        .expect("generated indexed read has a bounds guard");
                    let position = block
                        .operations
                        .iter()
                        .position(|operation| {
                            matches!(
                                operation,
                                Operation::Guard {
                                    kind: GuardKind::Bounds,
                                    ..
                                }
                            )
                        })
                        .expect("bounds guard position");
                    block.operations.insert(position + 1, block.operations[position].clone());
                });
            let main = candidate.functions()[0].id;
            let baseline_guard_count = candidate
                .functions()
                .iter()
                .flat_map(|function| &function.blocks)
                .flat_map(|block| &block.operations)
                .filter(|operation| {
                    matches!(
                        operation,
                        Operation::Guard {
                            kind: GuardKind::Bounds,
                            ..
                        }
                    )
                })
                .count();

            let baseline = validate_final(
                check_initial_with_witness(candidate.clone(), source)
                    .expect("generated baseline validates initially"),
                source,
            )
            .expect("generated baseline validates finally");
            let optimized = eliminate_same_block_redundant_bounds_guards_checked(
                check_initial_with_witness(candidate, source)
                    .expect("generated optimization input validates"),
            )
            .expect("generated relation-checked optimization");
            let optimized_guard_count = optimized
                .module()
                .functions()
                .iter()
                .flat_map(|function| &function.blocks)
                .flat_map(|block| &block.operations)
                .filter(|operation| {
                    matches!(
                        operation,
                        Operation::Guard {
                            kind: GuardKind::Bounds,
                            ..
                        }
                    )
                })
                .count();
            assert!(
                optimized_guard_count < baseline_guard_count,
                "generated case was not transformed for length={length}, index={index}",
            );
            let optimized = validate_optimized_final(optimized, source)
                .expect("generated optimized graph validates finally");

            let baseline_observation = interpret(&baseline, main, Vec::new());
            let optimized_observation = interpret(&optimized, main, Vec::new());
            if (0..length).contains(&index) {
                let expected = vec![InterpreterValue::Int(i64::from(index))];
                assert_eq!(baseline_observation, Ok(expected.clone()));
                assert_eq!(optimized_observation, Ok(expected));
            } else {
                let expected = format!(
                    "runtime error: `main`, line 3: list index {index} out of bounds (length {length})"
                );
                for observation in [&baseline_observation, &optimized_observation] {
                    let Err(InterpretError::DiagnosticAbort { message, output }) = observation else {
                        panic!("expected bounds diagnostic, got {observation:?}");
                    };
                    assert_eq!(message, &expected, "wrong bounds diagnostic");
                    assert!(output.is_empty(), "bounds refusal produced output: {output:?}");
                }
            }
            assert_eq!(
                format!("{optimized_observation:?}"),
                format!("{baseline_observation:?}"),
                "bounds optimization changed length={length}, index={index}",
            );

            if index == -1 || index == length {
                for (route, validated) in [("baseline", baseline), ("optimized", optimized)] {
                    let artifact = witchy_wir::admit_canonical(validated, source)
                        .expect("generated boundary module admits");
                    let engine = wasmtime::Engine::default();
                    let module = wasmtime::Module::new(&engine, artifact.wasm())
                        .expect("generated boundary Wasm");
                    let mut store = wasmtime::Store::new(&engine, ());
                    let instance = crate::canonical_pipeline::tests::fixture_diagnostic_linker(&engine)
                        .instantiate(&mut store, &module)
                        .expect("instantiate generated boundary Wasm");
                    let entry = artifact.entry().expect("generated entry").export();
                    let function = instance
                        .get_typed_func::<(), i64>(&mut store, entry)
                        .expect("generated typed entry");
                    assert!(
                        function.call(&mut store, ()).is_err(),
                        "{route} Wasm lost the bounds trap for length={length}, index={index}",
                    );
                }
            }
        }
    }
}
