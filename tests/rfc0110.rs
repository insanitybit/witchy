//! RFC-0110 uniform ownership/access ABI conformance.

use witchy::runtime::{Capabilities, Runtime};
use witchy::{codegen, interpreter};
use witchy_syntax::ast::{Expr, Item, Module, Stmt};
use witchy_syntax::opt::{self, Opt, OptSet};
use witchy_types::access::{
    AccessKind, AccessQualifier, AccessSignature, CheckedPlaceStep, OwnershipStateClass,
    checked_facts, checked_place_facts,
};
use witchy_wir::wir::{ClosureSignature, Kind, WirExpr, WirFunc, WirNode, WirTy};

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
            Capabilities {
                print: true,
                quiet: true,
                ..Default::default()
            },
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

const ACCESS_CONSUMER_MATRIX: &str = r#"
mode opt

import list

trait Lists:
    fn revise(let self, var values: unique List(Int)) -> unique List(Int)

type Box:
    Box(Int)

impl Box:
    fn touch(let self, var values: unique List(Int)) -> unique List(Int):
        values.push(1)
        [1]

impl Lists for Box:
    fn revise(let self, var values: unique List(Int)) -> unique List(Int):
        values.push(2)
        [2]

fn direct_access(var values: unique List(Int), value: Int) -> unique List(Int):
    values.push(value)
    [value]

fn identity(operation: fn(var unique List(Int), Int) -> unique List(Int)) -> fn(var unique List(Int), Int) -> unique List(Int):
    operation

fn self_tail(var values: unique List(Int), n: Int) -> unique List(Int):
    if n == 0:
        return [10]
    values.push(n)
    self_tail(values, n - 1)

fn mutual_left(var values: unique List(Int), n: Int) -> unique List(Int):
    if n == 0:
        return [11]
    values.push(n)
    mutual_right(values, n - 1)

fn mutual_right(var values: unique List(Int), n: Int) -> unique List(Int):
    if n == 0:
        return [12]
    values.push(n)
    mutual_left(values, n - 1)

fn oracle(value: Int) -> String:
    match value:
        1 -> "1"
        2 -> "2"
        3 -> "3"
        4 -> "4"
        10 -> "10"
        12 -> "12"
        _ -> "unexpected"

fn main(console: Console):
    var direct_values = []
    let direct_result = direct_access(direct_values, 1)

    var method_values = []
    let method_result = Box(1).touch(method_values)

    let function_value = direct_access
    var function_value_values = []
    let function_value_result = function_value(function_value_values, 2)

    let closure = fn(var values: unique List(Int), value: Int) -> unique List(Int):
        values.push(value)
        [value]
    var lambda_values = []
    let lambda_result = closure(lambda_values, 3)

    var apply_values = []
    let apply_result = identity(direct_access)(apply_values, 4)

    var static_trait_values = []
    let static_trait_result = Box(5).revise(static_trait_values)

    let item: dyn Lists = Box(6)
    var existential_values = []
    let existential_result = item.revise(existential_values)

    var self_values = []
    let self_result = self_tail(self_values, 1)
    var mutual_values = []
    let mutual_result = mutual_left(mutual_values, 1)

    console.print(oracle(list.at(direct_result, 0)))
    console.print(oracle(list.length(direct_values)))
    console.print(oracle(list.at(direct_values, 0)))
    console.print(oracle(list.at(method_result, 0)))
    console.print(oracle(list.length(method_values)))
    console.print(oracle(list.at(method_values, 0)))
    console.print(oracle(list.at(function_value_result, 0)))
    console.print(oracle(list.length(function_value_values)))
    console.print(oracle(list.at(function_value_values, 0)))
    console.print(oracle(list.at(lambda_result, 0)))
    console.print(oracle(list.length(lambda_values)))
    console.print(oracle(list.at(lambda_values, 0)))
    console.print(oracle(list.at(apply_result, 0)))
    console.print(oracle(list.length(apply_values)))
    console.print(oracle(list.at(apply_values, 0)))
    console.print(oracle(list.at(static_trait_result, 0)))
    console.print(oracle(list.length(static_trait_values)))
    console.print(oracle(list.at(static_trait_values, 0)))
    console.print(oracle(list.at(existential_result, 0)))
    console.print(oracle(list.length(existential_values)))
    console.print(oracle(list.at(existential_values, 0)))
    console.print(oracle(list.at(self_result, 0)))
    console.print(oracle(list.length(self_values)))
    console.print(oracle(list.at(self_values, 0)))
    console.print(oracle(list.at(mutual_result, 0)))
    console.print(oracle(list.length(mutual_values)))
    console.print(oracle(list.at(mutual_values, 0)))
"#;

const ACCESS_CONSUMER_EXPECTED: [&str; 27] = [
    "1", "1", "1", "1", "1", "1", "2", "1", "2", "3", "1", "3", "4", "1", "4",
    "2", "1", "2", "2", "1", "2", "10", "1", "1", "12", "1", "1",
];

const ACCESS_DIAGNOSTIC_MATRIX: &str = r#"
mode opt

import list

trait Lists:
    fn revise(let self, var values: unique List(Int)) -> unique List(Int)

type Box:
    Box(Int)

impl Box:
    fn touch(let self, var values: unique List(Int)) -> unique List(Int):
        values.push(1)
        [1]

impl Lists for Box:
    fn revise(let self, var values: unique List(Int)) -> unique List(Int):
        values.push(2)
        [2]

fn direct_access(var values: unique List(Int), value: Int) -> unique List(Int):
    values.push(value)
    [value]

fn identity(operation: fn(var unique List(Int), Int) -> unique List(Int)) -> fn(var unique List(Int), Int) -> unique List(Int):
    operation

