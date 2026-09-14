use witchy_syntax::origin::OriginTable;
use witchy_wir::canonical_interpreter::{InterpreterValue, interpret};
use witchy_wir::canonical_safewir::optimize::eliminate_same_block_redundant_bounds_guards_checked;
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
    let optimized = eliminate_same_block_redundant_bounds_guards_checked(checked)
        .expect("relation-checked optimization");
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
