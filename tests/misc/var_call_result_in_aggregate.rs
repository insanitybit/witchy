//! A `var` argument's write-back token may come back as a fresh copy-on-write
//! replacement, a unique token. Storing that binding into an aggregate after
//! the call must retain it as a reference like any other owned value.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

const BUMP: &str = "type Tally:\n    n: Int\n\n\
fn bump(var t: Tally) -> Int:\n    t.n = t.n + 1\n    t.n\n\n";

fn output(main: &str) -> Vec<String> {
    let source = format!("{BUMP}{main}");
    let checked = witchy::resolve_std_only_checked(&source).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    assert_eq!(actor.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
    actor.output().to_vec()
}

#[test]
fn a_written_back_binding_is_stored_in_a_tuple() {
    let out = output("fn main(console: Console):\n    var s = Tally(0)\n    let r = bump(s)\n    let d = (s, r)\n    console.print(\"${r} ${d.0.n}\")\n");
    assert_eq!(out, ["1 1"]);
}

#[test]
fn a_written_back_copy_is_stored_in_a_record_and_leaves_its_source_alone() {
    let out = output("type Box:\n    v: Tally\n\n\
fn main(console: Console):\n    let p = Tally(0)\n    var s = p\n    let r = bump(s)\n    let d = Box(s)\n    console.print(\"${r} ${d.v.n} ${p.n}\")\n");
    assert_eq!(out, ["1 1 0"]);
}
