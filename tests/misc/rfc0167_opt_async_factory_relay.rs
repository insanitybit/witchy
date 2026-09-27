//! A checked opt async factory may relay an exact opt task recipe through its
//! thunk. The backend proves each factory in the chain and rejects cycles.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimization;
impl Drop for ResetOptimization {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn opt_async_factory_relay_runs_on_admitted_wasm() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimization;
    let source = r#"mode opt
async fn base() -> Int:
    42
async fn answer() -> Int:
    base().await
async fn main(console: Console):
    console.print("${answer().await}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("checked opt task chain");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("opt factory chain admits on unoptimized Wasm");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted instance");
    vm.run().expect("admitted execution");
    assert_eq!(vm.output(), ["42"]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn opt_async_factory_cycle_cannot_grant_task_authority() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimization;
    let source = r#"mode opt
async fn a() -> Int:
    b().await
async fn b() -> Int:
    a().await
async fn main(console: Console):
    console.print("${a().await}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("recursive source checks");
    let error = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_rejected("a cyclic factory chain has no terminal checked recipe");
    assert!(format!("{error:?}").contains("TaskBehaviorMismatch"), "{error:?}");
}
