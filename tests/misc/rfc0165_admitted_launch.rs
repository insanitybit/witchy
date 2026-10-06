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
fn admitted_string_literals_cross_owned_parameters_and_returns() {
    // A literal has no reference-count token; passing or returning one under
    // the owned ABI materializes a fresh copy rather than cloning static data.
    let output = run_admitted(
        "pub fn shout(name: String) -> String:\n    \"hi \" + name\n\nfn greet() -> String:\n    \"hello\"\n\nfn keep(own value: String) -> String:\n    value\n\nfn main(console: Console):\n    console.print(shout(\"x\"))\n    console.print(greet())\n    console.print(keep(\"ok\"))\n",
    );
    assert_eq!(output, vec!["hi x".to_string(), "hello".to_string(), "ok".to_string()]);
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
fn detached_bytes_cannot_claim_canonical_admission() {
    // RFC-0165 B11: the admitted bytes carry `witchy.entry`, but the section
    // is not an admission receipt. Only the artifact-typed path may use it to
    // mint and pass capability tokens.
    let checked = witchy::resolve_std_only_checked("fn main(console: Console) -> Int:\n    console.print(\"hi\")\n    40 + 2\n")
        .expect("fixture checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the foundation subset admits this fixture");
    let from_bytes = witchy_wir::canonical_entry_from_wasm(artifact.wasm())
        .expect("entry section decodes")
        .expect("a program carries an entry");
    assert_eq!(Some(&from_bytes), artifact.entry());
    let mut runtime = Runtime::batch().expect("runtime");
    let error = match runtime.spawn(
            artifact.wasm(),
            Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
            256,
        ) {
        Ok(_) => panic!("detached bytes have no admission authority"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("raw Wasm cannot claim canonical admission"));
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
fn admitted_aggregates_render_structurally_as_expected() {
    // `"${x}"` on a sum, record, tuple, or list renders structurally: a
    // sum dispatches on its tag into per-variant arms that join on string
    // lanes, a payload-free variant is its bare name, a list folds its
    // rendered elements between brackets with `, ` before every element
    // but the first, and nested aggregates recurse.
    let output = run_admitted(
        "type Shape:\n    Circle(Int)\n    Square(Int)\n    Dot\n\ntype Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let none: Option(Int) = None\n    let empty: List(Int) = []\n    let r: Result(Int, String) = Err(\"bad\")\n    let ok: Result(Int, String) = Ok(3)\n    let pair = (1, true)\n    let words: List(Option(String)) = [Some(\"a\"), None]\n    console.print(\"${Some(1)} ${none}\")\n    console.print(\"${Circle(2)} ${Dot}\")\n    console.print(\"${Point(1, 2)}\")\n    console.print(\"${pair}\")\n    console.print(\"${[1, 2, 3]} ${empty}\")\n    console.print(\"${words}\")\n    console.print(\"${[Point(1, 2), Point(3, 4)]}\")\n    console.print(\"${r} ${ok}\")\n",
    );
    assert_eq!(
        output,
        vec!["Some(1) None", "Circle(2) Dot", "Point(1, 2)", "(1, true)", "[1, 2, 3] []", "[Some(a), None]", "[Point(1, 2), Point(3, 4)]", "Err(bad) Ok(3)"]
            .into_iter().map(str::to_string).collect::<Vec<_>>()
    );
}

#[test]
fn admitted_float_render_writes_through_the_authenticated_float_to_str_import() {
    // `"${f}"` on a Float hands the authenticated `float_to_str` import a
    // fresh 32-byte buffer (a write buffer of a declared minimum size); the
    // host writes the shortest round-trip decimal and returns the length,
    // which becomes the string's logical length behind a capacity guard.
    let output = run_admitted(
        "import math\n\nfn main(console: Console):\n    let tenth = 0.1\n    console.print(\"${math.sqrt(16.0)} ${tenth + 0.2} ${2.5} ${1.0 / 4.0} ${100000.0 * 100000.0}\")\n    console.print(\"${[1.5, 2.0]} ${Some(0.25)}\")\n",
    );
    assert_eq!(output, vec!["4.0 0.30000000000000004 2.5 0.25 10000000000.0".to_string(), "[1.5, 2.0] Some(0.25)".to_string()]);
}

#[test]
fn admitted_dictionaries_are_tuples_of_two_lists_in_insertion_order() {
    // `Dict(k, v)` is its canonical representation, a tuple of the key list
    // and the value list in insertion order: `insert` searches the keys
    // (an `Int` compare or a string equality) and either overwrites the
    // value slot of a fresh copy or grows both copies, rebinding the `var`;
    // `get_or`/`at`/`contains_key`/`length`/`keys`/`values`/`pairs` read
    // through the same lists. `unique` on a `var` parameter reads as the
    // write-back convention, so the std wrappers lower.
    let output = run_admitted(
        "fn main(console: Console):\n    var counts = dict.new()\n    let _ = counts.insert(\"a\", 1)\n    let _ = counts.insert(\"b\", 2)\n    let _ = counts.insert(\"a\", counts.get_or(\"a\", 0) + 10)\n    console.print(\"${counts.length()} ${counts.get_or(\"a\", 0)} ${counts.get_or(\"zz\", -1)} ${counts.contains_key(\"b\")} ${counts.contains_key(\"c\")}\")\n    var total = 0\n    for v in counts.values():\n        total = total + v\n    for entry in counts.pairs():\n        let (k, v) = entry\n        console.print(k + \"=\" + \"${v}\")\n    console.print(\"${total} ${counts.keys()} ${counts.at(\"b\")}\")\n    var ids = dict.new()\n    let _ = ids.insert(7, 70)\n    let _ = ids.insert(8, 80)\n    console.print(\"${ids.get_or(7, 0)} ${ids.get_or(9, 0)} ${ids.length()}\")\n",
    );
    assert_eq!(
        output,
        vec!["2 11 -1 true false", "a=11", "b=2", "13 [a, b] 2", "70 0 2"].into_iter().map(str::to_string).collect::<Vec<_>>()
    );
}

#[test]
fn admitted_closures_are_records_of_code_and_int_captures() {
    // Every `fn` value is a closure record: slot 0 holds the code (a
    // function taking the record first), the remaining slots its `Int`
    // captures. A lambda allocates one with its captures' current values,
    // a named function used as a value one whose code is the function's
    // adapter, and a call through a `fn` value loads the code slot and
    // passes a shared clone of the record first (in tail position too).
    let output = run_admitted(
        "fn apply(f: fn(Int) -> Int, x: Int) -> Int:\n    f(x)\n\nfn twice(f: fn(Int) -> Int, x: Int) -> Int:\n    f(f(x))\n\nfn make_adder(n: Int) -> fn(Int) -> Int:\n    fn(x: Int): x + n\n\nfn reduce(xs: List(Int), f: fn(Int, Int) -> Int, init: Int) -> Int:\n    var acc = init\n    for x in xs:\n        acc = f(acc, x)\n    acc\n\nfn double(x: Int) -> Int:\n    x * 2\n\nfn main(console: Console):\n    let add10 = make_adder(10)\n    console.print(\"${apply(add10, 5)} ${apply(fn(x: Int): x * x, 9)} ${twice(add10, 1)}\")\n    let nums = [1, 2, 3, 4, 5]\n    console.print(\"${reduce(nums, fn(a: Int, b: Int): a + b, 0)} ${reduce(nums, fn(a: Int, b: Int): a * b, 1)}\")\n    let base = 100\n    let offset = 7\n    console.print(\"${apply(fn(x: Int): base + offset + x, 1)} ${nums.map(double)} ${nums.map(make_adder(base))}\")\n",
    );
    assert_eq!(
        output,
        vec!["15 81 21", "15 120", "108 [2, 4, 6, 8, 10] [101, 102, 103, 104, 105]"].into_iter().map(str::to_string).collect::<Vec<_>>()
    );
}

#[test]
fn admitted_unique_scalar_builders_preserve_snapshots() {
    let output = run_admitted(
        "import bytes\nimport list\nimport dict\n\nfn main(console: Console):\n    var b = bytes.with_capacity(3)\n    for i in 0..64:\n        b.push(65)\n    let old = b\n    b.push(66)\n    console.print(\"${old.length()} ${b.length()} ${old.at(63)} ${b.at(64)}\")\n    let joined = list.concat([1, 2, 3], [4, 5, 6])\n    console.print(\"${joined.length()} ${joined[5]}\")\n    let words = list.concat([\"a\", \"b\"], [\"c\"])\n    let rest = match words:\n        [head, ..tail] -> tail\n        _ -> []\n    console.print(\"${list.length(words)} ${list.length(rest)} ${rest[1]}\")\n    var entries = dict.new()\n    for i in 0..32:\n        let _ = entries.insert(i, i + 1)\n    console.print(\"${entries.length()} ${entries.get_or(31, 0)}\")\n",
    );
    assert_eq!(output, ["64 65 65 66", "6 6", "3 2 c", "32 32"].map(str::to_string));
}

#[test]
fn admitted_bytes_kernels_index_grow_slice_and_render() {
    // `Bytes` is the raw-byte buffer layout under its own source type. Its
    // intrinsics are byte kernels with the interpreter's rules: `at`,
    // `set_at` and `swap` abort out of range, `slice` clamps both ends and
    // yields empty for a start past the end, `push`/`append`/`concat` preserve
    // earlier snapshots, and a render shows only the length.
    let output = run_admitted(
        "import bytes\n\nfn main(console: Console):\n    let b = bytes.from_string(\"héllo\")\n    console.print(\"${b.length()} ${b.at(1)}\")\n    var c = b\n    c.push(33)\n    console.print(bytes.to_string(c))\n    var d = c\n    d.set_at(0, 72)\n    console.print(bytes.to_string(d))\n    var e = d\n    e.swap(0, 5)\n    console.print(bytes.to_string(e))\n    console.print(\"${e.slice(-3, 2).length()} ${e.slice(2, 100).length()} ${e.slice(5, 2).length()} ${e.slice(100, 200).length()} ${bytes.to_string(e.slice(0, 1))}\")\n    let r = bytes.repeat(65, 4)\n    let w = bytes.with_capacity(16)\n    let f: Bytes = match bytes.from_list([104, 105]):\n        Ok(v) -> v\n        Err(_) -> bytes.from_string(\"\")\n    console.print(\"${bytes.to_string(r)} ${w.length()} ${bytes.to_string(f.concat(r))} ${f} ${bytes.from_string(\"\")}\")\n    var g = f\n    g.append(r)\n    g.reserve(10)\n    console.print(\"${g.length()} ${f.get(1)} ${f.get(2)} ${f.is_empty()}\")\n",
    );
    assert_eq!(
        output,
        vec!["6 195", "héllo!", "Héllo!", "oéllH!", "2 5 0 0 o", "AAAA 0 hiAAAA Bytes(len=2) Bytes(len=0)", "6 Some(105) None false"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>()
    );
}

#[test]
fn admitted_bytes_out_of_range_index_value_and_count_trap() {
    // The interpreter aborts on an out-of-range index, a byte value outside
    // `0..=255` and a negative count; the admitted artifact traps.
    let fixtures = [
        "import bytes\n\nfn main(console: Console):\n    let b = bytes.repeat(1, 3)\n    console.print(\"${b.at(3)}\")\n",
        "import bytes\n\nfn main(console: Console):\n    let b = bytes.repeat(300, 2)\n    console.print(\"${b.length()}\")\n",
        "import bytes\n\nfn main(console: Console):\n    let b = bytes.with_capacity(0 - 1)\n    console.print(\"${b.length()}\")\n",
        "import bytes\n\nfn main(console: Console):\n    var b = bytes.repeat(1, 3)\n    b.set_at(0 - 1, 7)\n    console.print(\"${b.length()}\")\n",
    ];
    // Five independent compiles: run them in parallel.
    std::thread::scope(|scope| {
        let traps: Vec<_> = fixtures
            .into_iter()
            .map(|fixture| scope.spawn(move || assert!(run_admitted_result(fixture).is_err(), "{fixture}")))
            .collect();
        // A capacity hint is unobservable: a huge one is clamped, not an abort
        // (the interpreter clamps its own hint).
        let clamp = scope.spawn(|| {
            let output = run_admitted("import bytes\n\nfn main(console: Console):\n    let b = bytes.repeat(1, 3)\n    var c = bytes.with_capacity(9223372036854775807)\n    c.reserve(9223372036854775807)\n    console.print(\"${b.at(2)} ${b.slice(1, 3).length()} ${c.length()}\")\n");
            assert_eq!(output, vec!["1 2 0".to_string()]);
        });
        for trap in traps {
            trap.join().expect("trap fixture");
        }
        clamp.join().expect("clamp fixture");
    });
}

/// Run an admitted program with `Dir` and `Env` grants beside the console.
fn run_admitted_with_grants(source: &str, capabilities: Capabilities) -> Result<Vec<String>, String> {
    let checked = witchy::resolve_std_only_checked(source).expect("fixture checks");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("the foundation subset admits this fixture");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(&artifact, capabilities, 256)
        .map_err(|error| error.to_string())?;
    match vm.run() {
        Ok(()) => Ok(vm.output()),
        Err(error) => Err(error.to_string()),
    }
}

fn scratch_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("witchy-rfc0165-{tag}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn admitted_dir_read_exists_write_and_subtree_go_through_bounded_host_calls() {
    // A `Dir` token is an externref minted by the launcher from the grant;
    // `read` lands in a guest buffer (`to_upper` is ASCII-only, so `é` keeps its case); whose published length is bounded by
    // the buffer's own capacity, `exists`/`write`/`subtree` cross as checked
    // slices. The expected result is the directory's contents, independent
    // of either backend.
    let root = scratch_dir("dir");
    std::fs::create_dir_all(root.join("notes")).expect("subdir");
    std::fs::write(root.join("greeting.txt"), "héllo, dir").expect("seed file");
    std::fs::write(root.join("notes/todo.txt"), "a\nb\n").expect("seed file");
    let output = run_admitted_with_grants(
        "fn shout(dir: Dir[Read], name: String) -> String:\n    (dir.read(name) ?? \"\").to_upper()\n\nfn main(console: Console, dir: Dir):\n    console.print(dir.read(\"greeting.txt\") ?? \"\")\n    console.print(\"${dir.exists(\"greeting.txt\") ?? false} ${dir.exists(\"missing.txt\") ?? false} ${(dir.read(\"greeting.txt\") ?? \"\").length()}\")\n    let notes = dir.subtree(\"notes\") ?? dir\n    console.print(\"${(notes.read(\"todo.txt\") ?? \"\").length()} ${notes.exists(\"todo.txt\") ?? false}\")\n    let _ = dir.write(\"out.txt\", \"written by main\")\n    console.print(dir.read(\"out.txt\") ?? \"\")\n    console.print(shout(dir as Dir[Read], \"greeting.txt\"))\n",
        Capabilities {
            print: true,
            print_int: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            dir_write: true,
            ..Default::default()
        },
    )
    .expect("runs");
    assert_eq!(
        output,
        vec!["héllo, dir", "true false 11", "4 true", "written by main", "HéLLO, DIR"].into_iter().map(str::to_string).collect::<Vec<_>>()
    );
    assert_eq!(std::fs::read_to_string(root.join("out.txt")).expect("written"), "written by main");
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[test]
fn admitted_dir_reads_larger_than_the_probe_buffer_and_denied_paths() {
    // The first host call answers the full length into an empty buffer,
    // the second fills an exact-size buffer; a file well past any fixed
    // buffer comes back whole. A path outside the root is a `Denied` value
    // (RFC-0126 rule 3); a write through a read-only grant traps on the host.
    let root = scratch_dir("dir-large");
    let large = "x".repeat(100_000) + "end";
    std::fs::write(root.join("large.txt"), &large).expect("seed file");
    let grants = || Capabilities {
        print: true,
        print_int: true,
        quiet: true,
        dir_root: Some(root.clone()),
        dir_read: true,
        dir_write: false,
        ..Default::default()
    };
    let output = run_admitted_with_grants(
        "fn main(console: Console, dir: Dir[Read]):\n    let text = dir.read(\"large.txt\") ?? \"\"\n    console.print(\"${text.length()} ${text.substring(100000, 100003)}\")\n",
        grants(),
    )
    .expect("runs");
    assert_eq!(output, vec!["100003 end".to_string()]);
    let escape = run_admitted_with_grants(
        "fn main(console: Console, dir: Dir[Read]):\n    match dir.read(\"../escape.txt\"):\n        Ok(text) -> console.print(\"read \" + text)\n        Err(Denied(_)) -> console.print(\"denied\")\n        Err(_) -> console.print(\"other\")\n",
        grants(),
    )
    .expect("an escape is a value");
    assert_eq!(escape, vec!["denied".to_string()]);
    let write = run_admitted_with_grants(
        "fn main(console: Console, dir: Dir):\n    let _ = dir.write(\"out.txt\", \"denied\")\n    console.print(\"unreachable\")\n",
        grants(),
    );
    assert!(write.is_err(), "{write:?}");
    assert!(!root.join("out.txt").exists());
    std::fs::remove_dir_all(&root).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn admitted_dir_read_rejects_a_pipe_without_waiting_for_a_writer() {
    let root = scratch_dir("dir-pipe");
    std::fs::write(root.join("plain.txt"), "ok").expect("seed file");
    assert!(std::process::Command::new("mkfifo").arg(root.join("pipe")).status().unwrap().success());
    let output = run_admitted_with_grants(
        "fn main(console: Console, dir: Dir[Read]):\n    console.print(dir.read(\"plain.txt\") ?? \"bad\")\n    match dir.read(\"pipe\"):\n        Ok(_) -> console.print(\"bad\")\n        Err(Io(_)) -> console.print(\"io\")\n        Err(_) -> console.print(\"other\")\n",
        Capabilities {
            print: true,
            print_int: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        },
    ).expect("admitted program runs");
    assert_eq!(output, ["ok", "io"]);
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn admitted_dir_read_bounds_the_host_response_before_copying_to_the_guest() {
    let root = scratch_dir("dir-bounded");
    std::fs::write(root.join("small.txt"), "ok").expect("seed file");
    std::fs::File::create(root.join("oversized.txt"))
        .expect("create sparse file")
        .set_len((64 << 20) + 1)
        .expect("size sparse file");
    let output = run_admitted_with_grants(
        "fn main(console: Console, dir: Dir[Read]):\n    console.print(dir.read(\"small.txt\") ?? \"bad\")\n    match dir.read(\"oversized.txt\"):\n        Ok(_) -> console.print(\"bad\")\n        Err(Io(_)) -> console.print(\"io\")\n        Err(_) -> console.print(\"other\")\n",
        Capabilities {
            print: true,
            print_int: true,
            quiet: true,
            dir_root: Some(root.clone()),
            dir_read: true,
            ..Default::default()
        },
    ).expect("admitted program runs");
    assert_eq!(output, ["ok", "io"]);
    std::fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn admitted_env_get_env_answers_set_unset_and_denied_without_an_existence_oracle() {
    // RFC-0126 rule 3: one host lookup answers `Result(Option(String),
    // CapError)`. The table crosses allow-listed and not-allow-listed names
    // with set and unset host variables. A denied name answers the same
    // whether or not the host sets it, and its message names only that name.
    const SET: &str = "WITCHY_RFC0165_ENV_PROBE";
    const UNSET: &str = "WITCHY_RFC0165_ENV_UNSET";
    const DENIED_SET: &str = "WITCHY_RFC0165_ENV_DENIED_SET";
    const DENIED_UNSET: &str = "WITCHY_RFC0165_ENV_DENIED_UNSET";
    // SAFETY: the names are unique to this test and no other thread reads them.
    unsafe {
        std::env::set_var(SET, "from the host");
        std::env::remove_var(UNSET);
        std::env::set_var(DENIED_SET, "host secret");
        std::env::remove_var(DENIED_UNSET);
    }
    let source = format!(
        "fn show(console: Console, answer: Result(Option(String), CapError)):\n    match answer:\n        Ok(Some(value)) -> console.print(\"set ${{value}} ${{value.length()}}\")\n        Ok(None) -> console.print(\"unset\")\n        Err(Denied(message)) -> console.print(\"denied ${{message}}\")\n        Err(_) -> console.print(\"other\")\n\nfn main(console: Console, env: Env):\n    for name in [\"{SET}\", \"{UNSET}\", \"{DENIED_SET}\", \"{DENIED_UNSET}\"]:\n        show(console, env.get_env(name))\n"
    );
    let output = run_admitted_with_grants(
        &source,
        Capabilities {
            print: true,
            print_int: true,
            quiet: true,
            env: true,
            env_allow: Some(vec![SET.into(), UNSET.into()]),
            ..Default::default()
        },
    )
    .expect("a denied name is a value, not a trap");
    assert_eq!(
        output,
        vec![
            "set from the host 13".to_string(),
            "unset".to_string(),
            format!("denied `{DENIED_SET}` is not covered by this Env capability"),
            format!("denied `{DENIED_UNSET}` is not covered by this Env capability"),
        ]
    );
    // No allow-listed name or host value crosses in a denial.
    for line in &output[2..] {
        for leaked in [format!("`{SET}`"), format!("`{UNSET}`"), "host secret".to_string()] {
            assert!(!line.contains(&leaked), "{line} leaks {leaked}");
        }
    }
}

#[test]
fn admitted_entry_without_a_dir_or_env_grant_is_refused_before_it_runs() {
    let refused = run_admitted_with_grants(
        "fn main(console: Console, dir: Dir[Read]):\n    console.print(\"${dir.exists(\"x\") ?? false}\")\n",
        Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
    );
    assert!(refused.is_err(), "{refused:?}");
    let refused = run_admitted_with_grants(
        "fn main(console: Console, env: Env):\n    match env.get_env(\"HOME\"):\n        Ok(_) -> console.print(\"answered\")\n        Err(_) -> console.print(\"failed\")\n",
        Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
    );
    assert!(refused.is_err(), "{refused:?}");
}

#[test]
fn admitted_a_list_rebound_after_an_earlier_loop_crosses_the_next_loop_header() {
    // An earlier loop leaves a list it does not carry without extent facts;
    // a later loop that rebinds it must authenticate its header first, or
    // the copying update arrives on the back edge as a new identity.
    let output = run_admitted(
        "fn main(console: Console):\n    var xs = [1, 2, 3]\n    var total = 0\n    for i in 0..3:\n        total = total + i\n    for j in 0..3:\n        xs.set_at(j, j * 10)\n    console.print(\"${xs[0]} ${xs[1]} ${xs[2]} ${total}\")\n",
    );
    assert_eq!(output, vec!["0 10 20 3".to_string()]);
}

#[test]
fn admitted_equal_length_lists_keep_their_own_extent_lanes_in_nested_loops() {
    // Two lists of the same constant length cross a shared phi. Each bound
    // must rename to its own lane, not the first lane holding an equal
    // constant, or the inner loop finds one list on the other's extent.
    let output = run_admitted(
        "fn bump(n: Int) -> Int:\n    var a = [1, 2, 3, 4]\n    var c = [5, 5, 5, 5]\n    while true:\n        var advanced = false\n        while !advanced:\n            a.set_at(n, 7)\n            c.set_at(n, 8)\n            advanced = true\n        return a[1] + c[1] + a[0] + c[0]\n    0\n\nfn main(console: Console):\n    console.print(\"${bump(1)}\")\n",
    );
    assert_eq!(output, vec!["21".to_string()]);
}

#[test]
fn admitted_a_loop_in_a_branch_keeps_the_next_loop_body_extent() {
    // A loop inside a branch reloads a list's header; the following loop
    // body receives that reloaded length, and the checker must rename the
    // body lane's bounds through the same header equality.
    let output = run_admitted(
        "fn decrement(n: Int) -> Int:\n    var count = [0, 0, 0]\n    var r = 1\n    while true:\n        if n > 5:\n            for j in 0..n:\n                r = j % 2\n        var advanced = false\n        while !advanced:\n            count.set_at(r, count[r] - 1)\n            advanced = true\n        return count[0] * 10 + count[1]\n    0\n\nfn main(console: Console):\n    console.print(\"${decrement(3)} ${decrement(7)}\")\n",
    );
    assert_eq!(output, vec!["-1 -10".to_string()]);
}

#[test]
fn admitted_a_recursive_builder_of_a_task_free_type_has_no_callable_obligation() {
    // A recursive call's result summary nests one level per fixpoint round.
    // A result type that reaches no task or callable carries no obligation,
    // so the depth budget must not read the nesting as one.
    let output = run_admitted(
        "type Tree:\n    Leaf\n    Node(Tree, Tree)\n\nfn build(depth: Int) -> Tree:\n    if depth == 0:\n        Node(Leaf, Leaf)\n    else:\n        Node(build(depth - 1), build(depth - 1))\n\nfn count(t: Tree) -> Int:\n    match t:\n        Leaf -> 0\n        Node(l, r) -> 1 + count(l) + count(r)\n\nfn main(console: Console):\n    console.print(\"${count(build(3))}\")\n",
    );
    assert_eq!(output, vec!["15".to_string()]);
}

#[test]
fn admitted_a_recursive_builder_under_an_async_main_has_no_callable_obligation() {
    // An async program also exports task behavior summaries; the same
    // task-free result must not nest there until the budget rejects it.
    let output = run_admitted(
        "type Tree:\n    Leaf\n    Node(Tree, Tree)\n\nfn build(depth: Int) -> Tree:\n    if depth == 0:\n        Node(Leaf, Leaf)\n    else:\n        Node(build(depth - 1), build(depth - 1))\n\nfn count(t: Tree) -> Int:\n    match t:\n        Leaf -> 0\n        Node(l, r) -> 1 + count(l) + count(r)\n\nasync fn main(console: Console):\n    console.print(\"${count(build(3))}\")\n",
    );
    assert_eq!(output, vec!["15".to_string()]);
}
