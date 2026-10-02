//! Independent results for mutable record fields and retained snapshots.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

fn run(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).expect("record program checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("record program admits through SafeWIR");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print: true, print_int: true, quiet: true, ..Capabilities::none() }, 128,
    ).expect("admitted Wasm spawn");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    vm.output().to_vec()
}

#[test]
fn local_record_fields_keep_snapshot_semantics() {
    let source = r#"
type Stack:
    items: List(Int)
    total: Int

fn build(let n: Int) -> Int:
    var s = Stack([], 0)
    var i = 0
    while i < n:
        list.push(s.items, i)
        s.total += i
        i = i + 1
    let snapshot = s
    list.push(s.items, 99)
    s.total += 7
    snapshot.total + list.length(snapshot.items) * 1000 + s.total + list.length(s.items) * 10000

fn main(let console: Console):
    console.print("${build(20)}")
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_eq!(run(source), ["230387"]);
}

#[test]
fn opt_local_record_fields_keep_the_same_result() {
    let source = r#"
mode opt

type Stack:
    items: List(Int)
    total: Int

fn build(let n: Int) -> Stack:
    var s = Stack([], 0)
    var i = 0
    while i < n:
        list.push(s.items, i)
        s.total += i
        i = i + 1
    s

fn main(let console: Console):
    let s = build(20)
    console.print("${s.total + list.length(s.items)}")
"#;
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_eq!(run(source), ["210"]);
}
