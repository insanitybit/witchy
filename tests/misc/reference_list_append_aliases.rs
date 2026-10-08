//! A list of references appends in place only while no other binding can see
//! it: every snapshot, capture, container copy and caller alias keeps the
//! value it had before the push.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

fn output(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    actor.output().to_vec()
}

#[test]
fn a_snapshot_keeps_its_length_after_the_source_appends() {
    let out = output("fn main(console: Console):\n    var xs = [\"a\"]\n    xs.push(\"b\")\n    let s = xs\n    xs.push(\"c\")\n    console.print(\"${s} ${xs}\")\n");
    assert_eq!(out, ["[a, b] [a, b, c]"]);
}

#[test]
fn a_closure_capture_keeps_its_list_after_the_source_appends() {
    let out = output("fn main(console: Console):\n    var xs = [\"a\"]\n    xs.push(\"b\")\n    let count = fn(): xs.length()\n    xs.push(\"c\")\n    console.print(\"${count()} ${xs.length()}\")\n");
    assert_eq!(out, ["2 3"]);
}

#[test]
fn a_list_stored_in_another_list_keeps_its_value_after_the_source_appends() {
    let out = output("fn main(console: Console):\n    var xs = [\"a\"]\n    xs.push(\"b\")\n    var outer: List(List(String)) = []\n    outer.push(xs)\n    xs.push(\"c\")\n    outer[0].push(\"d\")\n    console.print(\"${outer} ${xs}\")\n");
    assert_eq!(out, ["[[a, b, d]] [a, b, c]"]);
}

/// A loop that pushes and then captures its list hands a shared value to the
/// next iteration; its pushes copy instead of failing the ownership check.
#[test]
fn a_list_captured_inside_the_loop_that_pushes_it_compiles_and_keeps_snapshots() {
    let out = output("fn main(console: Console):\n    var fs = [1]\n    var i = 0\n    var total = 0\n    while i < 3:\n        fs.push(2)\n        let g = fn(x: Int) -> Int: fs.length() + x\n        total = total + g(0)\n        i = i + 1\n    console.print(\"${total} ${fs.length()}\")\n");
    assert_eq!(out, ["9 4"]);
}

/// The same loop inside a function that also makes a proper tail call:
/// lowering it a second time must not see the first attempt's tail sites.
#[test]
fn a_relowered_loop_keeps_its_function_proper_tail_call() {
    let out = output("fn rounds(n: Int, acc: Int) -> Int:\n    if n == 0:\n        return acc\n    var fs = [1]\n    var i = 0\n    var total = 0\n    while i < 2:\n        fs.push(2)\n        let g = fn(x: Int) -> Int: fs.length() + x\n        total = total + g(0)\n        i = i + 1\n    rounds(n - 1, acc + total)\n\nfn main(console: Console):\n    console.print(\"${rounds(100000, 0)}\")\n");
    assert_eq!(out, ["500000"]);
}

#[test]
fn a_caller_alias_keeps_its_list_after_a_var_parameter_appends() {
    let out = output("fn grow(var xs: List(String)):\n    xs.push(\"g\")\n\nfn main(console: Console):\n    var xs = [\"a\"]\n    xs.push(\"b\")\n    let kept = xs\n    grow(xs)\n    console.print(\"${kept} ${xs}\")\n");
    assert_eq!(out, ["[a, b] [a, b, g]"]);
}