fn diagnostic_direct() -> Nil:
    var values = []
    let alias = values
    let _ = direct_access(values, 1)
    let _ = alias
    return

fn diagnostic_method() -> Nil:
    var values = []
    let alias = values
    let _ = Box(1).touch(values)
    let _ = alias
    return

fn diagnostic_function_value() -> Nil:
    let operation = direct_access
    var values = []
    let alias = values
    let _ = operation(values, 1)
    let _ = alias
    return

fn diagnostic_lambda() -> Nil:
    let operation = fn(var values: unique List(Int), value: Int) -> unique List(Int): [value]
    var values = []
    let alias = values
    let _ = operation(values, 1)
    let _ = alias
    return

fn diagnostic_apply() -> Nil:
    var values = []
    let alias = values
    let _ = identity(direct_access)(values, 1)
    let _ = alias
    return

fn diagnostic_trait() -> Nil:
    var values = []
    let alias = values
    let _ = Box(1).revise(values)
    let _ = alias
    return

fn diagnostic_existential() -> Nil:
    let item: dyn Lists = Box(1)
    var values = []
    let alias = values
    let _ = item.revise(values)
    let _ = alias
    return

fn main() -> Int:
    0
"#;

#[derive(Debug, PartialEq, Eq)]
struct LogicalAccessEnvelope {
    callable_qualifiers: Vec<AccessQualifier>,
    kinds: Vec<AccessKind>,
    qualifiers: Vec<Vec<AccessQualifier>>,
    ownership_inputs: Vec<Option<OwnershipStateClass>>,
    writebacks: Vec<Option<OwnershipStateClass>>,
    result_qualifiers: Vec<AccessQualifier>,
    result_ownership: Option<OwnershipStateClass>,
    borrow_owners: Vec<Vec<usize>>,
}

fn logical_access_envelope(signature: &AccessSignature) -> LogicalAccessEnvelope {
    LogicalAccessEnvelope {
        callable_qualifiers: signature.callable_qualifiers().to_vec(),
        kinds: signature.params().iter().map(|parameter| parameter.kind()).collect(),
        qualifiers: signature
            .params()
            .iter()
            .map(|parameter| parameter.qualifiers().to_vec())
            .collect(),
        ownership_inputs: signature
            .params()
            .iter()
            .map(|parameter| parameter.ownership().input().cloned())
            .collect(),
        writebacks: signature
            .params()
            .iter()
            .map(|parameter| parameter.ownership().writeback().cloned())
            .collect(),
        result_qualifiers: signature.result().qualifiers().to_vec(),
        result_ownership: signature.result().ownership_output().cloned(),
        borrow_owners: signature
            .borrow_relations()
            .iter()
            .map(|relation| relation.owners().iter().map(|owner| owner.position()).collect())
            .collect(),
    }
}

fn direct_logical_envelope() -> LogicalAccessEnvelope {
    let list_state = OwnershipStateClass::LayoutDependent { children: vec![None] };
    LogicalAccessEnvelope {
        callable_qualifiers: Vec::new(),
        kinds: vec![AccessKind::ExclusiveWriteback, AccessKind::OwnedImmutable],
        qualifiers: vec![vec![AccessQualifier::Unique], Vec::new()],
        ownership_inputs: vec![Some(list_state.clone()), None],
        writebacks: vec![Some(list_state.clone()), None],
        result_qualifiers: vec![AccessQualifier::Unique],
        result_ownership: Some(list_state),
        borrow_owners: Vec::new(),
    }
}

fn receiver_logical_envelope() -> LogicalAccessEnvelope {
    let list_state = OwnershipStateClass::LayoutDependent { children: vec![None] };
    LogicalAccessEnvelope {
        callable_qualifiers: Vec::new(),
        kinds: vec![AccessKind::SharedBorrow, AccessKind::ExclusiveWriteback],
        qualifiers: vec![Vec::new(), vec![AccessQualifier::Unique]],
        ownership_inputs: vec![None, Some(list_state.clone())],
        writebacks: vec![None, Some(list_state.clone())],
        result_qualifiers: vec![AccessQualifier::Unique],
        result_ownership: Some(list_state),
        borrow_owners: Vec::new(),
    }
}

fn function<'a>(module: &'a Module, source_name: &str) -> &'a witchy_syntax::ast::Function {
    module
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function)
                if function.name.rsplit('.').next() == Some(source_name) =>
            {
                Some(function)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing checked function `{source_name}`"))
}

