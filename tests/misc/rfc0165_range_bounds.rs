//! Regression: inferring only a transient desugared Range clone omitted type
//! facts for the retained source lo/hi nodes. Checked Coven source then failed
//! admission with MissingCheckedExpressionType on the parameterized `n + 1`.
//! Keep expressions in the source range itself; no hoisting workaround.
use witchy::runtime::{Capabilities, Runtime};
use witchy_syntax::ast::{Expr, Item, Stmt};
use witchy_types::typeck::TypeTable;
use witchy_wir::canonical_interpreter::{
    interpret_entry_program, interpret_program, InterpreterConfig, InterpreterValue,
};
use witchy_wir::canonical_safewir::FunctionId;

fn assert_bound_typed(table: &TypeTable, bound: &Expr) {
    let ty = table.type_of(bound)
        .unwrap_or_else(|| panic!("retained source range bound has no checked type: {bound:?}"));
    assert!(witchy_types::typeck::ty_to_ast(ty).is_some(),
        "retained source range bound must have a resolved type: {bound:?}: {ty:?}");
    if let Expr::Binary { lhs, rhs, .. } = bound {
        assert_bound_typed(table, lhs);
        assert_bound_typed(table, rhs);
    }
}

fn run_range(source: &str, inclusive: bool, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("range source checks");
    let canonical = checked.canonical_source().expect("authenticated canonical source");
    let typed = canonical.typed_module();
    let function = typed.module().items.iter().find_map(|item| match item {
        Item::Function(function) if function.name == "count"
            || typed.module().linked_entry.as_deref()
                .is_some_and(|entry| function.name == format!("{entry}.count")) => Some(function),
        _ => None,
    }).expect("retain the parameterized source function");
    let range = function.body.stmts.iter().find_map(|statement| match statement {
        Stmt::Expr(Expr::For { iter, .. }) => match iter.as_ref() {
            Expr::Range { lo, hi, inclusive } => Some((lo, hi, *inclusive)),
            _ => None,
        },
        _ => None,
    }).expect("retain the original source Range nodes, not only a desugared clone");
    assert_eq!(range.2, inclusive);
    assert_bound_typed(typed.table(), range.0);
    assert_bound_typed(typed.table(), range.1);

    let (artifact, ()) = witchy::codegen::compile_checked_module_artifact_with_observer(
        &checked,
        |validated, artifact| {
            let entry = artifact.entry().expect("source-bound range entry");
            let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
            for with_argv in [false, true] {
                let capabilities = entry.capabilities().iter().copied()
                    .map(InterpreterValue::entry_capability).collect();
                let run = if with_argv {
                    interpret_entry_program(validated, function, capabilities, &[], InterpreterConfig::default())
                } else {
                    interpret_program(validated, function, capabilities, InterpreterConfig::default())
                }.expect("canonical range oracle executes");
                assert_eq!(run.output, expected, "independent oracle output; argv={with_argv}");
                assert_eq!(run.leaked_objects, 0, "range oracle releases its objects");
            }
        },
    ).expect_lowered("checked range source independently admits without a source workaround");

    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Capabilities::none()
    }, 256).expect("instantiate admitted range artifact");
    vm.run().expect("admitted native range executes");
    assert_eq!(vm.output(), expected, "independent native output");
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn range_parameterized_add_bound_retains_types_and_admits() {
    run_range(r#"fn count(console: Console, n: Int):
    for i in 0..n + 1:
        console.print("${i}")
fn main(console: Console):
    count(console, 2)
"#, false, &["0", "1", "2"]);
}

#[test]
fn range_inclusive_parameterized_bounds_retain_types_and_admit() {
    run_range(r#"fn count(console: Console, n: Int):
    for i in n - 1..=n + 1:
        console.print("${i}")
fn main(console: Console):
    count(console, 2)
"#, true, &["1", "2", "3"]);
}

fn observable_bounds_source(operator: &str, upper: i64) -> String {
    format!(r#"fn bound(console: Console, var calls: Int, label: String, value: Int) -> Int:
    calls = calls + 1
    console.print("${{label}}:${{calls}}")
    value
fn count(console: Console):
    var calls = 0
    for i in bound(console, calls, "lo", 1){operator}bound(console, calls, "hi", {upper}):
        console.print("i=${{i}} calls=${{calls}}")
    console.print("done:${{calls}}")
fn main(console: Console):
    count(console)
"#)
}

#[test]
fn range_bound_calls_evaluate_left_to_right_exactly_once() {
    run_range(&observable_bounds_source("..", 3), false,
        &["lo:1", "hi:2", "i=1 calls=2", "i=2 calls=2", "done:2"]);
    run_range(&observable_bounds_source("..=", 3), true,
        &["lo:1", "hi:2", "i=1 calls=2", "i=2 calls=2", "i=3 calls=2", "done:2"]);
}

#[test]
fn range_empty_still_evaluates_both_bounds_exactly_once() {
    run_range(&observable_bounds_source("..", 0), false, &["lo:1", "hi:2", "done:2"]);
}
