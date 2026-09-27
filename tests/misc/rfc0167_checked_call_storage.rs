//! Call arguments use exact checked storage contracts without granting origin.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const SHAPES: &str = "type Box(a):\n    value: a\n    count: Int\n";
const WORKER: &str = r#"import shapes
pub fn direct(own box: shapes.Box(unique String)) -> Int:
    match move box:
        shapes.Box(value, count) -> value.length() * 10 + count
pub fn nested(own box: shapes.Box(shapes.Box(unique String))) -> Int:
    match move box:
        shapes.Box(inner, count) -> direct(inner) * 10 + count
pub fn tuple(own pair: (shapes.Box(unique String), Int)) -> Int:
    match move pair:
        (inner, count) -> direct(inner) * 10 + count
pub fn root(own text: unique String) -> Int:
    text.length()
"#;
const MAIN: &str = r#"import shapes
import worker
fn dynamic(prefix: String) -> String:
    prefix + "bc"
fn main() -> Int:
    let a = worker.direct(shapes.Box("abc", 4))
    let b = worker.nested(shapes.Box(shapes.Box("xy", 5), 6))
    let call = fn(own box: shapes.Box(unique String)): worker.direct(box)
    let c = call(shapes.Box("q", 7))
    let d = worker.tuple((shapes.Box("k", 8), 9))
    let text = dynamic("a")
    let alias = text
    let second = text
    let n = worker.root(PREPARED)
    let kept = alias.length() * 100 + second.length() * 10 + n
    a * 1000000 + b * 1000 + c * 10 + d + kept
"#;

fn checked(main_mode: &str, worker_mode: &str, shapes_mode: &str, main: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("shapes".into(), format!("{shapes_mode}{SHAPES}").into_bytes()),
             ("worker".into(), format!("{worker_mode}{WORKER}").into_bytes()),
             ("main".into(), format!("{main_mode}{main}").into_bytes())],
        "main", witchy_interp::comptime::EXPANDER,
    ).map_err(|error| error.to_string())
}

const MODES: [(&str, &str, &str); 4] = [
    ("", "", ""),
    ("mode opt\n", "mode opt\n", "mode opt\n"),
    ("", "mode opt\n", "mode opt\n"),
    ("", "", "mode opt\n"),
];

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn checked_call_storage_preserves_nested_fields_and_normal_preparation() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for (main_mode, worker_mode, shapes_mode) in MODES {
        let input = if main_mode.is_empty() { "text" } else { "text.owned()" };
        let source = checked(main_mode, worker_mode, shapes_mode, &MAIN.replace("PREPARED", input))
            .expect("checked call storage");
        let artifact = witchy::codegen::compile_checked_module_artifact(&source)
            .expect_lowered("unoptimized canonical admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).expect("admitted unoptimized Wasm");
        vm.run().expect("call storage execution");
        // a=3*10+4=34; b=(2*10+5)*10+6=256; c=1*10+7=17;
        // d=(1*10+8)*10+9=189. Both saved strings stay length3: kept=333.
        assert_eq!(vm.output(), vec!["34256692"]);
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}

#[test]
fn checked_call_storage_preserves_source_ownership_refusals() {
    for (main_mode, worker_mode, shapes_mode) in MODES {
        let error = checked(main_mode, worker_mode, shapes_mode, r#"import shapes
import worker
fn duplicate(own box: shapes.Box(unique String)) -> Int:
    worker.direct(move box) + worker.direct(move box)
fn main() -> Int:
    duplicate(shapes.Box("x", 1))
"#).expect_err("call ascription cannot duplicate an affine argument");
        assert!(error.contains("consumed") || error.contains("moved") || error.contains("transferred"), "{error}");
    }
    let error = checked("mode opt\n", "mode opt\n", "mode opt\n", &MAIN.replace("PREPARED", "text"))
        .expect_err("opt caller must explicitly prepare an ordinary shared argument");
    assert!(error.contains("cannot repair ordinary storage"), "{error}");
}
