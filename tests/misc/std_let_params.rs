//! Read-only std functions take their heap arguments as `let` borrows, so an
//! opt-mode caller holding a borrow passes it without an `.owned()` copy. A
//! `unique` result computed from a borrow is its own token; returning the
//! borrowed place itself as `unique` is still refused.

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
fn a_borrowed_buffer_reaches_read_only_bytes_functions_without_a_copy() {
    let out = output("mode opt\n\nimport bytes\n\n\
fn window(let b: Bytes, start: Int) -> String:\n    bytes.slice(b, start, start + 3).to_string()\n\n\
fn main(console: Console):\n    let b = bytes.from_string(\"abcdefgh\")\n    console.print(\"${window(b, 2)} ${b.contains(bytes.from_string(\"fg\"))} ${b.index_of(bytes.from_string(\"h\"))}\")\n");
    assert_eq!(out, ["cde true Some(7)"]);
}

#[test]
fn a_borrowed_string_reaches_read_only_string_functions_without_a_copy() {
    let out = output("mode opt\n\n\
fn describe(let s: String) -> String:\n    let parts = string.split(s, \",\")\n    \"${parts.length()} ${string.contains(s, \"b\")} ${string.starts_with(s, \"a\")} ${string.substring(s, 2, 3)}\"\n\n\
fn main(console: Console):\n    console.print(describe(\"a,b,c\"))\n");
    assert_eq!(out, ["3 true true b"]);
}

#[test]
fn returning_a_borrowed_place_as_unique_is_refused() {
    let error = witchy::resolve_std_only_checked("mode opt\n\nimport bytes\n\n\
fn leak(let b: Bytes) -> unique Bytes:\n    b\n\n\
fn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    console.print(\"${leak(b).length()}\")\n")
        .err().expect("a borrowed place is not a unique result");
    assert!(format!("{error:?}").contains("cannot escape through a returned value"), "{error:?}");
}

#[test]
fn a_borrow_wrapped_in_a_unique_record_is_refused() {
    let error = witchy::resolve_std_only_checked("type Box:\n    items: List(Int)\n\n\
fn wrap(let xs: List(Int)) -> unique Box:\n    Box(xs)\n\n\
fn main(console: Console):\n    var xs = [1, 2]\n    let b = wrap(xs)\n    xs[0] = 9\n    console.print(show(b.items[0]))\n")
        .err().expect("a record holding the borrow is not a unique result");
    assert!(format!("{error:?}").contains("cannot escape through a returned value"), "{error:?}");
}

#[test]
fn a_branch_that_returns_a_borrowed_place_as_unique_is_refused() {
    let error = witchy::resolve_std_only_checked("mode opt\n\nimport bytes\n\n\
fn pick(let b: Bytes, c: Bool) -> unique Bytes:\n    if c: b else: bytes.slice(b, 0, 1)\n\n\
fn main(console: Console):\n    let b = bytes.from_string(\"hi\")\n    console.print(\"${pick(b, true).length()}\")\n")
        .err().expect("a branch returning the borrowed place is not a unique result");
    assert!(format!("{error:?}").contains("cannot escape through a returned value"), "{error:?}");
}
