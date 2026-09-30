//! RFC-0167 row N: normal-mode worker execution has an explicit boundary.
//! The independent result is the square of 5, 3, 8, and 1 in source order.

use std::collections::BTreeSet;
use witchy::codegen;
use witchy::opt::{self, OptSet};
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimization;
impl Drop for ResetOptimization {
    fn drop(&mut self) {
        opt::set_for_tests(None);
    }
}

#[test]
fn normal_pure_worker_map_uses_admitted_zero_grant_workers() {
    opt::set_for_tests(Some(OptSet::none()));
    let _reset = ResetOptimization;
    let source = r#"
import vm
fn square(n: Int) -> Int:
    n * n
fn main(console: Console):
    let values = vm.par_map([5, 3, 8, 1], square)
    console.print("${values}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("normal worker source checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("normal worker map admits through canonical SafeWIR");
    let mut worker_imports = BTreeSet::new();
    for payload in wasmparser::Parser::new(0).parse_all(artifact.wasm()) {
        if let wasmparser::Payload::ImportSection(section) = payload.expect("valid admitted Wasm") {
            for import in section.into_imports() {
                let import = import.expect("valid admitted Wasm import");
                if import.module == "witchy" && import.name.starts_with("vm_par_map") {
                    worker_imports.insert(import.name.to_owned());
                }
            }
        }
    }
    assert_eq!(worker_imports, BTreeSet::from([
        "vm_par_map_admitted_run".to_owned(),
        "vm_par_map_admitted_write".to_owned(),
    ]), "canonical normal-mode map has exactly the authenticated worker boundary");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                quiet: true,
                ..Capabilities::none()
            },
            128,
        )
        .expect("admitted worker spawn");
    vm.run().expect("admitted worker run");
    assert!(vm.admitted_zero_grant_worker_launches() > 0, "worker map launches a fresh zero-grant VM");
    assert_eq!(vm.output(), ["[25, 9, 64, 1]"]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}
