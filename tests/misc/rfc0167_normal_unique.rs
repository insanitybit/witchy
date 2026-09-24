//! RFC-0167 normal-mode `unique` ownership matrix.
//!
//! These fixtures deliberately exercise the ordinary (copying) mode. A
//! `unique` parameter is a callee-entry ownership proof. Normal callers may
//! repair a call by copying, while a retained alias remains a value snapshot
//! and must not observe the write-back.

use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

fn assert_both_backends(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("fixture checks");
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("compile fixture");
    let entry = artifact.entry().expect("canonical entry");
    let function = FunctionId::new(
        entry
            .export()
            .strip_prefix('f')
            .expect("numeric entry export")
            .parse()
            .expect("numeric entry function"),
    );
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("canonical SafeWIR validation");
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(InterpreterValue::entry_capability)
        .collect();
    let oracle = interpret_program(&validated, function, arguments, Default::default())
        .expect("canonical interpreter executes fixture");
    assert_eq!(oracle.output, expected, "canonical interpreter output");
    assert_eq!(oracle.leaked_objects, 0, "canonical interpreter heap audit");

    let mut runtime = Runtime::batch().expect("create runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .expect("spawn fixture");
    vm.run().expect("run fixture");
    assert_eq!(vm.output(), expected, "compiled output");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn direct_normal_unique_list_root_preserves_retained_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_both_backends(
        r#"
fn update(var xs: unique List(Int)):
    xs[0] = 9

fn main(console: Console):
    var xs = [1, 2]
    let snapshot = xs
    update(xs)
    console.print("${snapshot[0]}:${xs[0]}")
"#,
        &["1:9"],
    );
}

#[test]
fn direct_normal_unique_string_root_preserves_retained_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_both_backends(
        r#"
fn update(var text: unique String):
    text = text + "!"

fn main(console: Console):
    var text = "old"
    let snapshot = text
    update(text)
    console.print("${snapshot}:${text}")
"#,
        &["old:old!"],
    );
}

#[test]
fn direct_normal_unique_record_root_preserves_retained_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_both_backends(
        r#"
type Box:
    values: List(Int)

fn update(var box: unique Box):
    box.values = [8, 2]

fn main(console: Console):
    var box = Box([1, 2])
    let snapshot = box
    update(box)
    console.print("${snapshot.values[0]}:${box.values[0]}")
"#,
        &["1:8"],
    );
}

#[test]
fn direct_normal_unique_nested_place_preserves_retained_snapshot() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_both_backends(
        r#"
type Outer:
    inner: Box

type Box:
    values: List(Int)

fn update(var xs: unique List(Int)):
    xs[0] = 7

fn main(console: Console):
    var outer = Outer(Box([1, 2]))
    let snapshot = outer
    update(outer.inner.values)
    console.print("${snapshot.inner.values[0]}:${outer.inner.values[0]}")
"#,
        &["1:7"],
    );
}

#[test]
fn direct_normal_unique_nested_unique_place_without_alias_updates_owner() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_both_backends(
        r#"
type Box:
    values: unique List(Int)

fn update(var xs: unique List(Int)):
    xs[0] = 7

fn main(console: Console):
    var box = Box([1, 2])
    update(box.values)
    console.print("${box.values[0]}")
"#,
        &["7"],
    );
}

#[test]
fn mode_opt_rejects_unique_call_with_retained_alias() {
    let source = r#"
mode opt

fn take(var xs: unique List(Int)):
    return

fn main(console: Console):
    var xs = [1, 2]
    let operation = take
    let snapshot = xs
    operation(xs)
    let _ = snapshot
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("mode opt must reject the indirect aliased unique call");
    let message = error.to_string();
    assert!(message.contains("mode opt") || message.contains("unique"), "{message}");
}

#[test]
fn normal_unique_call_copies_before_a_live_element_view() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_both_backends(
        r#"
fn update(var xs: unique List(Int)):
    xs[0] = 9

fn main(console: Console):
    var xs: unique List(Int) = [1, 2]
    let old = &xs[0]
    update(xs)
    console.print("${*old}:${xs[0]}")
"#,
        // The view holds the pre-call root [1, 2]; write-back replaces xs.
        &["1:9"],
    );
}

#[test]
fn mode_opt_refuses_hidden_copy_for_live_element_view_at_unique_call() {
    let source = r#"
mode opt

fn update(var xs: unique List(Int)):
    xs[0] = 9

fn main(console: Console):
    var xs: unique List(Int) = [1, 2]
    let old = &xs[0]
    update(xs)
    console.print("${*old}:${xs[0]}")
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("opt must refuse a hidden copy before the unique call");
    let message = error.to_string();
    assert!(message.contains("unique") || message.contains("mode opt"), "{message}");
}
