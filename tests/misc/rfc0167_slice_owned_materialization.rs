//! RFC-0167 row G: `.owned()` on an authenticated slice copies only
//! its projected range into a fresh `List(T)`. The expected result below is
//! hand-computed; the admitted unoptimized Wasm result and heap audit are
//! independent of the source interpreter.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn generic_scalar_slice_owned_copies_only_the_projection() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"
fn copy[a](xs: &[a]) -> List(a):
    xs.owned()

fn main() -> Int:
    var numbers = [1, 2, 3, 4, 5]
    let view = &numbers[1..4]
    var owned = copy(view)
    numbers[2] = 30
    owned[0] = 20
    let ints = owned[0] * 1000 + view.length() * 100 + numbers[2] * 10 + owned[1]

    let floats = [1.5, 2.5, 3.5, 4.5]
    let f = copy(&floats[1..3])
    let float_part = if f[0] == 2.5 && f[1] == 3.5: 100000 else: 0

    let flags = [false, true, false, true]
    let b = copy(&flags[1..3])
    let bool_part = if b[0] == true && b[1] == false: 1000000 else: 0

    let waits = [1s, 2s, 3s, 4s]
    let d = copy(&waits[1..3])
    let duration_part = if d[0] == 2s && d[1] == 3s: 10000000 else: 0
    ints + float_part + bool_part + duration_part
"#;
    // 20_000 + 300 + 300 + 3 + 100_000 + 1_000_000 + 10_000_000.
    let expected = 11_120_603;
    let checked = witchy::resolve_std_only_checked(source).expect("scalar slice materialization checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admits the exact scalar projection copy");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn string_slice_owned_retains_each_copied_element_and_the_original_view() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"
fn copy[a](xs: &[a]) -> List(a):
    xs.owned()

fn main() -> Int:
    var words = ["aa", "bbb", "cccc", "ddddd"]
    let view = &words[1..3]
    var owned = copy(view)
    words[1] = "Z"
    owned[0] = "YY"
    view[0].length() * 1000 + owned[0].length() * 100 + words[1].length() * 10 + owned[1].length()
"#;
    // The view still reads "bbb" (3), the new list reads "YY" (2) and
    // "cccc" (4), and the changed root reads "Z" (1): 3000+200+10+4.
    let expected = 3214;
    let checked = witchy::resolve_std_only_checked(source).expect("String slice materialization checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admits a pointer-bearing projection copy");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}

#[test]
fn nested_int_list_slice_materializes_and_unsupported_elements_fail_closed() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let source = r#"
fn copy[a](xs: &[a]) -> List(a):
    xs.owned()

fn main() -> Int:
    var nested = [[1, 2], [3, 4], [5, 6]]
    let view = &nested[1..3]
    var copied = copy(view)
    nested[1][0] = 30
    copied[0][1] = 40
    view[0][0] * 1000 + copied[0][1] * 100 + nested[1][0] * 10 + copied[1][1]
"#;
    // Old view = 3, materialized element = 40, current owner = 30, final = 6.
    let expected = 7_306;
    let checked = witchy::resolve_std_only_checked(source).expect("nested-list slice materialization checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admits the exact List(Int) projection copy");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected.to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));

    let scalar_lists = r#"
fn copy[a](xs: &[a]) -> List(a):
    xs.owned()

fn main() -> Int:
    let floats = [[1.5], [2.5, 3.5]]
    let f = copy(&floats[1..2])
    let flags = [[false], [true, false]]
    let b = copy(&flags[1..2])
    let waits = [[1s], [2s, 3s]]
    let d = copy(&waits[1..2])
    let float_part = if f[0][1] == 3.5: 1 else: 0
    let bool_part = if b[0][0] && !b[0][1]: 10 else: 0
    let duration_part = if d[0][1] == 3s: 100 else: 0
    float_part + bool_part + duration_part
"#;
    // Projected second rows contribute 1 + 10 + 100.
    let checked = witchy::resolve_std_only_checked(scalar_lists).expect("scalar-list slice materialization checks");
    let (artifact, ()) = codegen::compile_checked_module_artifact_with_observer(&checked, |_, _| ())
        .expect_lowered("canonical SafeWIR admits the three exact scalar-list projection copies");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
            128,
        )
        .expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["111".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));

    let other_list_source = r#"
fn copy[a](xs: &[a]) -> List(a):
    xs.owned()

fn main() -> Int:
    let nested = [["a"], ["b"]]
    let copied = copy(&nested[0..1])
    copied.length()
"#;
    let error = witchy::resolve_std_only_checked(other_list_source)
        .err().expect("List(String) elements remain outside the materialization boundary")
        .to_string();
    assert!(error.contains("List(String)") && error.contains("exact List(S)"), "{error}");

    let must_source = r#"
must type Ticket:
    Ticket(Int)

fn main() -> Int:
    let tickets = [Ticket(1)]
    let copied = (&tickets[0..1]).owned()
    copied.length()
"#;
    let error = witchy::resolve_std_only_checked(must_source)
        .err().expect("must element cannot become a copyable slice")
        .to_string();
    assert!(error.contains("Ticket") && error.contains("exact List(S)"), "{error}");
}
