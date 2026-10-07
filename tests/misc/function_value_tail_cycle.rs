//! A named function used as a value is called through an adapter. Its
//! forwarding call is a proper tail call, so a cycle through function values
//! runs in bounded stack like a direct one.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

const SOURCE: &str = "type Bounce:\n    Bounce(fn(Bounce, Int) -> Int)\n\n\
fn drive(bounce: Bounce, n: Int) -> Int:\n    match bounce:\n        Bounce(f) -> f(bounce, n)\n\n\
fn step(bounce: Bounce, n: Int) -> Int:\n    if n == 0:\n        7\n    else:\n        drive(bounce, n - 1)\n\n\
fn main(console: Console):\n    console.print(\"${drive(Bounce(step), 1000000)}\")\n";

#[test]
fn a_million_bounces_through_a_function_value_run_in_bounded_stack() {
    let checked = witchy::resolve_std_only_checked(SOURCE).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    assert_eq!(actor.output(), ["7"]);
}
