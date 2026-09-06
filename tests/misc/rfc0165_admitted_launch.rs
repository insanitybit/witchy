//! RFC-0165 launch boundary: an admitted canonical artifact runs through the
//! ordinary runtime API, not only through a test that calls `f0` by hand.
//!
//! Canonical modules export `f<id>` and no parameterless `run`; the runtime
//! enters the validated entry recorded at admission, mints one host token per
//! declared capability parameter, and prints an `Int` result under the frozen
//! exit-code convention. These are the two program shapes the compact
//! foundation subset promises to run end to end.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

fn run_admitted(source: &str) -> Vec<String> {
    let checked = witchy::resolve_std_only_checked(source).expect("fixture checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the foundation subset admits this fixture");
    let mut runtime = Runtime::batch().expect("runtime");
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
        .expect("spawn admitted artifact");
    vm.run().expect("run admitted artifact through the runtime entry");
    vm.output()
}

#[test]
fn admitted_console_program_runs_through_the_runtime_entry() {
    let output = run_admitted("fn main(console: Console):\n    console.print(\"ok\")\n");
    assert_eq!(output, vec!["ok".to_string()]);
}

#[test]
fn admitted_int_result_is_printed_under_the_exit_code_convention() {
    let output = run_admitted("fn main() -> Int:\n    40 + 2\n");
    assert_eq!(output, vec!["42".to_string()]);
}

