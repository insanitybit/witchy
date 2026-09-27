//! RFC-0167 sections 6 and 11: root uniqueness is not child uniqueness.
//! Each positive score is 1*1000 + 1*100 + 9*10 + 1 = 1191:
//! both saved children and the unmodified sibling must retain the old value.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const FAST: &str = r#"mode opt
type Rows:
    values: List(List(Int))

pub fn set_first(var values: unique List(Int)):
    values[0] = 9
"#;

fn checked(main: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("fast".into(), FAST.as_bytes().to_vec()),
             ("main".into(), main.as_bytes().to_vec())],
        "main",
        witchy_interp::comptime::EXPANDER,
    ).map_err(|error| error.to_string())
}

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

fn assert_execution(source: &str, expected: &str) {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    let program = checked(source).expect("checked source");
    let artifact = witchy::codegen::compile_checked_module_artifact(&program)
        .expect_lowered("unoptimized canonical SafeWIR admission");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(
        &artifact,
        Capabilities { print_int: true, quiet: true, ..Capabilities::none() },
        128,
    ).expect("admitted Wasm instance");
    vm.run().expect("admitted Wasm execution");
    assert_eq!(vm.output(), vec![expected]);
    assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
}

#[test]
fn normal_nested_mutation_preserves_snapshots_across_mode_and_type_boundaries() {
    assert_execution(r#"import fast

fn set_first(var values: List(Int)):
    values[0] = 9

fn ordinary() -> Int:
    let child = [1, 2]
    var rows = [child, child]
    let saved = rows
    set_first(rows[0])
    saved[0][0] * 1000 + saved[1][0] * 100 + rows[0][0] * 10 + rows[1][0]

fn cross_mode() -> Int:
    let child = [1, 2]
    var rows = [child, child]
    let saved = rows
    fast.set_first(rows[0])
    saved[0][0] * 1000 + saved[1][0] * 100 + rows[0][0] * 10 + rows[1][0]

fn imported_type() -> Int:
    let child = [1, 2]
    var rows = fast.Rows(values: [child, child])
    let saved = rows
    rows.values[0][0] = 9
    saved.values[0][0] * 1000 + saved.values[1][0] * 100 + rows.values[0][0] * 10 + rows.values[1][0]

fn main() -> Int:
    ordinary() * 100000000 + cross_mode() * 10000 + imported_type()
"#, "119111911191");
}

#[test]
fn opt_call_refuses_to_treat_fresh_outer_storage_as_unique_children() {
    let error = checked(r#"mode opt
import fast

fn main() -> Int:
    let child = [1, 2]
    var rows = [child, child]
    fast.set_first(rows[0])
    rows[1][0]
"#).expect_err("shared child cannot meet the opt callee's unique precondition");
    assert!(error.contains("cannot repair ordinary storage"), "{error}");
}

#[test]
fn opt_call_accepts_explicit_independent_child_preparation() {
    assert_execution(r#"mode opt
import fast

fn main() -> Int:
    let child = [1, 2]
    let rows = [child, child]
    var independent: unique List(Int) = [rows[0][0], rows[0][1]]
    fast.set_first(independent)
    rows[0][0] * 100 + rows[1][0] * 10 + independent[0]
"#, "119");

    assert_execution(r#"mode opt
import fast

fn main() -> Int:
    let child = [1, 2]
    let rows = [child, child]
    var independent = rows[0].owned()
    fast.set_first(independent)
    rows[0][0] * 100 + rows[1][0] * 10 + independent[0]
"#, "119");

    assert_execution(r#"mode opt
import fast

fn main() -> Int:
    let root = [1, 2]
    let view = &root
    var independent: unique List(Int) = view.owned()
    fast.set_first(independent)
    root[0] * 10 + independent[0]
"#, "19");
}
