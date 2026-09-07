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
fn admitted_while_loops_with_short_circuit_conditions_run() {
    // `while a && b` decides the exit in a block after the loop header; the
    // encoder emits the header structurally inside the loop and turns the
    // later exit branch into a `br` out of it. `||` and an early return in
    // the body take the same path.
    let output = run_admitted(
        "pub fn first_gap(xs: List(Int)) -> Int:\n    var i = 0\n    var found = false\n    while i < xs.length() && !found:\n        if xs[i] != i:\n            found = true\n        else:\n            i = i + 1\n    i\n\npub fn count_until(xs: List(Int), stop: Int) -> Int:\n    var i = 0\n    while i == 0 || xs[i - 1] != stop:\n        if i == xs.length():\n            return 0 - 1\n        i = i + 1\n    i\n\nfn main(console: Console):\n    console.print(\"${first_gap([0, 1, 5, 3])} ${first_gap([0, 1])} ${count_until([4, 7, 9], 7)} ${count_until([4, 7, 9], 2)}\")\n",
    );
    assert_eq!(output, vec!["2 2 2 -1".to_string()]);
}

#[test]
fn admitted_for_loops_carry_bindings_assigned_inside_nested_arms() {
    // The `for` lowering once scanned only the body's top-level statements
    // for rebinding, so `n = n + 1` inside an `if` was dropped and the loop
    // silently kept the entry value; the parity check caught it.
    let output = run_admitted(
        "pub fn count_even(xs: List(Int)) -> Int:\n    var n = 0\n    var last = 0 - 1\n    for x in xs:\n        if x % 2 == 0:\n            n = n + 1\n            last = x\n    n * 100 + last\n\nfn main(console: Console):\n    console.print(\"${count_even([1, 2, 3, 4, 6])} ${count_even([1, 3])}\")\n",
    );
    assert_eq!(output, vec!["306 -1".to_string()]);
}

#[test]
fn admitted_string_equality_and_char_count_scan_bytes() {
    // String equality is a byte scan whose loop leaves through drop blocks
    // on two edges (length mismatch, first differing byte); `char_count`
    // counts UTF-8 sequence starts without a branch.
    let output = run_admitted(
        "pub fn count(words: List(String), target: String) -> Int:\n    var n = 0\n    for w in words:\n        if w == target:\n            n = n + 1\n    n\n\npub fn same(a: String, b: String) -> Bool:\n    a == b\n\nfn main(console: Console):\n    let words = [\"cat\", \"dog\", \"cat\", \"cats\", \"\"]\n    console.print(\"${count(words, \"cat\")} ${count(words, \"\")} ${count(words, \"bird\")}\")\n    if same(\"abc\", \"abc\") && !same(\"abc\", \"abd\") && \"x\" != \"y\" && !(\"\" != \"\"):\n        console.print(\"eq ok\")\n    console.print(\"${\"héllo wörld\".char_count()} ${\"\".char_count()} ${\"日本語\".char_count()} ${\"abc\".char_count()}\")\n",
    );
    assert_eq!(output, vec!["2 1 0".to_string(), "eq ok".to_string(), "11 0 3 3".to_string()]);
}

#[test]
fn admitted_string_pushes_in_arms_and_across_loops_keep_the_prefix() {
    // A list of strings pushed in both arms of an `if` inside a loop, then
    // again in a second loop: the cached length loaded in one arm must not
    // leak into the other (it did not dominate), and the initialized-prefix
    // fact must survive the first loop's exit into the second header.
    let output = run_admitted(
        "pub fn tag(xs: List(String)) -> List(String):\n    var out = []\n    var i = 0\n    while i < xs.length():\n        if i == 0:\n            out.push(\"- \" + xs[i])\n        else:\n            out.push(\"+ \" + xs[i])\n        i = i + 1\n    var j = 0\n    while j < xs.length():\n        out.push(\"= \" + xs[j])\n        j = j + 1\n    out\n\npub fn transpose(m: List(List(Int)), rows: Int, cols: Int) -> List(List(Int)):\n    var out = []\n    for c in 0..cols:\n        var column = []\n        for r in 0..rows:\n            column.push(m[r][c])\n        out.push(column)\n    out\n\nfn main(console: Console):\n    let ys = tag([\"a\", \"bb\"])\n    console.print(\"${ys.length()} ${ys[0]} ${ys[1]} ${ys[3]}\")\n    let t = transpose([[1, 2, 3], [4, 5, 6]], 2, 3)\n    console.print(\"${t.length()} ${t[2][0]} ${t[2][1]}\")\n    var a = [1, 2]\n    let nested = [a, [3]]\n    a.push(9)\n    console.print(\"${nested[0].length()} ${a.length()}\")\n",
    );
    assert_eq!(output, vec!["4 - a + bb = bb".to_string(), "3 3 6".to_string(), "2 3".to_string()]);
}

