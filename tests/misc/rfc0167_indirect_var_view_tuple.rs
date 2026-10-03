//! RFC-0167 row H: indirect `var` write-back of a tuple storing views.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn indirect_var_tuple_write_back_keeps_the_old_graph_root() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn replace(var pair: (List(&Int), Int), next: &Int):
    pair = ([next], 9)

fn read(view: &(List(&Int), Int)) -> Int:
    let pair = *view
    *(pair.0[0]) * 10 + pair.1

fn main() -> Int:
    let a = 2
    let b = 7
    var pair: (List(&Int), Int) = ([&a], 3)
    let old = &pair
    let change = replace
    change(pair, &b)
    read(old) * 100 + read(&pair)
"#).expect("indirect view-tuple write-back checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("indirect view-tuple write-back admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["2379".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn generic_var_tuple_write_back_keeps_the_old_graph_root() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn replace[a](var pair: (List(&a), Int), next: &a):
    pair = ([next], 9)

fn read(view: &(List(&Int), Int)) -> Int:
    let pair = *view
    *(pair.0[0]) * 10 + pair.1

fn main() -> Int:
    let a = 2
    let b = 7
    var pair: (List(&Int), Int) = ([&a], 3)
    let old = &pair
    replace(pair, &b)
    read(old) * 100 + read(&pair)
"#).expect("generic view-tuple write-back checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("generic view-tuple write-back admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["2379".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn projected_var_record_write_back_keeps_the_old_graph_root() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
type Pair:
    values: List(&Int)
    tag: Int

fn replace(var pair: Pair, next: &Int):
    pair.values = [next]
    pair.tag = 9

fn read(view: &Pair) -> Int:
    let pair = *view
    *(pair.values[0]) * 10 + pair.tag

fn main() -> Int:
    let a = 2
    let b = 7
    var pair: Pair = Pair([&a], 3)
    let old = &pair
    let change = replace
    change(pair, &b)
    read(old) * 100 + read(&pair)
"#).expect("projected view-record write-back checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("projected view-record write-back admits");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["2379".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn indirect_var_tuple_cannot_write_back_a_region_born_view() {
    let error = witchy::resolve_std_only_checked(r#"
fn replace(var pair: (List(&Int), Int), next: &Int):
    pair = ([next], 9)

fn main() -> Int:
    let a = 2
    var pair: (List(&Int), Int) = ([&a], 3)
    let change = replace
    let _ = region -> ():
        let local = 7
        change(pair, &local)
    0
"#).expect_err("region-born view cannot escape through tuple write-back");
    assert!(error.to_string().contains("cannot write back into `pair` inside `region:`"), "{error}");
}

#[test]
fn projected_tuple_fields_write_back_and_keep_the_old_view() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn replace(var pair: (List(&Int), Int), next: &Int):
    pair.0 = [next]
    pair.1 = 9

fn read(view: &(List(&Int), Int)) -> Int:
    let pair = *view
    *(pair.0[0]) * 10 + pair.1

fn main() -> Int:
    let a = 2
    let b = 7
    var pair: (List(&Int), Int) = ([&a], 3)
    let old = &pair
    let change = replace
    change(pair, &b)
    read(old) * 100 + read(&pair)
"#).expect("tuple field assignments check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("tuple field assignments admit");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["2379".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn tuple_field_assignment_rejects_missing_field() {
    let error = witchy::resolve_std_only_checked(r#"
fn main() -> Int:
    var pair = (1, 2)
    pair.2 = 3
    pair.0
"#).expect_err("tuple field index must be checked");
    assert!(error.to_string().contains("cannot verify deep assignment place"), "{error}");
}

#[test]
fn tuple_field_assignment_cannot_discard_a_must_value() {
    let error = witchy::resolve_std_only_checked(r#"
must type Ticket:
    Ticket(Int)

fn main() -> Int:
    var pair: (Ticket, Int) = (Ticket(1), 2)
    pair.0 = Ticket(3)
    0
"#).expect_err("tuple field replacement cannot discard a must obligation");
    assert!(error.to_string().contains("assignment would overwrite live must-consume value `pair`"), "{error}");
}

#[test]
fn tuple_field_assignment_cannot_write_through_an_unproved_frozen_child() {
    let error = witchy::resolve_std_only_checked(r#"
type Box:
    value: Int

fn main() -> Int:
    var pair: (frozen Box, Int) = (Box(1), 2)
    pair.0.value = 3
    pair.1
"#).expect_err("deep tuple field mutation needs source qualifier proof");
    assert!(error.to_string().contains("cannot verify deep assignment place"), "{error}");
}

#[test]
fn scalar_tuple_field_assignment_and_compound_update_use_var() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let checked = witchy::resolve_std_only_checked(r#"
fn main() -> Int:
    var pair = (2, 3)
    pair.0 += 4
    pair.1 = 9
    pair.0 * 10 + pair.1
"#).expect("scalar tuple field assignments check");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("scalar tuple field assignments admit");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec!["69".to_string()]);
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}
