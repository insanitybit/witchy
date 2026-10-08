//! A dictionary may key or hold the unit value: its table slot is the eight-
//! byte unit carrier, so `Dict(k, ())` works as a set and `Dict((), v)` holds
//! at most one entry.

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
fn unit_values_mark_membership() {
    let out = output("fn main(console: Console):\n    var seen: Dict(String, ()) = dict.new()\n    let _ = seen.insert(\"a\", ())\n    let _ = seen.insert(\"b\", ())\n    let again = seen.insert(\"a\", ())\n    console.print(\"${seen.length()} ${again == Some(())} ${seen.contains_key(\"a\")} ${seen.contains_key(\"z\")} ${seen.get(\"b\") == Some(())}\")\n");
    assert_eq!(out, ["2 true true false true"]);
}

#[test]
fn a_unit_key_holds_one_entry() {
    let out = output("fn main(console: Console):\n    var units: Dict((), Int) = dict.new()\n    let _ = units.insert((), 3)\n    let old = units.insert((), 4)\n    console.print(\"${units.length()} ${units.get_or((), 0)} ${old}\")\n");
    assert_eq!(out, ["1 4 Some(3)"]);
}

#[test]
fn an_unconstrained_empty_dict_defaults_to_unit_slots() {
    let out = output("fn main(console: Console):\n    let empty = dict.new()\n    console.print(\"${empty.length()}\")\n");
    assert_eq!(out, ["0"]);
}
