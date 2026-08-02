//! RFC-0110 uniform ownership/access ABI conformance.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};
use witchy_syntax::ast::{Expr, Item, Stmt};
use witchy_syntax::opt::{self, Opt, OptSet};
use witchy_types::access::{CheckedPlaceStep, checked_place_facts};
use witchy_wir::wir::WirTy;

const ACCESS_MATRIX: &str = r#"
mode opt

import list

trait Lists:
    fn revise(let self, var values: unique List(Int)) -> Int

type Box:
    Box(Int)

type State:
    value: Int
    rows: List(List(Int))

impl Lists for Box:
    fn revise(let self, var values: unique List(Int)) -> Int:
        values = [4, 5]
        match self:
            Box(base) -> base

fn revise(var values: unique List(Int), value: Int) -> unique List(Int):
    values.push(value)
    [value * 10]

fn bump(var value: Int) -> Int:
    value = value + 10
    value * 2

fn walk(var values: unique List(Int), n: Int) -> unique List(Int):
    if n == 0:
        return [7]
    values.push(n)
    walk(values, n - 1)

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

    var state = State(4, [[1, 2], [3, 4]])
    let field_result = bump(state.value)
    console.print("${state.value * 100 + field_result}")
    let index_result = bump(state.rows[0][1])
    console.print("${list.at(list.at(state.rows, 0), 1) * 100 + index_result}")

    var tail_values = []
    var tail_result = walk(tail_values, 3)
    console.print("${list.length(tail_values) * 100 + (tail_result.pop() ?? 0)}")
"#;

const EXPECTED: [&str; 7] = ["210", "220", "230", "257", "1428", "1224", "307"];

fn compiled_output(
    checked: &witchy_types::pipeline::CheckedModule,
    optimizations: OptSet,
) -> (Vec<String>, i64) {
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
    let indirect_ownership_calls = actor
        .indirect_ownership_calls()
        .expect("RFC-0110 indirect ownership counter export");
    (actor.output(), indirect_ownership_calls)
}

fn bump_arguments(checked: &witchy_types::pipeline::CheckedModule) -> Vec<&Expr> {
    checked
        .module()
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name.rsplit('.').next() == Some("main") => {
                Some(
                    function
                        .body
                        .stmts
                        .iter()
                        .filter_map(|statement| match statement {
                            Stmt::Let {
                                value: Expr::Call { name, args },
                                ..
                            } if name.rsplit('.').next() == Some("bump") => args.first(),
                            _ => None,
                        })
                        .collect(),
                )
            }
            _ => None,
        })
        .expect("linked main function")
}

fn assert_canonical_fixed_places(checked: &witchy_types::pipeline::CheckedModule) {
    let arguments = bump_arguments(checked);
    assert_eq!(arguments.len(), 2, "the place matrix must keep both bump calls");
    let places = checked_place_facts(checked.module());

    let field = places
        .place_at(checked.module(), arguments[0])
        .expect("canonical fixed field place");
    assert_eq!(field.root(), "state");
    assert_eq!(field.steps(), &[CheckedPlaceStep::Field("value".to_string())]);
    assert!(!field.has_dynamic_index());

    let index = places
        .place_at(checked.module(), arguments[1])
        .expect("canonical nested fixed-index place");
    assert_eq!(index.root(), "state");
    assert_eq!(
        index.steps(),
        &[
            CheckedPlaceStep::Field("rows".to_string()),
            CheckedPlaceStep::Index(0),
            CheckedPlaceStep::Index(1),
        ]
    );
    assert!(!index.has_dynamic_index());
}

fn assert_optimized_tail_and_adapter_envelopes(
    checked: &witchy_types::pipeline::CheckedModule,
) {
    opt::set_for_tests(Some(OptSet::all()));
    let wir = codegen::assemble_checked_optimized_wir_module(checked)
        .expect_lowered("assemble optimized RFC-0110 access matrix");
    opt::set_for_tests(None);

    let walk = wir
        .funcs
        .iter()
        .find(|function| function.name.rsplit('.').next() == Some("walk"))
        .expect("lowered walk function");
    let wat = witchy_wir::wir::to_wat(&wir);
    let start_marker = format!("  (func ${}", walk.name);
    let start = wat.find(&start_marker).expect("walk WAT body");
    let rest = &wat[start..];
    let end = rest[1..]
        .find("\n  (func $")
        .map_or(rest.len(), |offset| offset + 1);
    let walk_wat = &rest[..end];
    assert!(
        walk_wat.contains("loop $__witchy_tail_loop"),
        "the ownership envelope must lower through a proper tail loop: {walk_wat}"
    );
    assert!(
        !walk_wat.contains(&format!("call ${}", walk.name)),
        "the proper tail loop must not retain a recursive call: {walk_wat}"
    );

    let adapters: Vec<_> = wir
        .funcs
        .iter()
        .filter(|function| function.name.starts_with("__dynw"))
        .collect();
    assert_eq!(adapters.len(), 1, "the matrix has one reachable witness slot");
    let adapter = adapters[0];
    assert_eq!(
        adapter.params.iter().map(|param| param.name.as_str()).collect::<Vec<_>>(),
        ["receiver", "arg0", "arg0__cap"]
    );
    assert_eq!(
        adapter.params.iter().map(|param| &param.ty).collect::<Vec<_>>(),
        [&WirTy::StructRef, &WirTy::Bool, &WirTy::Bool]
    );
    assert_eq!(
        adapter.ret,
        [WirTy::Int, WirTy::Bool, WirTy::Bool],
        "the adapter must return the ordinary result, var value, and ownership state"
    );
}

#[test]
fn access_matrix_matches_independent_oracle_across_every_deopt() {
    let checked = witchy::resolve_std_only_checked(ACCESS_MATRIX).expect("checked access matrix");
    witchy::enforce_performance_modes(checked.module(), "main")
        .expect("access matrix satisfies mode opt");
    assert_canonical_fixed_places(&checked);
    assert_optimized_tail_and_adapter_envelopes(&checked);
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret RFC-0110 access matrix");
    assert_eq!(interpreted, EXPECTED, "the independent semantic oracle changed");

    let (all, all_indirect_calls) = compiled_output(&checked, OptSet::all());
    assert_eq!(all, EXPECTED, "optimized Wasm changed the access contract");
    assert_eq!(
        all_indirect_calls, 2,
        "the optimized matrix must retain exactly two typed ownership-table calls"
    );
    let (none, none_indirect_calls) = compiled_output(&checked, OptSet::none());
    assert_eq!(none, EXPECTED, "forced de-opt Wasm changed the access contract");
    assert!(
        none_indirect_calls >= 3,
        "the de-opt matrix must execute the function value, lambda, and trait ownership envelopes through real tables"
    );
    for lever in Opt::ALL {
        let (actual, _) = compiled_output(&checked, OptSet::all().without(lever));
        assert_eq!(
            actual,
            EXPECTED,
            "disabling `{}` changed the access contract",
            lever.name()
        );
    }
}
