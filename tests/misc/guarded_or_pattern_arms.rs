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


/// Inside a loop the guard's success reaches the latch around the shared
/// body, which itself loops: the latch is the copied tail, never the body.
#[test]
fn a_guarded_arm_inside_a_loop_skips_a_looping_shared_body() {
    assert_eq!(output("type Shape:\n    Circle(Int)\n    Square(Int)\n    Dot\n\nfn main(console: Console):\n    let shapes = [Circle(200), Circle(2), Square(3), Dot]\n    var i = 0\n    var total = 0\n    while i < shapes.length():\n        match shapes[i]:\n            Circle(r) if r > 100 -> total = total + 1000\n            Circle(r) | Square(r) ->\n                var k = 0\n                while k < r:\n                    total = total + 1\n                    k = k + 1\n            Dot -> total = total + 7\n        i = i + 1\n    console.print(\"${total}\")\n"), ["1012"]);
}

/// The same shape with `break` in the guarded arm, a guarded `continue` arm,
/// and the whole loop driven from another loop.
#[test]
fn guarded_or_pattern_arms_with_loop_exits_inside_nested_loops() {
    assert_eq!(output("type Shape:\n    Circle(Int)\n    Square(Int)\n    Dot\n\nfn spin(n: Int) -> Int:\n    var k = 0\n    var acc = 0\n    while k < n:\n        acc = acc + 1\n        k = k + 1\n    acc\n\nfn run(shapes: List(Shape), stop_big: Bool) -> Int:\n    var i = 0\n    var total = 0\n    while i < shapes.length():\n        let s = shapes[i]\n        i = i + 1\n        match s:\n            Circle(r) if r > 100 ->\n                if stop_big:\n                    break\n                var j = 0\n                while j < 3:\n                    total = total + 100\n                    j = j + 1\n            Circle(r) | Square(r) if r == 0 -> continue\n            Circle(r) | Square(r) ->\n                total = total + spin(r)\n                var m = 0\n                while m < r:\n                    total = total + 10\n                    m = m + 1\n            Dot -> total = total + 7\n    total\n\nfn main(console: Console):\n    let shapes = [Circle(2), Square(0), Circle(200), Square(3), Dot]\n    var outer = 0\n    var rounds = 0\n    while rounds < 2:\n        outer = outer + run(shapes, rounds == 0)\n        rounds = rounds + 1\n    console.print(\"${run(shapes, false)} ${run(shapes, true)} ${outer}\")\n"), ["362 22 384"]);
}

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
