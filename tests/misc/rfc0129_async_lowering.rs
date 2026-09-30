//! RFC-0129 row 1: admitted async lowering retains source semantics and diagnostics.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, typeck};
use witchy_types::suspension_carrier::{CarrierLane, SuspensionCarrierCatalog};

const STATE_MACHINE_SOURCE: &str = r#"
async fn fold(limit: Int, stop_at: Int) -> Int:
    var total: Int = 0
    var index: Int = 0
    while index < limit:
        let next = task.done(index + 1).await
        total = total + next
        index = index + 1
        if index == stop_at:
            return total
    return total

async fn main(console: Console):
    let early: Int = fold(5, 3).await
    let complete: Int = fold(4, 99).await
    console.print("${early} ${complete}")
"#;

const TRAP_SOURCE: &str = r#"
async fn explode() -> Int:
    let _ = task.done(0).await
    let divisor: Int = 0
    42 / divisor

async fn main(console: Console):
    let _ = explode().await
"#;

fn checked(source: &str) -> witchy::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source).expect("RFC-0129 row-1 source must check")
}

fn compiled_result(
    module: &witchy::pipeline::CheckedModule,
) -> Result<Vec<String>, String> {
    let artifact = codegen::compile_checked_module_artifact(module)
        .expect_lowered("compile RFC-0129 row-1 corpus");
    let mut runtime = Runtime::batch().expect("create RFC-0129 row-1 runtime");
    let mut actor = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn RFC-0129 row-1 corpus");
    actor
        .run()
        .map_err(|error| error.root_cause().to_string())?;
    assert_eq!(actor.canonical_heap_leaks().expect("heap audit"), Some(0));
    Ok(actor.output())
}

fn source_line(source: &str, needle: &str) -> u32 {
    let mut matches = source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains(needle));
    let (index, _) = matches.next().expect("source marker exists");
    assert!(matches.next().is_none(), "source marker is unique");
    u32::try_from(index + 1).expect("source line fits u32")
}

#[test]
fn rfc0129_acceptance_row_1_async_lowering_preserves_source_contracts_on_admitted_wasm() {
    struct ResetOptimization;
    impl Drop for ResetOptimization {
        fn drop(&mut self) { witchy::opt::set_for_tests(None); }
    }
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimization;
    let state_machine = checked(STATE_MACHINE_SOURCE);
    let expected = vec!["6 10".to_string()];
    assert_eq!(
        compiled_result(&state_machine).expect("run compiled RFC-0129 state-machine corpus"),
        expected,
        "compiled Wasm must preserve mutable loop state and early return",
    );

    let mut lowered = witchy_types::traits::lower_checked(state_machine.module().clone())
        .expect("lower checked traits before carrier annotation");
    witchy::parser::lower_sugar_module(&mut lowered);
    let typed = typeck::annotate_checked(lowered)
        .expect("finalize types on the async-lowered module");
    let carrier = SuspensionCarrierCatalog::from_typed(&typed)
        .expect("build typed async carrier catalog");
    let fold_segments = carrier
        .states()
        .iter()
        .filter(|state| state.function.contains("__async_fold_"))
        .collect::<Vec<_>>();
    assert!(!fold_segments.is_empty(), "fold must produce suspension segments");
    let mut finalized_slots = std::collections::BTreeSet::new();
    for slot in fold_segments.iter().flat_map(|state| &state.slots) {
        if matches!(slot.name.as_str(), "limit" | "stop_at" | "total" | "index" | "next") {
            finalized_slots.insert(slot.name.as_str());
            assert_eq!(slot.ty, witchy::ast::Type::Named("Int".into(), Vec::new()));
            assert_eq!(slot.lanes, Some(vec![CarrierLane::I64]));
        }
    }
    for expected_slot in ["limit", "stop_at", "total", "index", "next"] {
        assert!(
            finalized_slots.contains(expected_slot),
            "typed carrier omitted `{expected_slot}`: {finalized_slots:?}",
        );
    }

    let trapping = checked(TRAP_SOURCE);
    let compiled = compiled_result(&trapping)
        .expect_err("compiled Wasm must preserve the async trap");

    let line = source_line(TRAP_SOURCE, "42 / divisor");
    assert_eq!(compiled, format!("runtime error: `main.explode`, line {line}: division by zero"));
    assert!(
        !compiled.contains("__async_"),
        "generated segment identity leaked into async trap: {compiled}",
    );
}