fn let_value<'a>(module: &'a Module, binding: &str) -> &'a Expr {
    function(module, "main")
        .body
        .stmts
        .iter()
        .find_map(|statement| match statement {
            Stmt::Let { name, value, .. } if name == binding => Some(value),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing checked binding `{binding}`"))
}

fn resolved_call_name(expression: &Expr) -> &str {
    match expression {
        Expr::Call { name, .. } => name,
        other => panic!("method or trait call was not statically resolved: {other:?}"),
    }
}

fn assert_wir_signature(
    function: &WirFunc,
    expected_params: &[(&str, WirTy)],
    expected_results: &[WirTy],
) {
    let actual_params = function
        .params
        .iter()
        .map(|parameter| (parameter.name.as_str(), parameter.ty.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        actual_params.as_slice(),
        expected_params,
        "physical parameter order changed for `{}`",
        function.name,
    );
    assert_eq!(
        function.ret.as_slice(),
        expected_results,
        "physical result order changed for `{}`",
        function.name,
    );
}

fn visit_wir_nodes<'a>(nodes: &'a [WirNode], visit: &mut impl FnMut(&'a WirNode)) {
    for node in nodes {
        visit(node);
        match node {
            WirNode::SetLocal { value, .. } | WirNode::SetGlobal { value, .. } => {
                visit_wir_expr(value, visit);
            }
            WirNode::Store { ptr, value, .. } | WirNode::Store8 { ptr, value, .. } => {
                visit_wir_expr(ptr, visit);
                visit_wir_expr(value, visit);
            }
            WirNode::CallStoreMulti { args, .. } => {
                for argument in args {
                    visit_wir_expr(argument, visit);
                }
            }
            WirNode::CallIndirectStoreMulti { args, index, .. } => {
                for argument in args {
                    visit_wir_expr(argument, visit);
                }
                visit_wir_expr(index, visit);
            }
            WirNode::MemoryCopy { dest, src, len } => {
                visit_wir_expr(dest, visit);
                visit_wir_expr(src, visit);
                visit_wir_expr(len, visit);
            }
            WirNode::MemoryFill { dest, value, len } => {
                visit_wir_expr(dest, visit);
                visit_wir_expr(value, visit);
                visit_wir_expr(len, visit);
            }
            WirNode::If { cond, then_, els, .. } => {
                visit_wir_expr(cond, visit);
                visit_wir_nodes(then_, visit);
                visit_wir_nodes(els, visit);
            }
            WirNode::Block { body, .. } | WirNode::Loop { body, .. } => {
                visit_wir_nodes(body, visit);
            }
            WirNode::Br { cond, .. } => {
                if let Some(condition) = cond {
                    visit_wir_expr(condition, visit);
                }
            }
            WirNode::Drop(value) | WirNode::Do(value) | WirNode::Push(value) => {
                visit_wir_expr(value, visit);
            }
            WirNode::Return(Some(value)) => visit_wir_expr(value, visit),
            WirNode::StructSet { base, value, .. } => {
                visit_wir_expr(base, visit);
                visit_wir_expr(value, visit);
            }
            WirNode::ArraySet { array, index, value, .. } => {
                visit_wir_expr(array, visit);
                visit_wir_expr(index, visit);
                visit_wir_expr(value, visit);
            }
            WirNode::Return(None) | WirNode::Unreachable => {}
        }
    }
}

fn visit_wir_expr<'a>(expression: &'a WirExpr, visit: &mut impl FnMut(&'a WirNode)) {
    match expression {
        WirExpr::ToSlot(value, _)
        | WirExpr::FromSlot(value, _)
        | WirExpr::MemoryGrow(value)
        | WirExpr::ArrayLen(value)
        | WirExpr::RefIsNull(value) => visit_wir_expr(value, visit),
        WirExpr::Binary { lhs, rhs, .. } => {
            visit_wir_expr(lhs, visit);
            visit_wir_expr(rhs, visit);
        }
        WirExpr::Unary { arg, .. } | WirExpr::Convert { arg, .. } => {
            visit_wir_expr(arg, visit);
        }
        WirExpr::Load { ptr, .. } | WirExpr::Load8U { ptr, .. } => {
            visit_wir_expr(ptr, visit);
        }
        WirExpr::Call { args, .. } | WirExpr::CallHost { args, .. } => {
            for argument in args {
                visit_wir_expr(argument, visit);
            }
        }
        WirExpr::CallIndirect { args, index, .. } => {
            for argument in args {
                visit_wir_expr(argument, visit);
            }
            visit_wir_expr(index, visit);
        }
        WirExpr::Control(node) => visit_wir_nodes(std::slice::from_ref(node.as_ref()), visit),
        WirExpr::Seq(nodes) => visit_wir_nodes(nodes, visit),
        WirExpr::StructNew { args, .. } => {
            for argument in args {
                visit_wir_expr(argument, visit);
            }
        }
        WirExpr::StructGet { base, .. } | WirExpr::RefCast { value: base, .. } => {
            visit_wir_expr(base, visit);
        }
        WirExpr::ArrayNew { value, len, .. } => {
            visit_wir_expr(value, visit);
            visit_wir_expr(len, visit);
        }
        WirExpr::ArrayNewFixed { items, .. } => {
            for item in items {
                visit_wir_expr(item, visit);
            }
        }
        WirExpr::ArrayGet { array, index, .. } => {
            visit_wir_expr(array, visit);
            visit_wir_expr(index, visit);
        }
        WirExpr::ConstI64(_)
        | WirExpr::ConstF64(_)
        | WirExpr::ConstI32(_)
        | WirExpr::StrPtr(_)
        | WirExpr::GetLocal(_)
        | WirExpr::GetGlobal(_)
        | WirExpr::MemorySize
        | WirExpr::RefNull(_) => {}
    }
}

fn assert_multi_call_shape(function: &WirFunc, target: &str, args: usize, dests: usize) {
    let mut matched = 0;
    visit_wir_nodes(&function.body, &mut |node| {
        if let WirNode::CallStoreMulti {
            func,
            args: actual_args,
            dests: actual_dests,
        } = node
            && func == target
        {
            matched += 1;
            assert_eq!(actual_args.len(), args, "argument ABI for `{target}`");
            assert_eq!(actual_dests.len(), dests, "result ABI for `{target}`");
        }
    });
    assert!(matched > 0, "`{}` never calls `{target}` through its ownership ABI", function.name);
}

fn multi_call_destinations(function: &WirFunc, target: &str) -> Vec<Vec<String>> {
    let mut destinations = Vec::new();
    visit_wir_nodes(&function.body, &mut |node| {
        if let WirNode::CallStoreMulti { func, dests, .. } = node
            && func == target
        {
            destinations.push(dests.clone());
        }
    });
    destinations
}

fn assert_multi_call_destinations(function: &WirFunc, target: &str, expected: &[&str]) {
    let destinations = multi_call_destinations(function, target);
    assert!(
        destinations.iter().any(|actual| {
            actual.iter().map(String::as_str).eq(expected.iter().copied())
        }),
        "`{}` has no `{target}` call with ordered destinations {expected:?}: {destinations:?}",
        function.name,
    );
}

fn assert_output_local_order(function: &WirFunc, expected: &[&str]) {
    let start = function
        .body
        .len()
        .checked_sub(expected.len())
        .unwrap_or_else(|| panic!("`{}` has too few physical outputs", function.name));
    let actual = function.body[start..]
        .iter()
        .map(|node| match node {
            WirNode::Push(WirExpr::GetLocal(local)) => local.as_str(),
            other => panic!("`{}` has a non-local physical output: {other:?}", function.name),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual.as_slice(),
        expected,
        "physical output component order changed for `{}`",
        function.name,
    );
}

fn multi_calls_to(function: &WirFunc, target: &str) -> usize {
    let mut calls = 0;
    visit_wir_nodes(&function.body, &mut |node| {
        if matches!(node, WirNode::CallStoreMulti { func, .. } if func == target) {
            calls += 1;
        }
    });
    calls
}

fn has_rebind(function: &WirFunc, local: &str, source: &str) -> bool {
    let mut found = false;
    visit_wir_nodes(&function.body, &mut |node| {
        if matches!(node, WirNode::SetLocal {
            local: destination,
            value: WirExpr::GetLocal(origin),
        } if destination == local && origin == source)
        {
            found = true;
        }
    });
    found
}

fn has_unconditional_branch(function: &WirFunc, target: &str) -> bool {
    let mut found = false;
    visit_wir_nodes(&function.body, &mut |node| {
        if matches!(node, WirNode::Br { target: branch, cond: None } if branch == target) {
            found = true;
        }
    });
    found
}

#[test]
fn every_access_consumer_uses_the_checked_logical_envelope() {
    let checked = witchy::resolve_std_only_checked(ACCESS_CONSUMER_MATRIX)
        .expect("checked access-consumer matrix");
    witchy::enforce_performance_modes(checked.module(), "main")
        .expect("the access-consumer matrix satisfies mode opt");
    let interpreted = interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("interpret access-consumer matrix");
    assert_eq!(
        interpreted, ACCESS_CONSUMER_EXPECTED,
        "the source-level access-consumer oracle changed",
    );
    let (optimized, _) = compiled_output(&checked, OptSet::all());
    assert_eq!(
        optimized, ACCESS_CONSUMER_EXPECTED,
        "optimized Wasm changed an access-consumer result or writeback",
    );
    let (deoptimized, _) = compiled_output(&checked, OptSet::none());
    assert_eq!(
        deoptimized, ACCESS_CONSUMER_EXPECTED,
        "deoptimized Wasm changed an access-consumer result or writeback",
    );

    let lowered = witchy_types::traits::lower(checked.module().clone());
    let typed = witchy_types::typeck::annotate_checked(lowered)
        .expect("final typed access-consumer matrix");
    let facts = checked_facts(typed.module(), typed.table())
        .expect("one checked access authority for the consumer matrix");
    let direct_expected = direct_logical_envelope();
    let receiver_expected = receiver_logical_envelope();

    let direct_call = let_value(typed.module(), "direct_result");
    let direct_name = resolved_call_name(direct_call);
    let direct = facts.declaration(direct_name).expect("direct declaration access");
    assert_eq!(facts.call_at(typed.module(), direct_call), Some(direct));
    assert_eq!(logical_access_envelope(direct), direct_expected);

    let method_call = let_value(typed.module(), "method_result");
    let method_name = resolved_call_name(method_call);
    assert!(method_name.ends_with("__touch"), "resolved inherent method: {method_name}");
    let method = facts.declaration(method_name).expect("inherent method declaration access");
    assert_eq!(facts.call_at(typed.module(), method_call), Some(method));
    assert_eq!(logical_access_envelope(method), receiver_expected);

    let function_value = let_value(typed.module(), "function_value");
    assert_eq!(facts.callable_at(typed.module(), function_value), Some(direct));
    assert_eq!(
        logical_access_envelope(
            facts
                .callable_at(typed.module(), function_value)
                .expect("named function-value access"),
        ),
        direct_expected,
    );
    let function_value_call = let_value(typed.module(), "function_value_result");
    assert_eq!(facts.call_at(typed.module(), function_value_call), Some(direct));
    assert_eq!(
        logical_access_envelope(
            facts
                .call_at(typed.module(), function_value_call)
                .expect("named function-value call access"),
        ),
        direct_expected,
    );

    let closure = let_value(typed.module(), "closure");
    let closure_access = facts
        .callable_at(typed.module(), closure)
        .expect("checked lambda access identity");
    let lambda_call = let_value(typed.module(), "lambda_result");
    assert_eq!(facts.call_at(typed.module(), lambda_call), Some(closure_access));
    assert_eq!(logical_access_envelope(closure_access), direct_expected);
    assert_eq!(
        logical_access_envelope(
            facts
                .call_at(typed.module(), lambda_call)
                .expect("lambda call access"),
        ),
        direct_expected,
    );

    let apply_call = let_value(typed.module(), "apply_result");
    assert!(matches!(apply_call, Expr::Apply { .. }), "fixture must retain Expr::Apply");
    assert_eq!(facts.call_at(typed.module(), apply_call), Some(direct));
    assert_eq!(
        logical_access_envelope(
            facts
                .call_at(typed.module(), apply_call)
                .expect("Apply call access"),
        ),
        direct_expected,
    );

    let trait_call = let_value(typed.module(), "static_trait_result");
    let trait_target = resolved_call_name(trait_call);
    assert!(trait_target.ends_with("__revise"), "resolved trait target: {trait_target}");
    let trait_access = facts
        .declaration(trait_target)
        .expect("statically selected trait implementation access");
    assert_eq!(facts.call_at(typed.module(), trait_call), Some(trait_access));
    assert_eq!(logical_access_envelope(trait_access), receiver_expected);

    let existential_call = let_value(typed.module(), "existential_result");
    assert!(matches!(existential_call, Expr::ExistentialCall { .. }));
    let existential_access = facts
        .call_at(typed.module(), existential_call)
        .expect("existential call access identity");
    assert_eq!(
        logical_access_envelope(existential_access),
        receiver_expected,
        "erasure may change the receiver type but not its source-required access envelope",
    );

    opt::set_for_tests(Some(OptSet::all()));
    let wir = codegen::assemble_checked_optimized_wir_module(&checked)
        .expect_lowered("optimized access-consumer WIR");
    opt::set_for_tests(None);

    let main_name = function(typed.module(), "main").name.as_str();
    let main = wir
        .funcs
        .iter()
        .find(|function| function.name == main_name)
        .expect("lowered access-consumer main");
    let direct_function = wir
        .funcs
        .iter()
        .find(|function| function.name == direct_name)
        .expect("lowered direct access function");
    assert_wir_signature(
        direct_function,
        &[
            ("values", WirTy::Bool),
            ("value", WirTy::Int),
            ("values__cap", WirTy::Bool),
        ],
        &[WirTy::Bool, WirTy::Bool, WirTy::Bool, WirTy::Bool],
    );
    assert_multi_call_shape(main, direct_name, 3, 4);
    assert_multi_call_destinations(
        main,
        direct_name,
        &[
            "__witchy_call_result_i32",
            "__witchy_unique_result_cap",
            "__witchy_var_result_i32_0",
            "direct_values__cap",
        ],
    );

    let method_function = wir
        .funcs
        .iter()
        .find(|function| function.name == method_name)
        .expect("lowered inherent method");
    assert_wir_signature(
        method_function,
        &[
            ("self", WirTy::Bool),
            ("values", WirTy::Bool),
            ("values__cap", WirTy::Bool),
        ],
        &[WirTy::Bool, WirTy::Bool, WirTy::Bool, WirTy::Bool],
    );
    assert_multi_call_shape(main, method_name, 3, 4);
    assert_multi_call_destinations(
        main,
        method_name,
        &[
            "__witchy_call_result_i32",
            "__witchy_unique_result_cap",
            "__witchy_var_result_i32_0",
            "method_values__cap",
        ],
    );

    let trait_function = wir
        .funcs
        .iter()
        .find(|function| function.name == trait_target)
        .expect("lowered static trait target");
    assert_wir_signature(
        trait_function,
        &[
            ("self", WirTy::Bool),
            ("values", WirTy::Bool),
            ("values__cap", WirTy::Bool),
        ],
        &[WirTy::Bool, WirTy::Bool, WirTy::Bool, WirTy::Bool],
    );
    assert_multi_call_shape(main, trait_target, 3, 4);
    assert_multi_call_destinations(
        main,
        trait_target,
        &[
            "__witchy_call_result_i32",
            "__witchy_unique_result_cap",
            "__witchy_var_result_i32_0",
            "static_trait_values__cap",
        ],
    );

    let adapters: Vec<_> = wir
        .funcs
        .iter()
        .filter(|function| function.name.starts_with("__dynw"))
        .collect();
    assert_eq!(adapters.len(), 1, "one closed existential witness adapter");
    let adapter = adapters[0];
    assert_wir_signature(
        adapter,
        &[
            ("receiver", WirTy::StructRef),
            ("arg0", WirTy::Bool),
            ("arg0__cap", WirTy::Bool),
        ],
        &[WirTy::Bool, WirTy::Bool, WirTy::Bool, WirTy::Bool],
    );
    assert_multi_call_shape(adapter, trait_target, 3, 4);
    let adapter_destinations = [
        format!("{}_result", adapter.name),
        format!("{}_unique_cap", adapter.name),
        format!("{}_arg0", adapter.name),
        format!("{}_arg0_cap", adapter.name),
    ];
    assert!(
        multi_call_destinations(adapter, trait_target)
            .iter()
            .any(|actual| actual.as_slice() == adapter_destinations.as_slice()),
        "existential adapter result components changed order: {:?}",
        adapter.body,
    );
    assert_output_local_order(
        adapter,
        &[
            adapter_destinations[0].as_str(),
            adapter_destinations[1].as_str(),
            adapter_destinations[2].as_str(),
            adapter_destinations[3].as_str(),
        ],
    );

    let self_tail_name = function(typed.module(), "self_tail").name.as_str();
    let self_tail_access = facts
        .declaration(self_tail_name)
        .expect("self-tail declaration access");
    assert_eq!(logical_access_envelope(self_tail_access), direct_expected);
    let self_tail = wir
        .funcs
        .iter()
        .find(|function| function.name == self_tail_name)
        .expect("lowered self-tail function");
    assert_wir_signature(
        self_tail,
        &[
            ("values", WirTy::Bool),
            ("n", WirTy::Int),
            ("values__cap", WirTy::Bool),
        ],
        &[WirTy::Bool, WirTy::Bool, WirTy::Bool, WirTy::Bool],
    );
    assert_output_local_order(
        self_tail,
        &[
            "__witchy_tail_result",
            "__witchy_unique_result_cap",
            "values",
            "values__cap",
        ],
    );
    let self_loop = match self_tail.body.first() {
        Some(WirNode::Block { body, .. }) => match body.first() {
            Some(WirNode::Loop { label, .. }) => label,
            node => panic!("self-tail exit block lacks a loop: {node:?}"),
        },
        node => panic!("self-tail envelope lacks an exit block: {node:?}"),
    };
    assert!(
        has_rebind(self_tail, "values", "__witchy_tail_arg_0")
            && has_rebind(self_tail, "values__cap", "__witchy_tail_arg_2"),
        "self-tail transition must rebind both the list value and its capacity: {:?}",
        self_tail.body,
    );
    assert!(has_unconditional_branch(self_tail, self_loop));
    assert_eq!(multi_calls_to(self_tail, self_tail_name), 0);

    let left_name = function(typed.module(), "mutual_left").name.as_str();
    let right_name = function(typed.module(), "mutual_right").name.as_str();
    let left_access = facts.declaration(left_name).expect("left tail access");
    let right_access = facts.declaration(right_name).expect("right tail access");
    assert_eq!(logical_access_envelope(left_access), direct_expected);
    assert_eq!(logical_access_envelope(right_access), direct_expected);
    let dispatcher = wir
        .funcs
        .iter()
        .find(|function| function.name.contains("__witchy_tail_envelope_scc"))
        .expect("capacity-bearing mutual-tail dispatcher");
    for name in [left_name, right_name] {
        let entry = wir
            .funcs
            .iter()
            .find(|function| function.name == name)
            .unwrap_or_else(|| panic!("missing mutual-tail entry `{name}`"));
        assert_wir_signature(
            entry,
            &[
                ("values", WirTy::Bool),
                ("n", WirTy::Int),
                ("values__cap", WirTy::Bool),
            ],
            &[WirTy::Bool, WirTy::Bool, WirTy::Bool, WirTy::Bool],
        );
        assert_multi_call_shape(entry, &dispatcher.name, 7, 4);
        assert_output_local_order(
            entry,
            &[
                "__witchy_tail_wrapper_result_0",
                "__witchy_tail_wrapper_result_1",
                "__witchy_tail_wrapper_result_2",
                "__witchy_tail_wrapper_result_3",
            ],
        );
    }
    assert_wir_signature(
        dispatcher,
        &[
            ("__witchy_tail_state", WirTy::Bool),
            ("__witchy_tail_p_0_0", WirTy::Bool),
            ("__witchy_tail_p_0_1", WirTy::Int),
            ("__witchy_tail_p_0_2", WirTy::Bool),
            ("__witchy_tail_p_1_0", WirTy::Bool),
            ("__witchy_tail_p_1_1", WirTy::Int),
            ("__witchy_tail_p_1_2", WirTy::Bool),
        ],
        &[WirTy::Bool, WirTy::Bool, WirTy::Bool, WirTy::Bool],
    );
    assert_output_local_order(
        dispatcher,
        &[
            "__witchy_tail_result_0",
            "__witchy_tail_result_1",
            "__witchy_tail_result_2",
            "__witchy_tail_result_3",
        ],
    );
    let dispatcher_loop = match dispatcher.body.first() {
        Some(WirNode::Block { body, .. }) => match body.first() {
            Some(WirNode::Loop { label, .. }) => label,
            node => panic!("mutual-tail dispatcher block lacks a loop: {node:?}"),
        },
        node => panic!("mutual-tail dispatcher lacks an exit block: {node:?}"),
    };
    for state in 0..2 {
        assert!(
            has_rebind(
                dispatcher,
                &format!("__witchy_tail_p_{state}_0"),
                &format!("__witchy_tail_arg_{state}_0"),
            ) && has_rebind(
                dispatcher,
                &format!("__witchy_tail_p_{state}_2"),
                &format!("__witchy_tail_arg_{state}_2"),
            ),
            "mutual-tail state {state} must rebind both the list value and its capacity: {:?}",
            dispatcher.body,
        );
    }
    assert!(has_unconditional_branch(dispatcher, dispatcher_loop));
    assert!(
        multi_calls_to(dispatcher, left_name) == 0
            && multi_calls_to(dispatcher, right_name) == 0,
        "mutual-tail dispatcher retained a backend call: {:?}",
        dispatcher.body,
    );

    opt::set_for_tests(Some(
        OptSet::all()
            .without(Opt::DirectCall)
            .without(Opt::ClosureElide),
    ));
    let indirect_wir = codegen::assemble_checked_optimized_wir_module(&checked)
        .expect_lowered("access-consumer WIR with indirect calls retained");
    opt::set_for_tests(None);
    let indirect_main = indirect_wir
        .funcs
        .iter()
        .find(|function| function.name == main_name)
        .expect("deoptimized access-consumer main");
    let mut indirect_calls = Vec::new();
    visit_wir_nodes(&indirect_main.body, &mut |node| {
        if let WirNode::CallIndirectStoreMulti { signature, dests, .. } = node {
            indirect_calls.push((signature.clone(), dests.clone()));
        }
    });
    let closure_signature = ClosureSignature {
        params: vec![Kind::GcRef(0), Kind::I64, Kind::I64, Kind::I32],
        results: vec![Kind::I64, Kind::I32, Kind::I64, Kind::I32],
    };
    let existential_signature = ClosureSignature {
        params: vec![Kind::StructRef, Kind::I32, Kind::I32],
        results: vec![Kind::I32, Kind::I32, Kind::I32, Kind::I32],
    };
    assert_eq!(
        indirect_calls
            .iter()
            .filter(|(signature, _)| *signature == closure_signature)
            .count(),
        3,
        "function value, lambda, and Apply must each retain the exact ownership-table ABI: {indirect_calls:?}",
    );
    assert_eq!(
        indirect_calls
            .iter()
            .filter(|(signature, _)| *signature == existential_signature)
            .count(),
        1,
        "existential dispatch must retain its exact witness-table ABI: {indirect_calls:?}",
    );
    assert_eq!(
        indirect_calls.len(),
        4,
        "the deoptimized main has one physical indirect consumer per source-indirect shape",
    );
    for (signature, destinations) in &indirect_calls {
        if signature == &closure_signature {
            assert_eq!(destinations.len(), 4);
            assert_eq!(destinations[0], "__witchy_match_tmp");
            assert_eq!(destinations[1], "__witchy_unique_result_cap");
            assert_eq!(destinations[2], "__witchy_scrut_save_0");
            assert!(destinations[3].ends_with("__cap"));
        } else if signature == &existential_signature {
            assert_eq!(destinations.len(), 4);
            assert_eq!(destinations[0], "__witchy_call_result_i32");
            assert_eq!(destinations[1], "__witchy_unique_result_cap");
            assert_eq!(destinations[2], "__witchy_var_result_i32_0");
            assert_eq!(destinations[3], "existential_values__cap");
        } else {
            panic!("unexpected indirect ownership ABI: {signature:?} {destinations:?}");
        }
    }

    let diagnostics_module = witchy::resolve_std_only_checked(ACCESS_DIAGNOSTIC_MATRIX)
        .expect("resolve access diagnostic matrix");
    let diagnostic = witchy::enforce_performance_modes(diagnostics_module.module(), "main")
        .expect_err("every aliased access-consumer call must fail mode opt");
    let misses = witchy_lower::analysis::try_module_no_copy_misses(diagnostics_module.module())
        .expect("diagnostics must use a complete checked access graph");
    let misses = misses
        .into_iter()
        .filter(|miss| {
            miss.function
                .rsplit('.')
                .next()
                .is_some_and(|function| function.starts_with("diagnostic_"))
        })
        .collect::<Vec<_>>();
    assert_eq!(misses.len(), 7, "one source diagnostic per access-consumer shape: {misses:?}");
    let callers = misses
        .iter()
        .map(|miss| miss.function.rsplit('.').next().unwrap_or(&miss.function))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        callers,
        std::collections::BTreeSet::from([
            "diagnostic_apply",
            "diagnostic_direct",
            "diagnostic_existential",
            "diagnostic_function_value",
            "diagnostic_lambda",
            "diagnostic_method",
            "diagnostic_trait",
        ]),
        "every source call shape must retain its checked unique requirement: {misses:?}",
    );
    for miss in &misses {
        let caller = miss.function.rsplit('.').next().unwrap_or(&miss.function);
        let source_callee = match caller {
            "diagnostic_direct" => miss.callee.ends_with("direct_access"),
            "diagnostic_method" => miss.callee.ends_with("Box"),
            "diagnostic_trait" => miss.callee.ends_with("Lists"),
            "diagnostic_function_value" | "diagnostic_lambda" | "diagnostic_apply" => {
                miss.callee == "indirect function"
            }
            "diagnostic_existential" => miss.callee == "existential dispatch",
            other => panic!("unexpected diagnostic consumer `{other}`"),
        };
        assert!(source_callee, "diagnostic names the wrong source callable: {miss:?}");
        assert_eq!(miss.var, "values", "diagnostic must name the source argument");
        assert!(miss.line > 0, "diagnostic must retain a source line: {miss:?}");
        assert!(miss.reason.contains("bound to a new name"), "{miss:?}");
        assert!(
            diagnostic.contains(&format!("error: in `{}` (line {})", miss.function, miss.line))
                && diagnostic.contains(&format!(
                    "`{}` cannot satisfy the no-copy `var` contract of `{}`",
                    miss.var, miss.callee,
                ))
                && diagnostic.contains(&miss.reason),
            "production diagnostic dropped a checked source field: {diagnostic}",
        );
    }
    assert_eq!(
        diagnostic.matches("cannot satisfy the no-copy `var` contract").count(),
        7,
        "production enforcement must surface all seven source consumers: {diagnostic}",
    );
    assert_eq!(
        diagnostic.matches("keep `values` uniquely owned").count(),
        7,
        "each source error must include the ownership repair: {diagnostic}",
    );
    assert!(diagnostic.contains("[mode opt]"), "{diagnostic}");
    assert!(
        ["__cap", "token local", "__witchy", "__touch", "__revise"]
            .iter()
            .all(|backend_name| !diagnostic.contains(backend_name)),
        "source diagnostics leaked backend ownership names: {diagnostic}",
    );
}

const COMBINED_ASCRIPTION_DECLARATION: &str = r#"
mode opt

fn strict(var values: unique List(Int)) -> unique List(Int):
    [0]
"#;

#[test]
fn combined_access_envelope_cannot_be_erased_at_any_ascription_boundary() {
    let source = format!(
        "{COMBINED_ASCRIPTION_DECLARATION}\nfn main() -> Int:\n    0\n"
    );
    let module = witchy_syntax::parser::parse_module(&source)
        .expect("parse combined access-envelope declaration");
    witchy_types::typeck::check(&module)
        .expect("check combined access-envelope declaration without unrelated std modules");
    let typed = witchy_types::typeck::annotate_checked(module)
        .expect("typed combined access-envelope declaration");
    let facts = checked_facts(typed.module(), typed.table())
        .expect("checked combined access-envelope facts");
    let strict = function(typed.module(), "strict");
    let signature = facts
        .declaration(&strict.name)
        .expect("combined access-envelope declaration fact");
    assert_eq!(signature.params()[0].kind(), AccessKind::ExclusiveWriteback);
    assert_eq!(signature.params()[0].qualifiers(), &[AccessQualifier::Unique]);
    assert!(signature.params()[0].ownership().input().is_some());
    assert!(signature.params()[0].ownership().writeback().is_some());
    assert_eq!(signature.result().qualifiers(), &[AccessQualifier::Unique]);
    assert!(signature.result().ownership_output().is_some());

    let erased = "fn(var List(Int)) -> List(Int)";
    let cases = [
        (
            "typed local",
            format!(
                "fn main():\n    let erased: {erased} = strict\n    return\n"
            ),
            "function value `erased`",
        ),
        (
            "function cast",
            format!(
                "fn main():\n    let erased = strict as {erased}\n    return\n"
            ),
            "function cast",
        ),
        (
            "higher-order argument",
            format!(
                "fn accept(operation: {erased}) -> Nil:\n    return\n\n\
                 fn main():\n    accept(strict)\n"
            ),
            "argument 1 passed to `accept`",
        ),
        (
            "returned function value",
            format!(
                "fn erase() -> {erased}:\n    strict\n\nfn main() -> Int:\n    0\n"
            ),
            "returned function value",
        ),
    ];
    for (name, body, context) in cases {
        let error = witchy::typeck::check_str(&format!(
            "{COMBINED_ASCRIPTION_DECLARATION}\n{body}"
        ))
        .expect_err("the combined access envelope must not be erased");
        if name == "function cast" {
            assert!(
                error.contains("`as` narrows a capability to a subset of its rights")
                    && error.contains(&format!("cannot ascribe `{erased}` as `{erased}`")),
                "function cast did not produce the canonical capability-subset rejection: {error}",
            );
            continue;
        }
        assert!(
            error.contains(context)
                && error.contains("erases or changes its ownership/access contract")
                && error.contains("Qualifier"),
            "{name} produced a generic mismatch instead of the checked access rejection: {error}",
        );
    }
}

struct ProjectionEntrypoint {
    name: &'static str,
    declarations: &'static str,
    setup: &'static str,
    exchange: &'static str,
    reserve: &'static str,
}

const PROJECTION_PROGRAM: &str = r#"
mode opt

type Pair:
    left: Int
    right: Int

fn exchange(var left: Int, var right: Int) -> Int:
    left = left + 10
    right = right + 20
    left + right

fn reserve(var whole: Pair, var part: Int) -> Nil:
    return
"#;

fn projection_source(entrypoint: &ProjectionEntrypoint, body: &str) -> String {
    format!(
        "{PROJECTION_PROGRAM}\n{}\nfn main() -> Int:\n\
         \x20   var pair = Pair(1, 2)\n\
         \x20   var rows = [[1, 2]]\n\
         \x20   var index = 0\n{}{}\n    0\n",
        entrypoint.declarations, entrypoint.setup, body,
    )
}

#[test]
fn checked_place_overlap_matrix_is_shared_by_every_var_call_entrypoint() {
    let entrypoints = [
        ProjectionEntrypoint {
            name: "direct",
            declarations: "",
            setup: "",
            exchange: "exchange",
            reserve: "reserve",
        },
        ProjectionEntrypoint {
            name: "function value",
            declarations: "",
            setup: "    let exchange_call = exchange\n    let reserve_call = reserve\n",
            exchange: "exchange_call",
            reserve: "reserve_call",
        },
        ProjectionEntrypoint {
            name: "existential",
            declarations: r#"
trait ProjectionOps:
    fn exchange(let self, var left: Int, var right: Int) -> Int
    fn reserve(let self, var whole: Pair, var part: Int) -> Nil

type Ops:
    Ops

impl ProjectionOps for Ops:
    fn exchange(let self, var left: Int, var right: Int) -> Int:
        left = left + 10
        right = right + 20
        left + right

    fn reserve(let self, var whole: Pair, var part: Int) -> Nil:
        return
"#,
            setup: "    let operation: dyn ProjectionOps = Ops\n",
            exchange: "operation.exchange",
            reserve: "operation.reserve",
        },
    ];

    for entrypoint in &entrypoints {
        let accepted = format!(
            "    let _ = {}(pair.left, pair.right)\n\
             \x20   let _ = {}(rows[0][0], rows[0][1])",
            entrypoint.exchange, entrypoint.exchange,
        );
        witchy::resolve_std_only_checked(&projection_source(entrypoint, &accepted))
            .unwrap_or_else(|error| panic!("{} rejected disjoint places: {error}", entrypoint.name));

        let rejected = [
            (
                "identical field",
                format!("    let _ = {}(pair.left, pair.left)", entrypoint.exchange),
                "pair",
            ),
            (
                "ancestor field",
                format!("    let _ = {}(pair, pair.left)", entrypoint.reserve),
                "pair",
            ),
            (
                "dynamic index",
                format!(
                    "    let _ = {}(rows[0][index], rows[0][1])",
                    entrypoint.exchange,
                ),
                "rows",
            ),
        ];
        for (case, call, root) in rejected {
            let error = witchy::resolve_std_only_checked(&projection_source(entrypoint, &call))
                .expect_err("overlapping or unknown places must be rejected")
                .to_string();
            assert!(
                error.contains("overlapping `var` places") && error.contains(root),
                "{} {case} did not use the shared checked-place rejection: {error}",
                entrypoint.name,
            );
        }
    }
}
