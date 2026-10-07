//! A nullary constructor of a generic type passed straight to a generic
//! function leaves its type parameter unconstrained. No value of it can hold
//! one, so the program compiles with the parameter defaulted to unit.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

fn output_of(source: &str) -> Vec<String> {
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
fn an_empty_generic_constructor_argument_compiles() {
    let source = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\n\
fn size(s: Stack(a)) -> Int:\n    match s:\n        Empty -> 0\n        Push(_x, rest) -> 1 + size(rest)\n\n\
fn main(console: Console):\n    console.print(\"${size(Empty)} ${size(Push(7, Empty))}\")\n";
    assert_eq!(output_of(source), ["0 1"]);
}

#[test]
fn a_bare_none_argument_compiles() {
    let source = "fn present(o: Option(a)) -> Int:\n    match o:\n        Some(_) -> 1\n        None -> 0\n\n\
fn main(console: Console):\n    console.print(\"${present(None)} ${present(Some(\\\"x\\\"))}\")\n";
    assert_eq!(output_of(source), ["0 1"]);
}
