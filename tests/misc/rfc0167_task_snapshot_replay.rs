//! RFC-0167 §7: a normal lazy Task replays its captured view snapshot.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn normal_task_replays_the_view_captured_before_owner_mutation() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
async fn size(items: &List(Int)) -> Int:
    (*items).length()

async fn main(console: Console):
    var items = [1, 2]
    let recipe = size(&items)
    items.push(3)
    let first = recipe.await
    let second = recipe.await
    console.print("${first * 10 + second}")
"#).expect("normal replayable view recipe checks");
    // Construction captures the two-element root; neither await rereads items.
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("view recipe admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["22".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn affine_unique_capture_cannot_enter_a_replayable_task_recipe() {
    let error = witchy::resolve_std_only_checked(r#"
async fn affine_capture(own items: unique List(Int)) -> Int:
    items.length()

fn main() -> Int:
    let items: unique List(Int) = [1, 2]
    let recipe = affine_capture(move items)
    0
"#).err().map(|error| error.to_string())
        .expect("unique owner cannot be captured by replayable Task");
    assert!(error.contains("replayable Task recipe has no copyable snapshot: affine unique storage"),
        "checked refusal must name the capture invariant: {error}");
}

#[test]
fn call_scoped_access_cannot_enter_a_replayable_task_recipe() {
    let error = witchy::resolve_std_only_checked(r#"
async fn borrowed_capture(let items: List(Int)) -> Int:
    items.length()

async fn main():
    let answer = borrowed_capture([1, 2]).await
"#).err().map(|error| error.to_string())
        .expect("call-scoped access cannot be retained by a lazy Task");
    assert!(error.contains("call-scoped `let` parameter"),
        "checked refusal must identify the escaped access: {error}");
    assert!(error.contains("lazy Task recipe"),
        "checked refusal must identify the retaining recipe: {error}");
}