#[test]
fn admitted_list_of_lists_grows_after_a_shared_element_read() {
    // `table.at(i - 1)` takes a shared clone of a row; the following push
    // relocates the owned slots, which keeps every clone valid, so the
    // prefix's shared mark is accepted and carried to the new allocation.
    let output = run_admitted(
        "pub fn build(n: Int) -> List(List(Int)):\n    var table = [[0]]\n    var i = 1\n    while i <= n:\n        let above = table.at(i - 1)\n        var row = [above.at(0) + 1]\n        row.push(i * 10)\n        table.push(row)\n        i = i + 1\n    table\n\nfn main(console: Console):\n    let t = build(3)\n    console.print(\"${t.length()} ${t[3][0]} ${t[3][1]} ${t[1][0]}\")\n",
    );
    assert_eq!(output, vec!["4 3 30 1".to_string()]);
}

#[test]
fn admitted_substring_and_trim_scan_characters_and_ascii_whitespace() {
    // `substring` counts Unicode scalars and clamps; `trim` strips the ASCII
    // set both backends agree on (a no-break space stays). Each scan is a
    // loop whose exit is decided in a block after the header, nested here
    // inside a `while`.
    let output = run_admitted(
        "fn main(console: Console):\n    let s = \"h\u{e9}llo w\u{f6}rld\"\n    console.print(\"[${s.substring(1, 4)}] [${s.substring(6, 100)}] [${s.substring(3, 1)}] [${s.substring(-2, 2)}] [${\"\".substring(0, 1)}]\")\n    console.print(\"[${\"  \\t hi there \\n\".trim()}] [${\"\".trim()}] [${\"   \".trim()}] [${\"x\".trim()}] [${\" \u{a0}y \".trim()}]\")\n    var i = 0\n    var acc = \"\"\n    while i < 3:\n        acc = acc + s.substring(i, i + 1).trim()\n        i = i + 1\n    console.print(acc)\n",
    );
    assert_eq!(
        output,
        vec![
            "[\u{e9}ll] [w\u{f6}rld] [] [h\u{e9}] []".to_string(),
            "[hi there] [] [] [x] [\u{a0}y]".to_string(),
            "h\u{e9}l".to_string(),
        ]
    );
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

#[test]
fn admitted_if_choosing_between_two_reference_parameters_runs() {
    // Each arm clones the parameter it returns, so the join's result lane owns
    // a token of its own and both parameters release at the return.
    let output = run_admitted(
        "pub fn choose(flag: Bool, left: String, right: String) -> String:\n    if flag:\n        left\n    else:\n        right\n\nfn main(console: Console):\n    console.print(choose(true, \"l\", \"r\") + choose(false, \"l\", \"r\"))\n",
    );
    assert_eq!(output, vec!["lr".to_string()]);
    let output = run_admitted(
        "pub fn choose(flag: Bool, left: List(Int), right: List(Int)) -> List(Int):\n    if flag:\n        left\n    else:\n        right\n\nfn main(console: Console):\n    let a = [1, 2]\n    let b = [3]\n    console.print(\"${choose(true, a, b).length()} ${choose(false, a, b).length()}\")\n",
    );
    assert_eq!(output, vec!["2 1".to_string()]);
}

#[test]
fn admitted_proper_tail_calls_inside_if_and_match_arms_run_in_constant_stack() {
    // RFC-0090 resource evidence through the admitted launch path: 300,000
    // self and mutual tail transitions inside `if`/`match` arms with a
    // 256-slot budget.
    let output = run_admitted(
        "pub fn countdown(n: Int, acc: Int) -> Int:\n    if n == 0:\n        acc\n    else:\n        countdown(n - 1, acc + 1)\n\npub fn spin(n: Int, acc: Int) -> Int:\n    match n:\n        0 -> acc\n        _ -> spin(n - 1, acc + 2)\n\npub fn ping(n: Int) -> Int:\n    if n == 0:\n        0\n    else:\n        pong(n - 1)\n\npub fn pong(n: Int) -> Int:\n    if n == 0:\n        1\n    else:\n        ping(n - 1)\n\nfn main(console: Console):\n    console.print(\"${countdown(300000, 0)} ${spin(300000, 0)} ${ping(300001)}\")\n",
    );
    assert_eq!(output, vec!["300000 600000 1".to_string()]);
}

#[test]
fn admitted_if_joining_a_shared_parameter_with_a_fresh_allocation_runs() {
    // One arm returns the parameter (a shared token), the other a fresh
    // unique list; the fresh token weakens to shared at the join.
    let output = run_admitted(
        "pub fn pick(xs: List(Int), n: Int) -> List(Int):\n    if xs.length() == n:\n        xs\n    else:\n        var out = xs\n        out.push(n)\n        out\n\nfn main(console: Console):\n    console.print(\"${pick([1], 1).length()} ${pick([1], 2).length()}\")\n",
    );
    assert_eq!(output, vec!["1 2".to_string()]);
}

#[test]
fn admitted_list_patterns_run() {
    let output = run_admitted(
        "pub fn sum(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n        [head, ..tail] -> head + sum(tail)\n\npub fn describe(xs: List(Int)) -> String:\n    match xs:\n        [] -> \"empty\"\n        [only] -> \"one: ${only}\"\n        [first, ..] -> \"starts with ${first}\"\n\nfn main(console: Console):\n    console.print(\"${sum([3, 7, 11])}\")\n    console.print(describe([3, 7, 11]))\n    console.print(describe([42]))\n    console.print(describe([]))\n",
    );
    assert_eq!(
        output,
        vec!["21".to_string(), "starts with 3".to_string(), "one: 42".to_string(), "empty".to_string()]
    );
}

#[test]
fn admitted_bytes_launch_from_their_entry_section_without_the_artifact() {
    // RFC-0165 B11: the admitted bytes carry `witchy.entry`; a launcher that
    // only has the `.wasm` file enters through it, and the artifact's exact
    // byte identity covers the section.
    let checked = witchy::resolve_std_only_checked("fn main(console: Console) -> Int:\n    console.print(\"hi\")\n    40 + 2\n")
        .expect("fixture checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the foundation subset admits this fixture");
    let from_bytes = witchy_wir::canonical_entry_from_wasm(artifact.wasm())
        .expect("entry section decodes")
        .expect("a program carries an entry");
    assert_eq!(Some(&from_bytes), artifact.entry());
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn(
            artifact.wasm(),
            Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn admitted bytes");
    vm.run().expect("run from the entry section");
    assert_eq!(vm.output(), vec!["hi".to_string(), "42".to_string()]);
    let mut mutated = artifact.wasm().to_vec();
    let last = mutated.len() - 1;
    mutated[last] ^= 2;
    assert!(artifact.verify_exact_bytes(&mutated).is_err(), "a mutated entry section is not the admitted artifact");
}

#[test]
fn admitted_lists_of_strings_are_read_iterated_and_pushed() {
    // A managed `List(String)` parameter's initialized prefix is seeded from
    // its contract; a local list of strings grows by push; a `var` string
    // accumulator owns its token from its static initializer onward.
    let output = run_admitted(
        "pub fn join_all(xs: List(String)) -> String:\n    var out = \"\"\n    for s in xs:\n        out = out + s\n    out\n\npub fn second(xs: List(String)) -> String:\n    xs[1]\n\nfn main(console: Console):\n    let xs = [\"a\", \"bb\", \"ccc\"]\n    console.print(join_all(xs))\n    console.print(second(xs))\n    var ys = [\"x\"]\n    ys.push(\"yy\")\n    console.print(\"${ys.length()} ${xs.length()}\")\n",
    );
    assert_eq!(output, vec!["abbccc".to_string(), "bb".to_string(), "2 3".to_string()]);
}
