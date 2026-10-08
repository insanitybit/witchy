//! A capture-free lambda read in a loop shares one closure record; one that is
//! stored in an aggregate keeps its own record. Both compute the same values.

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
fn a_shared_update_callback_counts_every_key() {
    let out = output("fn main(console: Console):\n    var counts = dict.new()\n    var i = 0\n    while i < 30:\n        counts.update(i % 7, 0, fn(c: Int) -> Int: c + 1)\n        i = i + 1\n    console.print(\"${counts.length()} ${counts.get_or(0, 0)} ${counts.get_or(6, 0)}\")\n");
    assert_eq!(out, ["7 5 4"]);
}

#[test]
fn lambdas_stored_in_aggregates_inside_a_loop_stay_independent() {
    let out = output("type Op:\n    f: fn(Int) -> Int\n\nfn main(console: Console):\n    var t = 0\n    var i = 0\n    while i < 3:\n        let fs = [fn(x: Int) -> Int: x + 1]\n        let op = Op(fn(x: Int) -> Int: x * 3)\n        let pair = (fn(x: Int) -> Int: x - 1, 5)\n        let o = Some(fn(x: Int) -> Int: x + 100)\n        let h = match o:\n            Some(g) -> g(1)\n            None -> 0\n        t = t + fs[0](i) + (op.f)(i) + (pair.0)(i) + pair.1 + h\n        i = i + 1\n    console.print(\"${t}\")\n");
    assert_eq!(out, ["333"]);
}

#[test]
fn a_shared_callback_argument_maps_every_round() {
    let out = output("fn main(console: Console):\n    var total = 0\n    var i = 0\n    while i < 4:\n        let ys = [1, 2, 3].map(fn(x: Int) -> Int: x * 10)\n        total = total + ys[i % 3]\n        i = i + 1\n    console.print(\"${total}\")\n");
    assert_eq!(out, ["70"]);
}
