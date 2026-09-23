//! RFC-0167 row G: `?` on an `Option(&Int)` / `Result(&Int, E)` operand.
//!
//! An aggregate whose payload is an `&Int` view is an opaque owner, so `?`
//! reads its tag and payloads through the owner family (as `match` does)
//! rather than as fields of a plain tagged record. The payload read carries
//! its own token, so release placement no longer loses it.
//! Independent expectation, admitted unoptimized Wasm, zero leaks.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_wasm(source: &str, expected: i64) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admission");

    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()], "admitted Wasm matches the expectation");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0),
        "admitted Wasm releases all roots");
}

const OPTION_PICK: &str = r#"
fn pick(xs: &[Int], i: Int) -> Option(&Int):
    if i < xs.length(): Some(&xs[i]) else: None

fn get(xs: &[Int], i: Int) -> Option(Int):
    let v = pick(xs, i)?
    Some(*v + 1)
"#;

const RESULT_PICK: &str = r#"
fn pick(xs: &[Int], i: Int) -> Result(&Int, String):
    if i < xs.length(): Ok(&xs[i]) else: Err("out of range")

fn get(xs: &[Int], i: Int) -> Result(Int, String):
    let v = pick(xs, i)?
    Ok(*v * 2)
"#;

#[test]
fn try_on_an_option_int_view_continues_with_the_payload() {
    // xs[1] = 20, plus one.
    assert_wasm(&format!("{OPTION_PICK}
fn main() -> Int:
    let xs = [10, 20, 30]
    get(&xs[0..3], 1) ?? 0
"), 21);
}

#[test]
fn try_on_an_option_int_view_propagates_none() {
    // Index 7 is out of range: `?` returns None, `??` yields 5.
    assert_wasm(&format!("{OPTION_PICK}
fn main() -> Int:
    let xs = [10, 20, 30]
    get(&xs[0..3], 7) ?? 5
"), 5);
}

#[test]
fn try_on_a_result_int_view_continues_with_the_payload() {
    // xs[2] = 30, doubled.
    assert_wasm(&format!("{RESULT_PICK}
fn main() -> Int:
    let xs = [10, 20, 30]
    match get(&xs[0..3], 2):
        Ok(n) -> n
        Err(_) -> 0
"), 60);
}

#[test]
fn try_on_a_result_int_view_propagates_the_error() {
    // "out of range" has 12 characters.
    assert_wasm(&format!("{RESULT_PICK}
fn main() -> Int:
    let xs = [10, 20, 30]
    match get(&xs[0..3], 9):
        Ok(_) -> 0
        Err(e) -> e.length()
"), 12);
}

#[test]
fn try_on_an_int_view_inside_a_loop() {
    // n = 4 sums 1+2+3+4 = 10; n = 6 fails at i = 4, so `??` yields 99.
    assert_wasm(&format!("{OPTION_PICK}
fn total(xs: &[Int], n: Int) -> Option(Int):
    var sum = 0
    for i in 0..n:
        let v = pick(xs, i)?
        sum = sum + *v
    Some(sum)

fn main() -> Int:
    let xs = [1, 2, 3, 4]
    let a = total(&xs[0..4], 4) ?? 0
    let b = total(&xs[0..4], 6) ?? 99
    a * 1000 + b
"), 10099);
}

#[test]
fn try_relays_an_int_view_into_an_int_view_result() {
    // The failure is rebuilt as the caller's own opaque Result: xs[1] = 8,
    // and "missing" has 7 characters.
    assert_wasm(r#"
fn pick(xs: &[Int], i: Int) -> Result(&Int, String):
    if i < xs.length(): Ok(&xs[i]) else: Err("missing")

fn relay(xs: &[Int], i: Int) -> Result(&Int, String):
    let v = pick(xs, i)?
    Ok(v)

fn main() -> Int:
    let xs = [7, 8, 9]
    let a = match relay(&xs[0..3], 1):
        Ok(v) -> *v
        Err(_) -> 0
    let b = match relay(&xs[0..3], 5):
        Ok(_) -> 0
        Err(e) -> e.length()
    a * 100 + b
"#, 807);
}
