//! Nested constructor payloads retain their checked stored-child qualifiers.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const SHAPES: &str = "type Box(a):\n    value: a\n    count: Int\n";
const PROGRAM: &str = r#"import shapes

fn finish(own box: shapes.Box(unique String)) -> Int:
    match move box:
        shapes.Box(value, count) -> value.length() * 10 + count

fn record_value() -> shapes.Box(shapes.Box(unique String)):
    shapes.Box(shapes.Box("ab", 3), 4)

fn record_score(own box: shapes.Box(shapes.Box(unique String))) -> Int:
    match move box:
        shapes.Box(inner, count) -> finish(inner) * 10 + count

fn tuple_value() -> (shapes.Box(unique String), Int):
    (shapes.Box("x", 5), 6)

fn tuple_score(own pair: (shapes.Box(unique String), Int)) -> Int:
    match move pair:
        (inner, count) -> finish(inner) * 10 + count

fn sum_value() -> .[Some(shapes.Box(unique String)) | None]:
    .Some(shapes.Box("xyz", 7))

fn sum_score(own sum: .[Some(shapes.Box(unique String)) | None]) -> Int:
    match move sum:
        .Some(inner) -> finish(inner)
        .None -> 0

fn main() -> Int:
    record_score(record_value()) * 10000 + tuple_score(tuple_value()) * 100 + sum_score(sum_value())
"#;

fn checked(mode: &str, shapes_mode: &str, source: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("shapes".into(), format!("{shapes_mode}{SHAPES}").into_bytes()),
             ("main".into(), format!("{mode}{source}").into_bytes())],
        "main", witchy_interp::comptime::EXPANDER,
    ).map_err(|error| error.to_string())
}

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn recursive_constructor_storage_preserves_qualified_children_across_modes() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    // A normal caller of an opt-defined nominal type remains a normal caller.
    for (mode, shapes_mode) in [("", ""), ("mode opt\n", "mode opt\n"), ("", "mode opt\n")] {
        let source = checked(mode, shapes_mode, PROGRAM).expect("checked recursive constructors");
        let artifact = witchy::codegen::compile_checked_module_artifact(&source)
            .expect_lowered("unoptimized canonical admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).expect("admitted unoptimized Wasm");
        vm.run().expect("constructor execution");
        // Record: (2*10+3)*10+4=234. Tuple: (1*10+5)*10+6=156.
        // Sum: 3*10+7=37. Combine 234*10000+156*100+37=2355637.
        assert_eq!(vm.output(), vec!["2355637"]);
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}

#[test]
fn recursive_constructor_storage_does_not_manufacture_child_ownership() {
    for (mode, shapes_mode) in [("", ""), ("mode opt\n", "mode opt\n"), ("", "mode opt\n")] {
        let error = checked(mode, shapes_mode, r#"import shapes
fn duplicate(own input: shapes.Box(unique String)) -> (shapes.Box(unique String), shapes.Box(unique String)):
    (move input, move input)
fn main() -> Int:
    let pair = duplicate(shapes.Box("x", 1))
    0
"#).expect_err("a typed payload plan cannot duplicate a unique child");
        assert!(error.contains("consumed") || error.contains("moved") || error.contains("transferred"), "{error}");
    }
    let error = checked("mode opt\n", "mode opt\n", r#"import shapes
fn main() -> Int:
    let shared = "x"
    let outer: shapes.Box(shapes.Box(unique String)) = shapes.Box(shapes.Box(shared, 3), 4)
    0
"#).expect_err("an opt-defined type cannot grant unique origin to an ordinary child");
    assert!(error.contains("cannot repair ordinary storage"), "{error}");
}
