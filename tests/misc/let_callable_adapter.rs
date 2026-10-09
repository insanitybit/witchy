//! A named function whose parameters borrow (`let`) fills a callable slot
//! that passes those arguments owned: the checker accepts it through an owning
//! adapter (`fn(p): f(p)`), which lends each argument and drops it. Only
//! owned-to-`let` positions adapt; `var` and the reverse direction, which
//! would need a hidden copy, are still refused.

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

fn refusal(source: &str) -> String {
    match witchy::resolve_std_only_checked(source) {
        Ok(_) => panic!("program was accepted"),
        Err(error) => format!("{error:?}"),
    }
}

#[test]
fn a_module_function_with_let_parameters_maps_a_list() {
    let out = output("fn main(console: Console):\n    let lines = [\"  a \", \" b\", \"c  \"]\n    \
console.print(lines.map(string.trim).join(\"|\"))\n    console.print(\"${lines.map(string.length)}\")\n");
    assert_eq!(out, ["a|b|c", "[4, 2, 3]"]);
}

#[test]
fn a_local_function_with_a_let_parameter_fills_owned_slots() {
    let out = output("fn shout(let s: String) -> String:\n    s.to_upper() + \"!\"\n\n\
fn apply(f: fn(String) -> String, s: String) -> String:\n    f(s)\n\n\
type Holder:\n    f: fn(String) -> String\n\n\
fn main(console: Console):\n    let g: fn(String) -> String = shout\n    let held = Holder(shout)\n    \
console.print(\"${apply(shout, \"a\")} ${g(\"b\")} ${(held.f)(\"c\")} ${[\"d\"].map(shout)}\")\n");
    assert_eq!(out, ["A! B! C! [D!]"]);
}

#[test]
fn a_let_slot_still_takes_a_let_function_unchanged() {
    let out = output("fn shout(let s: String) -> String:\n    s.to_upper() + \"!\"\n\n\
fn lend(f: fn(let String) -> String, s: String) -> String:\n    f(s)\n\n\
fn main(console: Console):\n    console.print(\"${lend(shout, \"a\")} ${lend(string.trim, \" b \")}\")\n");
    assert_eq!(out, ["A! b"]);
}

#[test]
fn an_opt_mode_function_with_a_let_parameter_keeps_its_provenance() {
    let out = output("mode opt\n\n\
fn tidy(let s: String) -> String:\n    string.trim(s)\n\n\
fn main(console: Console):\n    console.print([\" x \", \"y \"].map(tidy).join(\",\"))\n");
    assert_eq!(out, ["x,y"]);
}

#[test]
fn a_var_parameter_does_not_adapt() {
    let error = refusal("fn bump(var n: Int) -> Int:\n    n = n + 1\n    n\n\n\
fn apply(f: fn(Int) -> Int, n: Int) -> Int:\n    f(n)\n\n\
fn main(console: Console):\n    console.print(\"${apply(bump, 1)}\")\n");
    assert!(error.contains("found `fn(var Int) -> Int`"), "{error}");
}

#[test]
fn an_owned_function_does_not_fill_a_let_slot() {
    let error = refusal("fn owned(s: String) -> String:\n    s + \"!\"\n\n\
fn lend(f: fn(let String) -> String, s: String) -> String:\n    f(s)\n\n\
fn main(console: Console):\n    console.print(lend(owned, \"a\"))\n");
    assert!(error.contains("expected `fn(let String) -> String`, found `fn(String) -> String`"), "{error}");
}