#[test]
fn admitted_entry_without_a_console_grant_is_refused_before_it_runs() {
    let checked = witchy::resolve_std_only_checked(
        "fn main(console: Console):\n    console.print(\"ok\")\n",
    )
    .expect("fixture checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the foundation subset admits this fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    // Without the `print` grant the `witchy.print` import is absent, so the
    // module cannot even instantiate: the structural check fires first, and
    // no Console token is ever minted.
    assert!(runtime
        .spawn_admitted(&artifact, Capabilities { quiet: true, ..Default::default() }, 256)
        .is_err());
}

#[test]
fn admitted_loop_holding_a_capability_runs_every_iteration() {
    // The checker once refused any loop in a function holding a capability:
    // the header parameter carrying `console` had no linear-memory object
    // state, and the edge check demanded one for every non-scalar parameter.
    let output = run_admitted(
        "fn main(console: Console):\n    var i = 0\n    while i < 3:\n        console.print(\"tick\")\n        i = i + 1\n",
    );
    assert_eq!(output, vec!["tick".to_string(); 3]);
}

#[test]
fn admitted_explicit_let_parameters_run() {
    let output = run_admitted(
        "fn twice(let n: Int) -> Int:\n    n * 2\n\nfn main() -> Int:\n    twice(21)\n",
    );
    assert_eq!(output, vec!["42".to_string()]);
}

#[test]
fn admitted_if_without_else_and_effect_arms_run() {
    let output = run_admitted(
        "fn main(console: Console):\n    let a = 1\n    if a > 0:\n        console.print(\"yes\")\n",
    );
    assert_eq!(output, vec!["yes".to_string()]);
    let output = run_admitted(
        "fn main(console: Console):\n    let a = 7\n    if a > 5:\n        console.print(\"big\")\n    else:\n        console.print(\"small\")\n",
    );
    assert_eq!(output, vec!["big".to_string()]);
}

#[test]
fn admitted_short_circuit_operators_run() {
    let output = run_admitted(
        "fn main(console: Console):\n    let a = 1\n    if a > 0 && a < 5:\n        console.print(\"in\")\n    else:\n        console.print(\"out\")\n    if a > 3 || a == 1:\n        console.print(\"or\")\n",
    );
    assert_eq!(output, vec!["in".to_string(), "or".to_string()]);
}

#[test]
fn admitted_string_literals_cross_owned_parameters_and_returns() {
    // A literal has no reference-count token; passing or returning one under
    // the owned ABI materializes a fresh copy rather than cloning static data.
    let output = run_admitted(
        "pub fn shout(name: String) -> String:\n    \"hi \" + name\n\nfn greet() -> String:\n    \"hello\"\n\nfn keep(own value: String) -> String:\n    value\n\nfn main(console: Console):\n    console.print(shout(\"x\"))\n    console.print(greet())\n    console.print(keep(\"ok\"))\n",
    );
    assert_eq!(output, vec!["hi x".to_string(), "hello".to_string(), "ok".to_string()]);
}

#[test]
fn admitted_early_return_in_an_if_arm_runs() {
    let output = run_admitted(
        "pub fn clamp(n: Int) -> Int:\n    if n < 0:\n        return 0\n    n\n\nfn main(console: Console):\n    if clamp(-3) == 0 && clamp(7) == 7:\n        console.print(\"done\")\n",
    );
    assert_eq!(output, vec!["done".to_string()]);
}

#[test]
fn admitted_range_for_loops_run() {
    let output = run_admitted(
        "fn main(console: Console):\n    for i in 0..3:\n        console.print(\"tick\")\n    for j in 1..=2:\n        console.print(\"tock\")\n",
    );
    assert_eq!(output, vec!["tick".to_string(), "tick".to_string(), "tick".to_string(), "tock".to_string(), "tock".to_string()]);
}

#[test]
fn admitted_float_arithmetic_and_integer_division_run() {
    let output = run_admitted(
        "pub fn c(f: Float) -> Float:\n    (f - 32.0) * 5.0 / 9.0\n\nfn main() -> Int:\n    let x = c(212.0)\n    if x > 99.0 && x < 101.0:\n        17 % 5 + 17 / 5\n    else:\n        0\n",
    );
    assert_eq!(output, vec!["5".to_string()]);
}

#[test]
fn admitted_string_interpolation_renders_ints_and_strings() {
    // `"${e}"` desugars to `lit + @render(e) + lit`. Ints render through the
    // static digit table with an arithmetic sign; `Int::MIN` is never negated.
    let output = run_admitted(
        "fn main(console: Console):\n    let n = 16\n    let who = \"witchy\"\n    console.print(\"8 doubled is ${n}\")\n    console.print(\"hi ${who}!\")\n    console.print(\"${0} ${-7} ${1234567890} ${-9223372036854775807 - 1} ${9223372036854775807}\")\n",
    );
    assert_eq!(
        output,
        vec![
            "8 doubled is 16".to_string(),
            "hi witchy!".to_string(),
            "0 -7 1234567890 -9223372036854775808 9223372036854775807".to_string(),
        ]
    );
}

#[test]
fn admitted_scalar_var_parameters_write_back() {
    // A scalar `var` parameter is a value in and a result lane out; the call
    // site rebinds the variable to that lane.
    let output = run_admitted(
        "fn bump(var n: Int):\n    n = n + 1\n\nfn scale(var n: Int, by: Int) -> Int:\n    n = n * by\n    n + 1\n\nfn main(console: Console):\n    var k = 1\n    bump(k)\n    bump(k)\n    let r = scale(k, 10)\n    console.print(\"k=${k} r=${r}\")\n",
    );
    assert_eq!(output, vec!["k=30 r=31".to_string()]);
}

#[test]
fn admitted_scalar_var_write_back_through_an_indirect_call() {
    let output = run_admitted(
        "fn bump(var n: Int) -> Int:\n    n = n + 5\n    n\n\nfn apply(f: fn(var Int) -> Int, var value: Int) -> Int:\n    let r = f(value)\n    r + value\n\nfn main(console: Console):\n    var v = 1\n    let r = apply(bump, v)\n    console.print(\"${r} ${v}\")\n",
    );
    assert_eq!(output, vec!["12 6".to_string()]);
}

#[test]
fn admitted_early_return_inside_a_loop_over_a_list_parameter() {
    // The loop header once refused to join: the entry edge carried the list's
    // header-prefix extent while the back edge carried the counted extent
    // promoted by the body's element read.
    let output = run_admitted(
        "pub fn first_even(xs: List(Int)) -> Int:\n    for x in xs:\n        if x % 2 == 0:\n            return x\n    0 - 1\n\nfn main(console: Console):\n    console.print(\"${first_even([1, 3, 8, 5])} ${first_even([1, 3, 5])}\")\n",
    );
    assert_eq!(output, vec!["8 -1".to_string()]);
}

#[test]
fn admitted_loop_over_list_parameters_drops_the_current_binding_at_exit() {
    // A loop threads list parameters through block parameters; the exit drop
    // must target the current binding, not the entry value.
    let output = run_admitted(
        "pub fn dot(a: List(Int), b: List(Int)) -> Int:\n    var sum = 0\n    var i = 0\n    while i < a.length():\n        sum = sum + a[i] * b[i]\n        i = i + 1\n    sum\n\nfn main(console: Console):\n    let u = [1, 2, 3]\n    let v = [4, 5, 6]\n    console.print(\"dot = ${dot(u, v)}\")\n",
    );
    assert_eq!(output, vec!["dot = 32".to_string()]);
}

#[test]
fn admitted_var_alias_of_a_list_parameter_copies_on_write() {
    let output = run_admitted(
        "pub fn grow(placed: List(Int), col: Int) -> Int:\n    var next = placed\n    next.push(col)\n    next.length()\n\nfn main(console: Console):\n    let base = [1, 2]\n    console.print(\"${grow(base, 3)} ${base.length()}\")\n",
    );
    assert_eq!(output, vec!["3 2".to_string()]);
}

#[test]
fn admitted_guarded_match_arm_with_a_short_circuit_guard_runs() {
    let output = run_admitted(
        "pub fn cls(code: Int) -> String:\n    match code:\n        x if x >= 200 && x <= 299 -> \"success\"\n        _ -> \"unknown\"\n\nfn main(console: Console):\n    console.print(cls(204))\n    console.print(cls(600))\n",
    );
    assert_eq!(output, vec!["success".to_string(), "unknown".to_string()]);
}

#[test]
fn admitted_nested_list_index_runs() {
    let output = run_admitted(
        "fn main(console: Console):\n    let grid = [[1, 2, 3], [4, 5, 6]]\n    console.print(\"g = ${grid[1][2]}\")\n",
    );
    assert_eq!(output, vec!["g = 6".to_string()]);
}

#[test]
fn admitted_recursive_search_threads_a_list_parameter_through_loop_and_join() {
    // `placed` reaches the value join after the loop under two counted
    // extents (the entry's header loads and the loop's carried lanes); the
    // join forgets the count instead of refusing.
    let output = run_admitted(
        "pub fn count(n: Int, placed: List(Int)) -> Int:\n    if placed.length() == n:\n        1\n    else:\n        var total = 0\n        var col = 0\n        while col < n:\n            if col % 2 == 0:\n                var next = placed\n                next.push(col)\n                total = total + count(n, next)\n            col = col + 1\n        total\n\nfn main(console: Console):\n    console.print(\"${count(3, [])}\")\n",
    );
    assert_eq!(output, vec!["8".to_string()]);
}
