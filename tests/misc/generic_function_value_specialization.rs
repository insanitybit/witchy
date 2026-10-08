//! A generic function passed as a value into another generic call is
//! specialized once. A second identical copy of the instance gave two items
//! one canonical identity and broke its proper-tail and ownership facts.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

const SOURCE: &str = "import func\n\n\
fn main(console: Console):\n    var people = [(\"ada\", 36), (\"babbage\", 49), (\"turing\", 41)]\n    \
people.sort_by(func.on_key(fn(a: Int, b: Int): a < b, func.second))\n    \
for p in people:\n        console.print(\"${func.first(p)}: ${func.second(p)}\")\n";

#[test]
fn a_generic_function_value_inside_a_generic_call_sorts() {
    let checked = witchy::resolve_std_only_checked(SOURCE).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    assert_eq!(actor.output(), ["ada: 36", "turing: 41", "babbage: 49"]);
}
