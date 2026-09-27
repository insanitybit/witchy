//! Imported function values retain the same stored parameter types as calls.

use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

const WORKER: &str = r#"type Box(a):
    value: a
    count: Int
impl Box(a):
    pub fn get(self: Box(a)) -> a:
        self.value
type Parcel:
    value: unique String
    count: Int
type Counter:
    value: Int
impl Counter:
    pub fn measure(self: Counter) -> Int:
        self.value
pub fn direct(own box: Box(unique String)) -> Int:
    match move box:
        Box(value, count) -> value.length() * 10 + count
pub fn nested(own pair: (Box(unique String), Int)) -> Int:
    match move pair:
        (inner, count) -> direct(inner) * 10 + count
pub fn hidden(own box: Parcel) -> Int:
    match move box:
        Parcel(value, count) -> value.length() * 10 + count
pub fn identity(value: a) -> a:
    value
"#;
const MAIN: &str = r#"import worker
fn main() -> Int:
    let direct = worker.direct
    let nested = worker.nested
    let hidden = worker.hidden
    let measure = worker.measure
    let identity_int = worker.identity
    let identity_text = worker.identity
    let get_int = worker.get
    let get_text = worker.get
    let a = direct(worker.Box("abc", 4))
    let b = nested((worker.Box("xy", 5), 6))
    let c = hidden(worker.Parcel("q", 8))
    let d = measure(worker.Counter(9))
    let e = identity_int(11) + identity_text("xyz").length()
    let got: String = get_text(worker.Box("z", 0))
    let f = get_int(worker.Box(5, 0)) + got.length()
    a * 10000 + b * 100 + c * 10 + d + e + f
"#;

fn checked(main_mode: &str, worker_mode: &str, main: &str) -> Result<witchy_types::pipeline::CheckedModule, String> {
    witchy_types::pipeline::link_checked_from_source_bundle(
        vec![("worker".into(), format!("{worker_mode}{WORKER}").into_bytes()),
             ("main".into(), format!("{main_mode}{main}").into_bytes())],
        "main", witchy_interp::comptime::EXPANDER,
    ).map_err(|error| error.to_string())
}

const MODES: [(&str, &str); 3] = [("", ""), ("mode opt\n", "mode opt\n"), ("", "mode opt\n")];

struct ResetOptimizationOverride;
impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) { opt::set_for_tests(None); }
}

#[test]
fn eta_storage_signature_keeps_nested_and_nominal_contracts() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for (main_mode, worker_mode) in MODES {
        let source = checked(main_mode, worker_mode, MAIN).expect("checked function values");
        let artifact = witchy::codegen::compile_checked_module_artifact(&source)
            .expect_lowered("unoptimized eta storage admission");
        let mut runtime = Runtime::batch().expect("runtime");
        let mut vm = runtime.spawn_admitted(&artifact,
            Capabilities { print_int: true, quiet: true, ..Capabilities::none() }, 128,
        ).expect("admitted unoptimized Wasm");
        vm.run().expect("function value execution");
        // direct=34; nested=(2*10+5)*10+6=256; hidden=18;
        // method=9; generic functions=11+3=14; generic methods=5+1=6.
        assert_eq!(vm.output(), vec!["365809"]);
        assert_eq!(vm.canonical_heap_leaks().expect("ownership audit"), Some(0));
    }
}

#[test]
fn eta_storage_signature_does_not_allow_duplicate_affine_inputs() {
    for (main_mode, worker_mode) in MODES {
        let error = checked(main_mode, worker_mode, r#"import worker
fn duplicate(own box: worker.Box(unique String)) -> Int:
    let call = worker.direct
    call(move box) + call(move box)
fn main() -> Int:
    duplicate(worker.Box("x", 1))
"#).expect_err("eta storage facts do not duplicate ownership");
        assert!(error.contains("consumed") || error.contains("moved") || error.contains("transferred"), "{error}");
    }
}
