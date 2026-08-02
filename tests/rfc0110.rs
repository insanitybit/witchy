//! RFC-0110 uniform ownership/access ABI conformance.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};
use witchy_syntax::opt::{self, Opt, OptSet};

const ACCESS_MATRIX: &str = r#"
mode opt

import list

trait Lists:
    fn revise(let self, var values: unique List(Int)) -> Int

type Box:
    Box(Int)

impl Lists for Box:
    fn revise(let self, var values: unique List(Int)) -> Int:
        values = [4, 5]
        match self:
            Box(base) -> base

fn revise(var values: unique List(Int), value: Int) -> unique List(Int):
    values.push(value)
    [value * 10]

fn main(console: Console):
    var direct_values = [0]
    var direct_result = revise(direct_values, 1)
    console.print("${list.length(direct_values) * 100 + list.at(direct_result, 0)}")

    let indirect = revise
    var indirect_values = [0]
    var indirect_result = indirect(indirect_values, 2)
    console.print("${list.length(indirect_values) * 100 + list.at(indirect_result, 0)}")

    let closure = fn(var values: unique List(Int), value: Int) -> unique List(Int):
        values.push(value)
        [value * 10]
    var closure_values = [0]
    var closure_result = closure(closure_values, 3)
    console.print("${list.length(closure_values) * 100 + list.at(closure_result, 0)}")

    let item: dyn Lists = Box(7)
    var trait_values = [0]
    let trait_result = item.revise(trait_values)
    console.print("${list.length(trait_values) * 100 + list.at(trait_values, 1) * 10 + trait_result}")
"#;

const EXPECTED: [&str; 4] = ["210", "220", "230", "257"];

fn compiled_output(
    checked: &witchy_types::pipeline::CheckedModule,
    optimizations: OptSet,
) -> Vec<String> {
    opt::set_for_tests(Some(optimizations));
    let bytes = codegen::compile_checked_module_binary(checked)
        .expect_lowered("compile RFC-0110 access matrix");
    opt::set_for_tests(None);

    let mut runtime = Runtime::batch().expect("runtime");
    let mut actor = runtime
        .spawn(
            &bytes,
            Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn RFC-0110 access matrix");
    actor.run().expect("run RFC-0110 access matrix");
    actor.output()
}

#[test]
fn access_matrix_matches_independent_oracle_across_every_deopt() {
    let checked = witchy::resolve_std_only_checked(ACCESS_MATRIX).expect("checked access matrix");
    witchy::enforce_performance_modes(checked.module(), "main")
        .expect("access matrix satisfies mode opt");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret RFC-0110 access matrix");
    assert_eq!(interpreted, EXPECTED, "the independent semantic oracle changed");

    let all = compiled_output(&checked, OptSet::all());
    assert_eq!(all, EXPECTED, "optimized Wasm changed the access contract");
    let none = compiled_output(&checked, OptSet::none());
    assert_eq!(none, EXPECTED, "forced de-opt Wasm changed the access contract");
    for lever in Opt::ALL {
        let actual = compiled_output(&checked, OptSet::all().without(lever));
        assert_eq!(
            actual,
            EXPECTED,
            "disabling `{}` changed the access contract",
            lever.name()
        );
    }
}
