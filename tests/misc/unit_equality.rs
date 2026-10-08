//! Unit has one value: `()` equals `()` directly, inside tuples and lists,
//! and through the standard `Hash`/`Ord` protocols.

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
fn unit_values_compare_equal_alone_and_in_aggregates() {
    let out = output("fn main(console: Console):\n    let a = ((), 3)\n    let b = ((), 4)\n    console.print(\"${() == ()} ${() != ()} ${a == b} ${[(), ()] == [(), ()]}\")\n");
    assert_eq!(out, ["true false false true"]);
}

#[test]
fn unit_ordering_operators_compare_one_value_with_itself() {
    let out = output("fn main(console: Console):\n    console.print(\"${() < ()} ${() <= ()} ${() > ()} ${() >= ()}\")\n");
    assert_eq!(out, ["false true false true"]);
}

#[test]
fn unit_satisfies_the_standard_hash_and_order_protocols() {
    let out = output("fn digest(x: a) -> Int where a: Hash:\n    x.hash()\n\n\
fn least(x: a, y: a) -> a where a: Ord:\n    if x.compare(y) == Greater: y else: x\n\n\
fn main(console: Console):\n    console.print(\"${digest(())} ${least((), ()) == ()}\")\n");
    assert_eq!(out, ["0 true"]);
}
