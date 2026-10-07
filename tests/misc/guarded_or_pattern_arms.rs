//! A guarded arm followed by an or-pattern arm that shares its body between
//! the guard's failure and another constructor. The guard's success returns
//! around that shared body, so structured emission must merge at the body
//! and copy only the one-block return tail.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

const SOURCE: &str = "type Shape:\n    Circle(Int)\n    Square(Int)\n\n\
fn describe(s: Shape) -> String:\n    match s:\n        Circle(r) if r > 100 -> \"big circle\"\n        Circle(r) | Square(r) -> \"small \" + \"${r}\"\n\n\
fn main(console: Console):\n    console.print(describe(Circle(200)))\n    console.print(describe(Circle(2)))\n    console.print(describe(Square(300)))\n";

#[test]
fn a_guarded_arm_before_a_shared_or_pattern_body_compiles() {
    let checked = witchy::resolve_std_only_checked(SOURCE).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime
        .spawn_admitted(&artifact, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, 128)
        .expect("spawn program");
    actor.run().expect("run program");
    assert_eq!(actor.output(), ["big circle", "small 2", "small 300"]);
}
