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

/// Run an admitted program and report a runtime trap instead of panicking,
/// for fixtures whose expected outcome is the trap.
fn run_admitted_result(source: &str) -> Result<Vec<String>, String> {
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
    match vm.run() {
        Ok(()) => Ok(vm.output()),
        Err(error) => Err(error.to_string()),
    }
}

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
fn admitted_float_conversions_bool_render_and_duration_identities_run() {
    // `math.sqrt`, `math.to_int` (saturating, NaN traps before the
    // conversion), and `math.to_float` are one SafeWIR unary float
    // operation; a `Bool` renders from the static `falsetrue` table; the
    // duration conversions are identities, also at a tail position where
    // the tail-site classifier no longer takes them for calls.
    let output = run_admitted(
        "import math\n\npub fn wrap(n: Int) -> Duration:\n    int_to_duration(n * 2)\n\nfn main(console: Console):\n    let big = 1000000000.0 * 1000000000.0 * 1000000000.0\n    console.print(\"${math.to_int(math.sqrt(2.25) * 10.0)} ${math.to_int(-7.9)} ${math.to_int(big)} ${math.to_int(0.0 - big)} ${math.to_int(math.to_float(41) + 1.5)}\")\n    let d = wrap(750)\n    console.print(\"${duration_to_int(d)} ${duration_to_int(d) > 1000} ${d == 1500ms}\")\n",
    );
    assert_eq!(
        output,
        vec![
            "15 -7 9223372036854775807 -9223372036854775808 42".to_string(),
            "1500 true true".to_string(),
        ]
    );
}

#[test]
fn admitted_heap_records_construct_read_update_join_and_cross_calls() {
    // An ordinary record with scalar fields is an immutable reference-counted
    // heap aggregate: positional and named construction, field reads, spread
    // update, a rebinding joined with a shared alias, and passage through
    // parameters and results.
    let output = run_admitted(
        "type Point:\n    x: Int\n    y: Int\n\npub fn norm1(p: Point) -> Int:\n    p.x + p.y\n\npub fn shift(p: Point) -> Point:\n    Point(x: p.x + 1, y: p.y)\n\nfn main(console: Console):\n    let p = Point(x: 3, y: 4)\n    let q = Point(5, 6)\n    console.print(\"${p.x + q.y}\")\n    var r = p\n    if r.x > 2:\n        r = Point(x: 10, y: r.y)\n    console.print(\"${r.x + r.y} ${norm1(shift(p))} ${p.x}\")\n    let s = Point(y: 99, ..p)\n    console.print(\"${s.x} ${s.y}\")\n",
    );
    assert_eq!(output, vec!["9".to_string(), "14 8 3".to_string(), "3 99".to_string()]);
}

#[test]
fn admitted_sum_types_option_and_result_over_scalars_run() {
    // A sum type is a tag plus one slot per variant field behind a reference;
    // scalar Option/Result share the shape. Constructor patterns select by
    // tag and bind payload slots, `??` takes the payload or the default, and
    // `?` returns the function's own failure from a terminal block.
    let output = run_admitted(
        "type Shape:\n    Circle(Int)\n    Square(Int)\n    Dot\n\npub fn area(s: Shape) -> Int:\n    match s:\n        Circle(r) -> 3 * r * r\n        Square(w) -> w * w\n        Dot -> 0\n\npub fn first(xs: List(Int)) -> Option(Int):\n    match xs:\n        [] -> None\n        [head, ..] -> Some(head)\n\npub fn half(n: Int) -> Result(Int, Int):\n    if n % 2 == 0:\n        Ok(n / 2)\n    else:\n        Err(n)\n\npub fn quarter(n: Int) -> Result(Int, Int):\n    let h = half(n)?\n    half(h)\n\npub fn second(xs: List(Int)) -> Option(Int):\n    let a = first(xs)?\n    Some(a + 1)\n\nfn main(console: Console):\n    console.print(\"${area(Circle(2))} ${area(Square(3))} ${area(Dot)}\")\n    console.print(\"${first([7, 8]) ?? 0} ${first([]) ?? 9} ${second([4]) ?? 0} ${second([]) ?? -1}\")\n    match quarter(12):\n        Ok(v) -> console.print(\"ok ${v}\")\n        Err(e) -> console.print(\"err ${e}\")\n    match quarter(6):\n        Ok(v) -> console.print(\"ok ${v}\")\n        Err(e) -> console.print(\"err ${e}\")\n",
    );
    assert_eq!(
        output,
        vec!["12 9 0".to_string(), "7 9 5 -1".to_string(), "ok 3".to_string(), "err 3".to_string()]
    );
}

