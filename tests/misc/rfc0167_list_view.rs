//! RFC-0167 row G: a view of a whole `List` of scalars or Strings.
//!
//! `&xs` is the retained list object (`ListRoot`). A later mutation of the
//! owner finds the list shared and copies, so the view keeps its snapshot.
//! Independent expectation, canonical SafeWIR interpreter, admitted Wasm, zero leaks.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_entry_program, InterpreterValue};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_canonical_and_wasm(source: &str, expected: i64) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;

    let checked = witchy::resolve_std_only_checked(source).expect("fixture type-checks");
    let (artifact, oracle) = codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("canonical entry");
            let function = witchy_wir::canonical_safewir::FunctionId::new(
                entry.export().strip_prefix('f').expect("numeric entry export")
                    .parse().expect("numeric entry function"),
            );
            let tokens = entry.capabilities().iter().copied()
                .map(InterpreterValue::entry_capability).collect();
            interpret_entry_program(validated, function, tokens, &[], Default::default())
                .expect("canonical SafeWIR execution")
        },
    )
    .expect_lowered("canonical SafeWIR admission");

    assert_eq!(oracle.results, vec![InterpreterValue::Int(expected)],
        "canonical interpreter matches the independent expected result");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter releases all roots");

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

#[test]
fn a_list_view_crosses_a_call() {
    assert_canonical_and_wasm(r#"
fn count(xs: &List(Int)) -> Int:
    (*xs).length()

fn main() -> Int:
    let xs = [1, 2, 3]
    count(&xs)
"#, 3);
}

#[test]
fn a_list_view_keeps_its_snapshot_when_the_owner_pushes() {
    assert_canonical_and_wasm(r#"
fn total(xs: &List(Int)) -> Int:
    var sum = 0
    for x in (*xs):
        sum = sum + x
    sum

fn main() -> Int:
    var xs = [1, 2, 3]
    let v = &xs
    xs.push(10)
    total(v) * 100 + total(&xs)
"#, 616);
}

#[test]
fn a_list_view_keeps_its_snapshot_across_a_var_write_back() {
    assert_canonical_and_wasm(r#"
fn grow(var xs: List(Int)):
    xs.push(1)
    xs.push(2)
    xs.push(3)
    xs.push(4)

fn main() -> Int:
    var xs = [7]
    let v = &xs
    grow(xs)
    (*v).length() * 10 + xs.length()
"#, 15);
}

#[test]
fn a_view_of_a_string_list_reads_its_elements() {
    assert_canonical_and_wasm(r#"
fn joined_length(xs: &List(String)) -> Int:
    var n = 0
    for s in (*xs):
        n = n + s.length()
    n

fn main() -> Int:
    let words = ["ab", "cde"]
    joined_length(&words)
"#, 5);
}

#[test]
fn a_region_born_list_cannot_escape_through_its_view() {
    let source = r#"
fn bad() -> &List(Int):
    region -> &List(Int):
        let local = [1, 2]
        &local
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a region-born list root must not escape through its view");
    assert!(error.to_string().contains("region"), "{error}");
}

/// Every list mutation keeps a held view on its snapshot: an indexed store,
/// `pop`, a callee's `var` indexed store, and push plus store with the view
/// copied. Stores and push write into fresh storage; `pop` is in place only
/// on the sole live token, which a held view is not.
#[test]
fn list_mutations_never_reach_a_held_view() {
    assert_canonical_and_wasm(r#"
fn set_first(var xs: List(Int)):
    xs[0] = 99

fn main() -> Int:
    var a = [1, 2, 3]
    let va = &a
    a[0] = 50
    var b = [1, 2, 3]
    let vb = &b
    let _ = b.pop()
    var c = [1, 2]
    let vc = &c
    set_first(c)
    var d = [1, 2, 3]
    let vd = &d
    let wd = vd
    d.push(4)
    d[1] = 40
    let first = (*va)[0] * 1000 + a[0]
    let second = (*vb).length() * 10 + b.length()
    let third = (*vc)[0] * 1000 + c[0]
    let fourth = (*wd)[1] * 1000 + (*vd).length() * 100 + d[1]
    first + second + third + fourth
"#, 1050 + 32 + 1099 + 2340);
}
