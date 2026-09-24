use witchy_syntax::origin::OriginTable;
use witchy_wir::canonical_interpreter::{InterpretError, InterpreterValue, interpret};
use witchy_wir::canonical_safewir::optimize::eliminate_same_block_redundant_bounds_guards_checked;
use witchy_wir::canonical_safewir::{
    GuardKind, Operation, check_initial_with_witness, validate_final, validate_optimized_final,
};

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
                check_initial_with_witness(candidate.clone(), &source.view())
                    .expect("generated baseline validates initially"),
                &source.view(),
            )
            .expect("generated baseline validates finally");
            let optimized = eliminate_same_block_redundant_bounds_guards_checked(
                check_initial_with_witness(candidate, &source.view())
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
            let optimized = validate_optimized_final(optimized, &source.view())
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
                    let artifact = witchy_wir::admit_canonical(validated, &source.view())
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