#[test]
fn admitted_string_searches_ordering_find_and_to_int_scan_bytes() {
    // starts_with/ends_with/contains compare byte ranges; ordering is the
    // sign of the first differing byte, then of the lengths; find reports
    // the character index of the first match; to_int parses a trimmed
    // signed decimal, accumulating negatively so the minimum parses.
    let output = run_admitted(
        "fn main(console: Console):\n    let s = \"hello world\"\n    console.print(\"${s.starts_with(\"hello\")} ${s.starts_with(\"world\")} ${s.ends_with(\"world\")} ${s.ends_with(\"\")} ${\"\".starts_with(\"a\")}\")\n    console.print(\"${s.contains(\"o w\")} ${s.contains(\"xyz\")} ${s.contains(\"\")} ${\"ab\".contains(\"abc\")}\")\n    console.print(\"${\"apple\" < \"banana\"} ${\"b\" > \"abc\"} ${\"abc\" <= \"abc\"} ${\"abc\" < \"abcd\"} ${\"\" < \"a\"} ${\"z\" >= \"aa\"}\")\n    let t = \"h\u{e9}llo w\u{f6}rld\"\n    console.print(\"${t.find(\"w\u{f6}r\")} ${t.find(\"h\")} ${t.find(\"zz\")} ${t.find(\"\")} ${\"aaa\".find(\"aa\")}\")\n    console.print(\"${\"42\".to_int()} ${\" -17 \".to_int()} ${\"+8\".to_int()} ${\"-9223372036854775808\".to_int()} ${\"0\".to_int()}\")\n",
    );
    assert_eq!(
        output,
        vec![
            "true false true true false".to_string(),
            "true false true false".to_string(),
            "true true true true true true".to_string(),
            "6 0 -1 0 0".to_string(),
            "42 -17 8 -9223372036854775808 0".to_string(),
        ]
    );
}

#[test]
fn admitted_aggregates_with_string_payloads_construct_match_and_propagate() {
    // A record with a string field, Result(Int, String) through `?`, and
    // Option(String) through `??`: reference payloads live behind owned
    // pointer slots, are copied in on construction, and load as shared
    // clones; the destructor drops each child.
    let output = run_admitted(
        "type Account:\n    name: String\n    balance: Int\n\npub fn parse(s: String) -> Result(Int, String):\n    if s.starts_with(\"x\"):\n        Err(\"bad \" + s)\n    else:\n        Ok(s.length())\n\npub fn both(a: String, b: String) -> Result(Int, String):\n    let x = parse(a)?\n    let y = parse(b)?\n    Ok(x + y)\n\npub fn name_of(n: Int) -> Option(String):\n    if n > 0:\n        Some(\"n${n}\")\n    else:\n        None\n\nfn main(console: Console):\n    let acc = Account(name: \"bob\", balance: 5)\n    let richer = Account(balance: acc.balance + 10, ..acc)\n    console.print(\"${acc.name} ${richer.name} ${richer.balance}\")\n    match both(\"ab\", \"cde\"):\n        Ok(v) -> console.print(\"ok ${v}\")\n        Err(e) -> console.print(\"err ${e}\")\n    match both(\"ab\", \"xz\"):\n        Ok(v) -> console.print(\"ok ${v}\")\n        Err(e) -> console.print(\"err ${e}\")\n    console.print(\"${name_of(3) ?? \"none\"} ${name_of(0) ?? \"none\"}\")\n",
    );
    assert_eq!(
        output,
        vec!["bob bob 15".to_string(), "ok 5".to_string(), "err bad xz".to_string(), "n3 none".to_string()]
    );
}

#[test]
fn admitted_tuples_construct_index_destructure_and_nest() {
    // A tuple is an anonymous aggregate: construction, `.n` access, `let`
    // destructuring, nested tuples behind pointer slots, and a tuple as an
    // Option payload matched by a constructor pattern.
    let output = run_admitted(
        "pub fn divmod(a: Int, b: Int) -> (Int, Int):\n    (a / b, a % b)\n\npub fn pick(n: Int) -> Option((Int, String)):\n    if n > 0:\n        Some((n, \"pos\"))\n    else:\n        None\n\nfn main(console: Console):\n    let (q, r) = divmod(17, 5)\n    console.print(\"${q} remainder ${r}\")\n    let pair = (7, \"seven\")\n    console.print(\"${pair.0} spells ${pair.1}\")\n    console.print(\"just the remainder: ${divmod(17, 5).1}\")\n    let grid = ((1, 2), (3, 4))\n    console.print(\"${grid.0.1} ${grid.1.0}\")\n    match pick(3):\n        Some(p) -> console.print(\"${p.0} ${p.1}\")\n        None -> console.print(\"none\")\n    match pick(0):\n        Some(p) -> console.print(\"${p.0} ${p.1}\")\n        None -> console.print(\"none\")\n",
    );
    assert_eq!(
        output,
        vec![
            "3 remainder 2".to_string(),
            "7 spells seven".to_string(),
            "just the remainder: 2".to_string(),
            "2 3".to_string(),
            "3 pos".to_string(),
            "none".to_string(),
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

#[test]
fn admitted_lists_of_aggregates_tuples_and_floats_are_built_read_and_grown() {
    // Records, tuples, and floats as list elements: iteration with field
    // reads, a record lane rebound from a loaded (shared) element, a `var`
    // alias of a literal list grown by push, a `var` list passed to a callee
    // and pushed afterwards (the callee receives a copy), tuple destructuring
    // out of a list, and a float fold.
    let output = run_admitted(
        "type Account:\n    name: String\n    balance: Int\n\npub fn total(accounts: List(Account)) -> Int:\n    var sum = 0\n    for a in accounts:\n        sum = sum + a.balance\n    sum\n\npub fn richest(accounts: List(Account)) -> String:\n    var best = Account(\"nobody\", -1)\n    for a in accounts:\n        if a.balance > best.balance:\n            best = a\n    best.name\n\npub fn count(xs: List(String)) -> Int:\n    xs.length()\n\npub fn mean(xs: List(Float)) -> Float:\n    var sum = 0.0\n    for x in xs:\n        sum = sum + x\n    sum / math.to_float(xs.length())\n\nfn main(console: Console):\n    let accounts = [Account(\"alice\", 100), Account(\"bob\", 50)]\n    console.print(\"total = ${total(accounts)}\")\n    console.print(\"richest = ${richest(accounts)}\")\n    var more = accounts\n    more.push(Account(\"carol\", 250))\n    console.print(\"${total(more)} ${richest(more)} ${accounts.length()}\")\n    let pairs = [(1, \"one\"), (2, \"two\")]\n    var labels = []\n    for pair in pairs:\n        let (n, word) = pair\n        labels.push(\"${n}:\" + word)\n    console.print(labels.join(\" \"))\n    var seen = []\n    seen.push(\"x\")\n    console.print(\"${count(seen)}\")\n    seen.push(\"y\")\n    console.print(\"${count(seen)} ${seen.join(\",\")}\")\n    console.print(\"${mean([1.5, 2.5, 5.0]) > 2.9}\")\n",
    );
    assert_eq!(
        output,
        vec![
            "total = 150".to_string(),
            "richest = alice".to_string(),
            "400 carol 2".to_string(),
            "1:one 2:two".to_string(),
            "1".to_string(),
            "2 x,y".to_string(),
            "true".to_string(),
        ]
    );
}

#[test]
fn admitted_string_chars_split_and_replace_run_over_runtime_strings() {
    // `string.chars`, `string.split`, and `string.replace` over runtime
    // strings: a UTF-8 sequence loop and a match-scan loop feeding the
    // canonical push and concatenation templates. Edge cases follow the
    // interpreter: an empty separator yields the whole string, an empty
    // `from` surrounds every character, trailing separators keep an empty
    // tail piece, and matches never overlap.
    let output = run_admitted(
        "pub fn show(xs: List(String)) -> String:\n    \"[\" + xs.join(\"|\") + \"]\"\n\nfn main(console: Console):\n    let word = \"héllo→\"\n    console.print(show(word.chars()))\n    console.print(\"${word.chars().length()}\")\n    console.print(show(\"a,b,,c\".split(\",\")))\n    console.print(show(\"a,b,c,\".split(\",\")))\n    console.print(show(\"\".split(\",\")))\n    console.print(show(\"abc\".split(\"\")))\n    console.print(show(\"é→\".split(\"\")))\n    console.print(show((\"x\" + \"y\").split(\"\")))\n    console.print(show(\"no separators\".split(\"xyz\")))\n    console.print(show(\"aaa\".split(\"aa\")))\n    console.print(\"a-b-c\".replace(\"-\", \"+\"))\n    console.print(\"aaaa\".replace(\"aa\", \"b\"))\n    console.print(\"ab\".replace(\"\", \"-\"))\n    console.print(\"é→\".replace(\"\", \".\"))\n    console.print(\"keep\".replace(\"zz\", \"y\"))\n    console.print(\"hello world\".replace(\"o\", \"\"))\n    var acc = \"\"\n    for c in \"xyz\".chars():\n        acc = c + acc\n    console.print(acc)\n",
    );
    assert_eq!(
        output,
        vec![
            "[h|é|l|l|o|→]".to_string(),
            "6".to_string(),
            "[a|b||c]".to_string(),
            "[a|b|c|]".to_string(),
            "[]".to_string(),
            "[abc]".to_string(),
            "[é→]".to_string(),
            "[xy]".to_string(),
            "[no separators]".to_string(),
            "[|a]".to_string(),
            "a+b+c".to_string(),
            "bb".to_string(),
            "-a-b-".to_string(),
            ".é.→.".to_string(),
            "keep".to_string(),
            "hell wrld".to_string(),
            "zyx".to_string(),
        ]
    );
}

#[test]
fn admitted_element_reads_after_a_kernel_loop_wait_for_the_converged_prefix() {
    // The checker's dataflow visits a kernel loop's exit with the entry
    // state first (an empty prefix); a read the back edge alone justifies
    // must be held until the states converge, not refused at once. The
    // receiver is a parameter, so the loop bound is not a constant the
    // checker could prune on.
    let output = run_admitted(
        "pub fn first(text: String) -> String:\n    let chars = text.chars()\n    chars.at(0)\n\npub fn encode(text: String) -> String:\n    let chars = text.chars()\n    let n = chars.length()\n    var out = \"\"\n    var i = 0\n    while i < n:\n        let run_char = chars.at(i)\n        var count = 0\n        while i < n && chars.at(i) == run_char:\n            count = count + 1\n            i = i + 1\n        out = out + \"${count}\" + run_char\n    out\n\nfn main(console: Console):\n    console.print(first(\"xyz\"))\n    console.print(encode(\"aaabbc\"))\n",
    );
    assert_eq!(output, vec!["x".to_string(), "3a2b1c".to_string()]);
}

#[test]
fn admitted_bool_lists_store_one_byte_elements_and_nest() {
    // `List(Bool)` elements are one-byte scalars: literal, pushed, indexed,
    // iterated, and nested in a list of lists.
    let output = run_admitted(
        "pub fn count_true(flags: List(Bool)) -> Int:\n    var n = 0\n    for f in flags:\n        if f:\n            n = n + 1\n    n\n\nfn main(console: Console):\n    var seen = [true, false, true]\n    seen.push(false)\n    seen.push(true)\n    console.print(\"${count_true(seen)} ${seen.length()} ${seen[1]} ${seen[4]}\")\n    let grid = [[true, false], [false, false]]\n    var rows = 0\n    for row in grid:\n        for cell in row:\n            if cell:\n                rows = rows + 1\n    console.print(\"${rows}\")\n",
    );
    assert_eq!(output, vec!["3 5 false true".to_string(), "1".to_string()]);
}

#[test]
fn admitted_for_over_a_loaded_row_and_a_var_copy_of_a_list_parameter_grow() {
    // A `for` over an element loaded out of a list of lists authenticates
    // that element's header itself; a `var` copy of a list parameter (a
    // pointer-bearing prefix cloned from a shared object) is grown by push
    // while the parameter keeps its length. A `for` header lane holding a
    // shared call result takes a shared phi, not the push template's unique
    // one.
    let output = run_admitted(
        "pub fn total(board: List(List(Int))) -> Int:\n    var sum = 0\n    for row in board:\n        for v in row:\n            sum = sum + v\n    sum\n\npub fn with_first(board: List(List(Int)), value: Int) -> List(List(Int)):\n    var copy = board\n    copy.push([value])\n    copy\n\nfn main(console: Console):\n    let board = [[1, 2], [3, 4]]\n    console.print(\"${total(board)}\")\n    let grown = with_first(board, 5)\n    console.print(\"${total(grown)} ${grown.length()} ${board.length()}\")\n",
    );
    assert_eq!(output, vec!["10".to_string(), "15 3 2".to_string()]);
}

#[test]
fn admitted_break_and_continue_leave_and_advance_every_loop_kind() {
    // `break` leaves through the loop's exit with the current state lanes
    // (the exit takes the header lanes' phi authority when a body breaks);
    // `continue` re-enters the header, advancing a `for` cursor or a range
    // counter. A `while true` loop reaches its exit only through `break`.
    let output = run_admitted(
        "pub fn first_big(xs: List(Int), limit: Int) -> Int:\n    var found = -1\n    for x in xs:\n        if x > limit:\n            found = x\n            break\n    found\n\npub fn evens_until(n: Int) -> List(Int):\n    var out = []\n    var i = 0\n    while true:\n        i = i + 1\n        if i > n:\n            break\n        if i % 2 == 1:\n            continue\n        out.push(i)\n    out\n\npub fn skip_odd_sum(n: Int) -> Int:\n    var sum = 0\n    for i in 0..n:\n        if i % 2 == 1:\n            continue\n        sum = sum + i\n    sum\n\nfn main(console: Console):\n    console.print(\"${first_big([1, 5, 9, 3], 4)} ${first_big([1, 2], 4)}\")\n    var rendered = \"\"\n    for e in evens_until(7):\n        rendered = rendered + \"${e},\"\n    console.print(rendered)\n    console.print(\"${skip_odd_sum(7)}\")\n",
    );
    assert_eq!(output, vec!["5 -1".to_string(), "2,4,6,".to_string(), "12".to_string()]);
}

#[test]
fn admitted_while_let_binds_nested_tuple_payloads_until_the_pattern_fails() {
    // `while let` is a loop whose header binds the scrutinee (its own lane
    // into the body) and tests the pattern; the source scrutinee and body
    // are typed directly, so the lowering reads their types. A tuple
    // payload inside the constructor pattern destructures through its slot.
    let output = run_admitted(
        "pub fn pop(xs: List(Int)) -> Option((Int, List(Int))):\n    match xs:\n        [] -> None\n        [h, ..t] -> Some((h, t))\n\nfn main(console: Console):\n    var stack = [1, 2, 3, 4]\n    var total = 0\n    while let Some((top, rest)) = pop(stack):\n        total = total + top\n        stack = rest\n    console.print(\"${total} ${stack.length()}\")\n    if let Some(x) = pop([7, 8, 9]):\n        let (head, _rest) = x\n        console.print(\"head is ${head}\")\n",
    );
    assert_eq!(output, vec!["10 0".to_string(), "head is 7".to_string()]);
}

#[test]
fn admitted_guard_near_misses_trap_exactly_at_the_boundary() {
    // B3: the last valid index reads; the index equal to the length traps
    // (a guard is a strict unsigned comparison against the logical length,
    // never the capacity); a negative index and an empty list trap too; a
    // substring end past the character count is clamped by the language's
    // rule, not by a guard.
    let output = run_admitted(
        "fn main(console: Console):\n    let xs = [10, 20, 30]\n    console.print(\"${xs[xs.length() - 1]}\")\n    var grown = xs\n    grown.push(40)\n    console.print(\"${grown[3]} ${grown.length()}\")\n    console.print(\"héllo\".substring(1, 9))\n",
    );
    assert_eq!(output, vec!["30".to_string(), "40 4".to_string(), "éllo".to_string()]);
    for (label, source) in [
        ("index equal to the length", "fn main(console: Console):\n    let xs = [10, 20, 30]\n    console.print(\"before\")\n    console.print(\"${xs[xs.length()]}\")\n"),
        ("negative index", "fn main(console: Console):\n    let xs = [10, 20, 30]\n    let i = 0 - 1\n    console.print(\"${xs[i]}\")\n"),
        ("first element of an empty list", "pub fn first(xs: List(String)) -> String:\n    xs[0]\n\nfn main(console: Console):\n    console.print(first([]))\n"),
    ] {
        // After admission the only failure mode is a Wasm trap; the runtime
        // reports it as an execution error with a backtrace.
        let error = run_admitted_result(source).expect_err(label);
        assert!(error.contains("error while executing") || error.contains("unreachable") || error.contains("trap"), "{label}: {error}");
    }
}

#[test]
fn admitted_capture_free_scalar_lambdas_are_function_values_bound_to_their_witness() {
    // A capture-free lambda over scalars lowers to a function of its own,
    // bound to its closure witness rather than a source declaration; its
    // value passes through `fn(...)` parameters and is called through the
    // authenticated table, alongside a named function used as a value.
    let output = run_admitted(
        "pub fn apply(f: fn(Int) -> Int, n: Int) -> Int:\n    f(n)\n\npub fn twice(f: fn(Int) -> Int, n: Int) -> Int:\n    f(f(n))\n\npub fn reduce(nums: List(Int), f: fn(Int, Int) -> Int, init: Int) -> Int:\n    var acc = init\n    for n in nums:\n        acc = f(acc, n)\n    acc\n\npub fn cube(n: Int) -> Int:\n    n * n * n\n\nfn main(console: Console):\n    console.print(\"${apply(fn(n: Int): n * n, 9)} ${twice(fn(n: Int): n + 3, 10)}\")\n    let nums = [1, 2, 3, 4, 5]\n    console.print(\"${reduce(nums, fn(a: Int, b: Int): a + b, 0)} ${reduce(nums, fn(a: Int, b: Int): a * b, 1)}\")\n    console.print(\"${apply(cube, 3)} ${apply(fn(n: Int): cube(n) - 1, 2)}\")\n",
    );
    assert_eq!(output, vec!["81 16".to_string(), "15 120".to_string(), "27 7".to_string()]);
}

#[test]
fn capturing_lambdas_and_reference_lambdas_are_refused_by_name() {
    // v1 has no closure environments: a lambda that captures a binding, or
    // one whose parameters or result are references, is refused at
    // canonical lowering with the construct named; nothing falls back.
    for (label, source) in [
        (
            "capture",
            "pub fn adder(k: Int) -> fn(Int) -> Int:\n    fn(x: Int): x + k\n\nfn main(console: Console):\n    console.print(\"${adder(2)(3)}\")\n",
        ),
    ] {
        let checked = witchy::resolve_std_only_checked(source).unwrap_or_else(|e| panic!("{label}: {e}"));
        match codegen::compile_checked_module_artifact(&checked) {
            codegen::LoweringOutcome::Unsupported(reason) => {
                let text = reason.to_string();
                assert!(text.contains("lambda with captures"), "{label}: {text}");
            }
            codegen::LoweringOutcome::Lowered(_) => panic!("{label}: a lambda outside the v1 subset must be refused"),
            codegen::LoweringOutcome::Rejected(error) => panic!("{label}: refused for the wrong reason: {error}"),
        }
    }
}

#[test]
fn admitted_lambdas_over_references_pass_through_indirect_calls_and_std_combinators() {
    // A capture-free lambda over strings: its shared parameters follow the
    // direct-call conventions through the indirect call (the checker's
    // move-into-shared rule), and the std combinators specialize over it.
    let output = run_admitted(
        "pub fn apply(f: fn(String) -> String, s: String) -> String:\n    f(s)\n\npub fn count_if(xs: List(String), f: fn(String) -> Bool) -> Int:\n    var n = 0\n    for x in xs:\n        if f(x):\n            n = n + 1\n    n\n\nfn main(console: Console):\n    console.print(apply(fn(s: String): s + \"!\", \"abc\"))\n    let words = [\"pear\", \"fig\", \"plum\", \"kiwi\"]\n    console.print(\"${count_if(words, fn(w: String): w.length() == 4)}\")\n    let loud = words.map(fn(w: String): w + w)\n    console.print(loud.join(\",\"))\n    let long = words.filter(fn(w: String): w.length() > 3)\n    console.print(\"${long.length()} ${long[0]}\")\n",
    );
    assert_eq!(
        output,
        vec!["abc!".to_string(), "3".to_string(), "pearpear,figfig,plumplum,kiwikiwi".to_string(), "3 pear".to_string()]
    );
}

#[test]
fn admitted_ascii_case_mapping_writes_bytes_under_a_byte_bound() {
    // `to_upper`/`to_lower` map ASCII letters over a fresh copy one byte at
    // a time (a `Size` store into a byte slot the path proves fits a byte);
    // every other byte, including multi-byte sequences, is untouched.
    let output = run_admitted(
        "fn main(console: Console):\n    let mixed = \"héLLo, wörld! 123 aZ\" + \"\"\n    console.print(mixed.to_upper())\n    console.print(mixed.to_lower())\n    console.print(\"\".to_upper() + \"|\" + \"ABC\".to_lower())\n    let words = [\"pear\", \"Fig\"]\n    console.print(words.map(fn(w: String): w.to_upper()).join(\",\"))\n",
    );
    assert_eq!(
        output,
        vec!["HéLLO, WöRLD! 123 AZ".to_string(), "héllo, wörld! 123 az".to_string(), "|abc".to_string(), "PEAR,FIG".to_string()]
    );
}

#[test]
fn admitted_nested_constructor_patterns_test_payloads_behind_the_tag() {
    // A refutable payload pattern (`Some(Less)`, `Some(Circle(r))`) loads its
    // slot only once the outer tag test selected the variant; the derived
    // `PartialOrd` on a record is exactly this shape, and a list of records
    // reads an element without a string-extent load.
    let output = run_admitted(
        "import cmp\n\ntype Version derive(PartialEq, Eq, PartialOrd, Ord):\n    major: Int\n    minor: Int\n\ntype Shape:\n    Circle(Int)\n    Square(Int)\n\nfn describe(o: Option(Ordering)) -> String:\n    match o:\n        Some(Less) -> \"less\"\n        Some(Greater) -> \"greater\"\n        Some(Equal) -> \"equal\"\n        None -> \"none\"\n\nfn area(s: Option(Shape)) -> Int:\n    match s:\n        Some(Circle(r)) -> 3 * r * r\n        Some(Square(w)) -> w * w\n        None -> 0\n\nfn top_major(xs: List(Version)) -> Int:\n    var best = xs.at(0)\n    for x in xs:\n        if x > best:\n            best = x\n    best.major\n\nfn main(console: Console):\n    console.print(describe(Version(1, 9).partial_compare(Version(2, 0))))\n    console.print(describe(Version(2, 1).partial_compare(Version(2, 0))))\n    console.print(describe(Version(2, 0).partial_compare(Version(2, 0))))\n    console.print(describe(None))\n    console.print(\"${Version(1, 12) > Version(1, 9)}\")\n    console.print(\"${area(Some(Circle(2)))} ${area(Some(Square(3)))} ${area(None)}\")\n    console.print(\"${top_major([Version(1, 9), Version(2, 0), Version(1, 12)])}\")\n",
    );
    assert_eq!(
        output,
        vec!["less", "greater", "equal", "none", "true", "12 9 0", "2"].into_iter().map(str::to_string).collect::<Vec<_>>()
    );
}

#[test]
fn admitted_record_write_back_in_a_loop_takes_a_shared_lane() {
    // A record bound from a call result (a shared value) and advanced by a
    // `var` write-back inside the loop is rebound by that call exactly as an
    // assignment rebinds it: the lane is a shared phi and the write-back's
    // unique result weakens into it on the back edge.
    let output = run_admitted(
        "type Counter:\n    n: Int\n\nfn bump(var c: Counter) -> Int:\n    c = Counter(n: c.n + 1)\n    c.n\n\nfn make() -> Counter:\n    Counter(n: 0)\n\nfn main(console: Console):\n    var c = make()\n    var i = 0\n    var total = 0\n    while i < 4:\n        total = total + bump(c)\n        i = i + 1\n    console.print(\"${total} ${c.n}\")\n    console.print([1, 2, 3].map(fn(n: Int): \"<${n}>\").join(\"\"))\n",
    );
    assert_eq!(output, vec!["10 4".to_string(), "<1><2><3>".to_string()]);
}
