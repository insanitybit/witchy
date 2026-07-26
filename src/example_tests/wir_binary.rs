use super::*;
use crate::{codegen, parser, typeck};

    /// RFC-0011: the raw-string `restrict` builtin is RETIRED. Address narrowing now goes
    /// only through the typed `net.only(Net...)` verb; a raw `host:port` string survives
    /// solely as a `--net`/config grant, not a language builtin. Both the free `restrict(net,
    /// …)` and the method `net.restrict(…)` forms are rejected — there is no such verb.
    #[test]
    fn retired_restrict_builtin_is_rejected() {
        assert!(
            typeck::check_str("fn main(net: Net):\n    let r = restrict(net, \"a:1\")\n").is_err(),
            "the free `restrict` builtin must be rejected after retirement",
        );
        assert!(
            typeck::check_str("fn main(net: Net):\n    let r = net.restrict(\"a:1\")\n").is_err(),
            "the `net.restrict` method form must be rejected after retirement",
        );
    }

    /// `witchy emit-wat <file>` compiles a program to WebAssembly text — the same
    /// module `sandbox` runs — for inspecting the generated code.
    #[test]
    fn emit_wat_returns_the_compiled_module() {
        let path = std::env::temp_dir().join(format!("witchy_emit_wat_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "fn fib(n: Int) -> Int:\n    if n < 2:\n        n\n    else:\n        fib(n - 1) + fib(n - 2)\nfn main(console: Console):\n    console.print(\"${fib(10)}\")\n",
        )
        .expect("write temp source");
        let wat = crate::emit_wat_file(path.to_str().unwrap()).expect("emit-wat");
        let _ = std::fs::remove_file(&path);
        assert!(wat.starts_with("(module"), "expected a wasm module, got: {}", &wat[..wat.len().min(40)]);
        // The fib function is emitted, module-qualified by the file stem.
        assert!(wat.contains(".fib (param $n i64)"), "expected the fib function in the WAT");
    }

    /// An `var` fn with an EARLY `return` on the binary path: the return must
    /// yield the full multi-result tuple (the declared value, then each var
    /// param's final value) so the arity matches the move-out ABI — a single
    /// `N::Return` would mismatch and the whole module bailed to WAT. `clamp`
    /// returns early when `n > 10`; both the early and fall-through exits write
    /// `n` back into the caller's variable.
    #[test]
    fn wir_var_early_return_binary_path() {
        let src = "fn clamp(var n: Int):\n    if (n > 10):\n        n = 10\n        return\n    n = n + 1\n\nfn main(console: Console):\n    var a = 5\n    clamp(a)\n    console.print(\"${a}\")\n    var b = 50\n    clamp(b)\n    console.print(\"${b}\")\n";
        let want = vec!["6".to_string(), "10".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower an var fn with an early return");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// Criterion-2: the slot-elimination pass shows a MEASURABLE improvement on a
    /// real lowered program. `[list.at(xs, 0)]` (with `xs: List(Bool)`) reads an
    /// i64 slot, narrows it to the bool's i32, then re-widens it to store in the
    /// new list — a redundant `ToSlot(FromSlot(..))` the pass removes. The
    /// optimized binary still runs identically to the interpreter oracle.
    #[test]
    fn wir_slot_elimination_shows_measurable_improvement() {
        let src = "fn main(console: Console):\n    let xs = [true, false]\n    let ys = [list.at(xs, 0)]\n    if list.at(ys, 0):\n        console.print(\"t\")\n    else:\n        console.print(\"f\")\n";
        let want = vec!["t".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let m = codegen::assemble_wir_module(&linked)
            .expect_lowered("program takes the WIR binary path");
        // Measurable: the pass removes redundant slot conversions.
        let mut opt_m = m.clone();
        let stats = witchy_wir::wir_opt::optimize(&mut opt_m);
        assert!(
            stats.eliminated > 0,
            "expected the slot-elimination pass to remove nodes, eliminated={}",
            stats.eliminated
        );
        // Oracle-validated: both the unoptimized and optimized binaries match the
        // interpreter (a behavior-preserving win, not a behavior change).
        assert_eq!(run_bytes_print_only(&crate::wir_encode::encode(&m, &[])), want, "unoptimized");
        assert_eq!(run_bytes_print_only(&crate::wir_encode::encode(&opt_m, &[])), want, "optimized");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// The `wir_opt` slot-elimination pass is a SOUND, behavior-preserving
    /// rewrite: for every lowering-subset program, the unoptimized and optimized
    /// binaries both run identically to the interpreter oracle. (Node-count
    /// reduction is unit-tested in `wir_opt` on synthetic `FromSlot(ToSlot)`
    /// redundancy; the current lowering emits no such round-trips — those arise
    /// at generic/monomorphization boundaries that do not lower yet — so
    /// `eliminated` is 0 on these real programs. The measurable payoff lands when
    /// that lowering does, producing the redundancy the pass removes.)
    #[test]
    fn wir_slot_elimination_is_behavior_preserving() {
        let progs = [
            "fn main(console: Console):\n    console.print(\"hi\")\n",
            "fn inc(n: Int) -> Int:\n    n + 1\n\nfn main(console: Console):\n    if inc(inc(0)) > 1:\n        console.print(\"ok\")\n    else:\n        console.print(\"no\")\n",
            "fn classify(n: Int) -> Bool:\n    match n:\n        0 -> true\n        _ -> false\n\nfn main(console: Console):\n    if classify(0):\n        console.print(\"zero\")\n    else:\n        console.print(\"nonzero\")\n",
        ];
        for src in progs {
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            let m = codegen::assemble_wir_module(&linked)
                .expect_lowered(&format!("expected the WIR binary path to handle:\n{src}"));
            let oracle = link_run(src);
            // Unoptimized encoding runs like the oracle...
            let unopt = crate::wir_encode::encode(&m, &[]);
            assert_eq!(run_bytes_all_caps(&unopt), oracle, "unoptimized:\n{src}");
            // ...and the optimized encoding runs identically (sound rewrite).
            let mut opt_m = m.clone();
            let stats = witchy_wir::wir_opt::optimize(&mut opt_m);
            assert!(stats.nodes_after <= stats.nodes_before, "the pass never grows the tree");
            let opt = crate::wir_encode::encode(&opt_m, &[]);
            assert_eq!(run_bytes_all_caps(&opt), oracle, "optimized:\n{src}");
        }
    }

    /// M3 sink-flip: the WIR→binary path (`compile_module_binary`, NO
    /// `wat::parse_str`) must, for every program whose whole module lowers,
    /// assemble a VALID wasm module that runs identically to the interpreter
    /// oracle and to the legacy WAT path. Programs are chosen from the lowering
    /// subset (string literals + control flow + scalar helpers; no list-building,
    /// string concat, generated render, or Int/Float `main` yet).
    #[test]
    fn wir_binary_path_runs_and_agrees_with_oracle() {
        let cases: &[(&str, Vec<String>)] = &[
            (
                "fn main(console: Console):\n    console.print(\"hello from WIR\")\n",
                vec!["hello from WIR".to_string()],
            ),
            (
                "fn main(console: Console):\n    console.print(\"one\")\n    console.print(\"two\")\n",
                vec!["one".to_string(), "two".to_string()],
            ),
            (
                "fn main(console: Console):\n    if true:\n        console.print(\"yes\")\n    else:\n        console.print(\"no\")\n",
                vec!["yes".to_string()],
            ),
            (
                "fn pick(b: Bool) -> Bool:\n    b\n\nfn main(console: Console):\n    if pick(true):\n        console.print(\"picked\")\n    else:\n        console.print(\"nope\")\n",
                vec!["picked".to_string()],
            ),
            // An aggregate: builds a tuple ($mk2 → $ensure) and destructures it —
            // exercises the migrated allocator helpers on the pruned binary path.
            (
                "fn main(console: Console):\n    let t = (1, 2)\n    let (a, b) = t\n    if a < b:\n        console.print(\"ordered\")\n    else:\n        console.print(\"no\")\n",
                vec!["ordered".to_string()],
            ),
            // A list with indexing ($mk3 → $ensure, $list_at) on the binary path.
            (
                "fn main(console: Console):\n    let xs = [10, 20, 30]\n    if list.at(xs, 1) == 20:\n        console.print(\"twenty\")\n    else:\n        console.print(\"no\")\n",
                vec!["twenty".to_string()],
            ),
            // Integer rendering ($int_to_string → $ensure) on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${42}\")\n    console.print(\"${-7}\")\n",
                vec!["42".to_string(), "-7".to_string()],
            ),
            // String content equality ($str_eq) on the binary path.
            (
                "fn main(console: Console):\n    if \"abc\" == \"abc\":\n        console.print(\"eq\")\n    else:\n        console.print(\"ne\")\n    if \"abc\" == \"xyz\":\n        console.print(\"eq2\")\n    else:\n        console.print(\"ne2\")\n",
                vec!["eq".to_string(), "ne2".to_string()],
            ),
            // String concatenation ($concat → $ensure) on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"hello, \" + \"world\")\n    console.print(\"x\" + \"y\" + \"z\")\n",
                vec!["hello, world".to_string(), "xyz".to_string()],
            ),
            // list.length on the binary path.
            (
                "fn main(console: Console):\n    let xs = [10, 20, 30]\n    console.print(\"${list.length(xs)}\")\n",
                vec!["3".to_string()],
            ),
            // string.length on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${\"hello\".length()}\")\n",
                vec!["5".to_string()],
            ),
            // string.contains ($find_byte — a conditional br inside a loop) on
            // the binary path.
            (
                "fn main(console: Console):\n    if \"hello\".contains(\"ell\"):\n        console.print(\"yes\")\n    else:\n        console.print(\"no\")\n    if \"hello\".contains(\"xyz\"):\n        console.print(\"yes2\")\n    else:\n        console.print(\"no2\")\n",
                vec!["yes".to_string(), "no2".to_string()],
            ),
            // string.starts_with ($starts_with — prefix byte-compare loop) on
            // the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${\"hello\".starts_with(\"hel\")}\")\n    console.print(\"${\"hello\".starts_with(\"lo\")}\")\n    console.print(\"${\"hello\".starts_with(\"\")}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string()],
            ),
            // string.ends_with ($ends_with — suffix byte-compare loop) on the
            // binary path.
            (
                "fn main(console: Console):\n    console.print(\"${\"hello\".ends_with(\"llo\")}\")\n    console.print(\"${\"hello\".ends_with(\"hel\")}\")\n    console.print(\"${\"hello\".ends_with(\"\")}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string()],
            ),
            // string.substring ($str_substring → $char_to_byte + $substr, a
            // heap-allocating slice) on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"hello world\".substring(0, 5))\n    console.print(\"hello world\".substring(6, 11))\n",
                vec!["hello".to_string(), "world".to_string()],
            ),
            // string.trim ($trim → $is_ws + $substr, two whitespace scan loops)
            // on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"  hi  \".trim())\n    console.print(\"abc\".trim())\n",
                vec!["hi".to_string(), "abc".to_string()],
            ),
            // string.split ($split → $substr + $list_push, nested scan/compare
            // loops building a List(String)) on the binary path; indexed with
            // the already-migrated $list_at.
            (
                "fn main(console: Console):\n    let parts = \"a,b,c\".split(\",\")\n    console.print(list.at(parts, 0))\n    console.print(list.at(parts, 1))\n    console.print(list.at(parts, 2))\n",
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ),
            // for-loop over a list with an arena-resettable body (the watermark
            // optimization, ported to WIR): per-iteration `$heap` save/restore.
            (
                "fn main(console: Console):\n    for piece in \"a,b,c\".split(\",\"):\n        console.print(piece)\n",
                vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ),
            // range for-loop whose body allocates per iteration (nothing escapes,
            // so it's watermarked) — exercises the range-for arena reset on WIR.
            (
                "fn main(console: Console):\n    for i in 0..3:\n        console.print(\"abcdef\".substring(i, i + 2))\n",
                vec!["ab".to_string(), "bc".to_string(), "cd".to_string()],
            ),
            // while-loop with an arena-resettable allocating body (the watermark
            // now ported to WIR for `while` too).
            (
                "fn main(console: Console):\n    var i: Int = 0\n    while i < 3:\n        console.print(\"abcdef\".substring(i, i + 2))\n        i = i + 1\n",
                vec!["ab".to_string(), "bc".to_string(), "cd".to_string()],
            ),
            // match on an ADT constructor with a payload bind (Some(n)) / a
            // nullary variant (None) — the new lower_pattern Ctor arm.
            (
                "fn pick(b: Bool) -> Option(Int):\n    if b:\n        Some(7)\n    else:\n        None\n\nfn main(console: Console):\n    console.print(\"${match pick(true):\n        Some(n) -> n\n        None -> 99}\")\n    console.print(\"${match pick(false):\n        Some(n) -> n\n        None -> 99}\")\n",
                vec!["7".to_string(), "99".to_string()],
            ),
            // match on string-literal patterns (str_eq) with a wildcard fallback.
            (
                "fn classify(s: String) -> Int:\n    match s:\n        \"yes\" -> 1\n        \"no\" -> 0\n        _ -> 9\n\nfn main(console: Console):\n    console.print(\"${classify(\"yes\")}\")\n    console.print(\"${classify(\"no\")}\")\n    console.print(\"${classify(\"maybe\")}\")\n",
                vec!["1".to_string(), "0".to_string(), "9".to_string()],
            ),
            // match with a LITERAL constructor field (Some(0)) — the short-circuit
            // `if tag == Some: field == 0` path of the Ctor pattern arm.
            (
                "fn check(o: Option(Int)) -> Int:\n    match o:\n        Some(0) -> 100\n        Some(n) -> n\n        None -> 99\n\nfn main(console: Console):\n    console.print(\"${check(Some(0))}\")\n    console.print(\"${check(Some(5))}\")\n    console.print(\"${check(None)}\")\n",
                vec!["100".to_string(), "5".to_string(), "99".to_string()],
            ),
            // list patterns: empty, exact-length head bind, and a `[h, ..t]` tail
            // bind (via $list_drop).
            (
                "fn sum_head(xs: List(Int)) -> Int:\n    match xs:\n        [] -> 0\n        [a, b] -> a + b\n        [h, ..t] -> h + list.length(t)\n        _ -> 99\n\nfn main(console: Console):\n    console.print(\"${sum_head([])}\")\n    console.print(\"${sum_head([10, 20])}\")\n    console.print(\"${sum_head([5, 1, 2, 3])}\")\n",
                vec!["0".to_string(), "30".to_string(), "8".to_string()],
            ),
            // structural `==` on scalar-field compounds: a tuple, a list, and a
            // tuple with a String field ($str_eq). Distinct literals so a stray
            // pointer-compare would diverge from the structural result.
            (
                "fn main(console: Console):\n    console.print(\"${(1, 2) == (1, 2)}\")\n    console.print(\"${(1, 2) == (1, 3)}\")\n    console.print(\"${[1, 2, 3] == [1, 2, 3]}\")\n    console.print(\"${[1, 2] == [1, 9]}\")\n    console.print(\"${(\"a\", 1) == (\"a\", 1)}\")\n    console.print(\"${(\"a\", 1) == (\"b\", 1)}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "false".to_string(), "true".to_string(), "false".to_string()],
            ),
            // NESTED structural `==`: a list of tuples and a tuple of (tuple, int)
            // — slot_cmp_wir recurses into the field shapes' eq helpers.
            (
                "fn main(console: Console):\n    console.print(\"${[(1, 2), (3, 4)] == [(1, 2), (3, 4)]}\")\n    console.print(\"${[(1, 2)] == [(1, 9)]}\")\n    console.print(\"${((1, 2), 3) == ((1, 2), 3)}\")\n    console.print(\"${((1, 2), 3) == ((1, 9), 3)}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "false".to_string()],
            ),
            // Structural render of compounds (the $ts renderer): a tuple, a tuple
            // with a String + Bool field, and a list — built with $concat/
            // $int_to_string.
            (
                "fn main(console: Console):\n    console.print(\"${(1, 2)}\")\n    console.print(\"${(\"hi\", true)}\")\n    console.print(\"${[1, 2, 3]}\")\n    console.print(\"${[true, false]}\")\n",
                vec!["(1, 2)".to_string(), "(hi, true)".to_string(), "[1, 2, 3]".to_string(), "[true, false]".to_string()],
            ),
            // a record: structural `==` (eq helper) and render (ts helper,
            // `Name(f0, f1)`) on the binary path.
            (
                "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    console.print(\"${Point(1, 2)}\")\n    console.print(\"${Point(1, 2) == Point(1, 2)}\")\n    console.print(\"${Point(1, 2) == Point(1, 9)}\")\n",
                vec!["Point(1, 2)".to_string(), "true".to_string(), "false".to_string()],
            ),
            // a tuple with a Float field renders via $float_to_str (host import).
            (
                "fn main(console: Console):\n    console.print(\"${(1.5, 2)}\")\n",
                vec!["(1.5, 2)".to_string()],
            ),
            // a closure: a lambda bound to a local, then called (the lifted body +
            // closure object + call_indirect on the binary path).
            (
                "fn main(console: Console):\n    let f = fn(n: Int): n + 1\n    console.print(\"${f(5)}\")\n    console.print(\"${f(10)}\")\n",
                vec!["6".to_string(), "11".to_string()],
            ),
            // string.chars ($str_chars → $byte_to_char + $str_substring +
            // $list_push) splitting a multibyte string into a List(String).
            (
                "fn main(console: Console):\n    let cs = \"héllo\".chars()\n    console.print(list.at(cs, 0))\n    console.print(list.at(cs, 1))\n    console.print(list.at(cs, 4))\n",
                vec!["h".to_string(), "é".to_string(), "o".to_string()],
            ),
            // list.concat ($list_concat — two memory.copy's into a fresh slot
            // array) on the binary path.
            (
                "fn main(console: Console):\n    let xs = list.concat([10, 20], [30, 40])\n    console.print(\"${list.at(xs, 0)}\")\n    console.print(\"${list.at(xs, 2)}\")\n    console.print(\"${list.at(xs, 3)}\")\n",
                vec!["10".to_string(), "30".to_string(), "40".to_string()],
            ),
            // string.to_upper / to_lower ($ascii_case byte transform) on the
            // binary path.
            (
                "fn main(console: Console):\n    console.print(\"Hello, World!\".to_upper())\n    console.print(\"Hello, World!\".to_lower())\n",
                vec!["HELLO, WORLD!".to_string(), "hello, world!".to_string()],
            ),
            // string.to_int ($str_to_int — whitespace/sign/overflow-checked parse)
            // on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${\"123\".to_int() + \"-23\".to_int()}\")\n",
                vec!["100".to_string()],
            ),
            // string.replace ($replace + $match_at — count-then-fill) on the
            // binary path, including a growing replacement.
            (
                "fn main(console: Console):\n    console.print(\"hello world\".replace(\"o\", \"0\"))\n    console.print(\"a.b.c\".replace(\".\", \"::\"))\n",
                vec!["hell0 w0rld".to_string(), "a::b::c".to_string()],
            ),
            // dict with String keys ($dict_new/insert/get_or/has/size →
            // $dict_find + $key_eq's $str_eq path) on the binary path.
            (
                "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n    console.print(\"${dict.get_or(d, \"a\", 0)}\")\n    console.print(\"${dict.get_or(d, \"z\", 99)}\")\n    console.print(\"${dict.contains_key(d, \"b\")}\")\n    console.print(\"${dict.contains_key(d, \"z\")}\")\n    console.print(\"${dict.length(d)}\")\n",
                vec!["1".to_string(), "99".to_string(), "true".to_string(), "false".to_string(), "2".to_string()],
            ),
            // dict iteration + remove ($dict_keys/values/pairs/remove). Asserts
            // order-independent facts (lengths, post-remove membership) so it's
            // robust to entry ordering.
            (
                "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n    console.print(\"${list.length(dict.keys(d))}\")\n    console.print(\"${list.length(dict.values(d))}\")\n    console.print(\"${list.length(dict.pairs(d))}\")\n    var d2 = d\n    dict.remove(d2, \"a\")\n    console.print(\"${dict.length(d2)}\")\n    console.print(\"${dict.contains_key(d2, \"a\")}\")\n    console.print(\"${dict.contains_key(d2, \"b\")}\")\n",
                vec!["2".to_string(), "2".to_string(), "2".to_string(), "1".to_string(), "false".to_string(), "true".to_string()],
            ),
            // a capturing closure: the lambda closes over `k` (an Int local),
            // recovered from the env at offset 4 on the binary path.
            (
                "fn main(console: Console):\n    let k = 10\n    let g = fn(n: Int): n + k\n    console.print(\"${g(5)}\")\n    console.print(\"${g(0)}\")\n",
                vec!["15".to_string(), "10".to_string()],
            ),
            // a closure passed to a user function and called through its
            // fn-typed param (`f(f(x))` — the closure-typed-local call_indirect).
            (
                "fn apply_twice(f: fn(Int) -> Int, x: Int) -> Int:\n    f(f(x))\nfn main(console: Console):\n    let k = 10\n    let g = fn(n: Int): n + k\n    console.print(\"${apply_twice(g, 1)}\")\n",
                vec!["21".to_string()],
            ),
            // short-circuit `&&`/`||` lower to a value-`If` on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${true && false}\")\n    console.print(\"${true || false}\")\n    console.print(\"${1 < 2 && 3 < 4}\")\n    console.print(\"${1 > 2 || 3 < 4}\")\n",
                vec!["false".to_string(), "true".to_string(), "true".to_string(), "true".to_string()],
            ),
            // `&&` must short-circuit: the RHS index would be out of bounds when the
            // LHS guard (`i < n`) is false, so it must NOT be evaluated.
            (
                "fn main(console: Console):\n    let xs = [10, 20]\n    let n = list.length(xs)\n    var i = 0\n    var sum = 0\n    while i < n && list.at(xs, i) > 0:\n        sum = sum + list.at(xs, i)\n        i = i + 1\n    console.print(\"${sum}\")\n",
                vec!["30".to_string()],
            ),
            // float ordering (`<`/`<=`/`>`/`>=`) lowers to the NaN-trapping
            // `$f_lt`/`$f_le`/`$f_gt`/`$f_ge` helpers on the binary path.
            (
                "fn main(console: Console):\n    console.print(\"${1.5 < 2.5}\")\n    console.print(\"${2.5 <= 2.5}\")\n    console.print(\"${3.5 > 2.5}\")\n    console.print(\"${1.5 >= 2.5}\")\n",
                vec!["true".to_string(), "true".to_string(), "true".to_string(), "false".to_string()],
            ),
            // string ordering (`<`/`<=`/`>`/`>=`) lowers to `$str_cmp` sign
            // compares — lexicographic, including the prefix tie-break by length.
            (
                "fn main(console: Console):\n    console.print(\"${\"abc\" < \"abd\"}\")\n    console.print(\"${\"abc\" < \"ab\"}\")\n    console.print(\"${\"abc\" <= \"abc\"}\")\n    console.print(\"${\"b\" > \"abc\"}\")\n    console.print(\"${\"abc\" >= \"abd\"}\")\n",
                vec!["true".to_string(), "false".to_string(), "true".to_string(), "true".to_string(), "false".to_string()],
            ),
            // a string accumulator (`s = s + ..`) — a self-assign whose in-place fast
            // path is list-only — lowers as a plain value-rebind (the `list.join`
            // shape that blocked ~20 programs). The if/else picks first vs separator.
            (
                "fn main(console: Console):\n    var s = \"\"\n    var first = true\n    for w in [\"a\", \"b\", \"c\"]:\n        if first:\n            s = w\n            first = false\n        else:\n            s = s + \"-\" + w\n    console.print(s)\n",
                vec!["a-b-c".to_string()],
            ),
            // `string.char_count` (Unicode scalars, not bytes) via the `$char_count`
            // → `$byte_to_char` helper — the blocker for parse_int/pad_*.
            (
                "fn main(console: Console):\n    console.print(\"${\"abc\".char_count()}\")\n    console.print(\"${\"héllo\".char_count()}\")\n",
                vec!["3".to_string(), "5".to_string()],
            ),
            // Int<->Float numeric conversions + sqrt (the new `ToFloat`/`ToInt`/`Sqrt`
            // UnOps) and scalar Float render (via `$float_to_str`).
            (
                "fn main(console: Console):\n    console.print(\"${math.to_int(math.sqrt(16.0))}\")\n    console.print(\"${math.to_int(math.to_float(7) + 0.5)}\")\n    console.print(\"${3.5}\")\n",
                vec!["4".to_string(), "7".to_string(), "3.5".to_string()],
            ),
            // `string.from_code` (Unicode scalar -> single-char string) via the
            // `$string_from_code` host-import wrapper.
            (
                "fn main(console: Console):\n    console.print(string.from_code(65))\n    console.print(string.from_code(233))\n",
                vec!["A".to_string(), "é".to_string()],
            ),
            // a closure bound from a MATCH pattern then called (`Box(f) -> f(x)`) —
            // the `iter.next` shape (`Iter(thunk) -> thunk()`). Now lowers since a
            // local in call position is always a closure (the guard is just `locals`).
            (
                "type Box:\n    Box(fn(Int) -> Int)\nfn apply(b: Box, x: Int) -> Int:\n    match b:\n        Box(f) -> f(x)\nfn main(console: Console):\n    let b = Box(fn(n: Int): n + 1)\n    console.print(\"${apply(b, 5)}\")\n",
                vec!["6".to_string()],
            ),
            // nested lambdas: an outer lambda built inside another function's body,
            // with two instances in a list — exercises the lifted-lambda index/name
            // fix (a nested lambda lowered during the outer's build must not collide
            // on the outer's table slot).
            (
                "type Adder:\n    Adder(fn(Int) -> Int)\nfn make(base: Int) -> Adder:\n    Adder(fn(x: Int): x + base)\nfn run(a: Adder, v: Int) -> Int:\n    match a:\n        Adder(f) -> f(v)\nfn main(console: Console):\n    let pair = [make(10), make(100)]\n    console.print(\"${run(list.at(pair, 0), 5)}\")\n    console.print(\"${run(list.at(pair, 1), 5)}\")\n",
                vec!["15".to_string(), "105".to_string()],
            ),
            // a bare top-level function name passed as a VALUE to a higher-order fn —
            // materialized as a forwarding closure `fn(p): is_odd(p)`.
            (
                "fn is_odd(n: Int) -> Bool:\n    n % 2 == 1\nfn count_if(xs: List(Int), pred: fn(Int) -> Bool) -> Int:\n    var c = 0\n    for x in xs:\n        if pred(x):\n            c = c + 1\n    c\nfn main(console: Console):\n    console.print(\"${count_if([1, 2, 3, 4, 5], is_odd)}\")\n",
                vec!["3".to_string()],
            ),
            // a `region:` block — a scalar result (reclaimed by stashing the value in
            // a register and resetting `$heap`) and a `List(Int)` result (reclaimed via
            // the generated `$rcopy_list_int`: scalar payload, one `memory.copy`).
            (
                "fn main(console: Console):\n    let s = region -> Int:\n        var sum = 0\n        for i in 0..10:\n            sum = sum + i\n        sum\n    console.print(\"${s}\")\n    let xs = region -> List(Int):\n        var ys = []\n        for i in 0..5:\n            list.push(ys, i * i)\n        ys\n    console.print(\"${list.at(xs, 3)}\")\n",
                vec!["45".to_string(), "9".to_string()],
            ),
            // a `region -> (Int, String):` tuple — the generated `$rcopy_tuple_*`
            // copies the tag, the scalar slot verbatim, and recurses through
            // `$rcopy_str` for the string slot. The biased copy-out keeps `t.1`
            // pointing at the reclaimed string; `after` reuses the freed space.
            (
                "fn main(console: Console):\n    let t = region -> (Int, String):\n        var acc = \"\"\n        for i in 0..3:\n            acc = acc + \"z\"\n        (7 * 6, acc)\n    let after = \"OK\"\n    console.print(\"${t}\")\n    console.print(t.1)\n    console.print(after)\n",
                vec!["(42, zzz)".to_string(), "zzz".to_string(), "OK".to_string()],
            ),
            // a `region -> List(String):` — a list with a COMPOUND payload: the
            // generated `$rcopy_list_str` writes the length header then deep-copies
            // each element string through `$rcopy_str`, so every slot holds a biased
            // pointer into the reclaimed block.
            (
                "fn main(console: Console):\n    let xs = region -> List(String):\n        var ys = []\n        for i in 0..3:\n            list.push(ys, \"n\" + \"${i}\")\n        ys\n    let after = \"OK\"\n    console.print(list.at(xs, 0))\n    console.print(list.at(xs, 2))\n    console.print(after)\n",
                vec!["n0".to_string(), "n2".to_string(), "OK".to_string()],
            ),
            // enum/record structural render: the generated `$ts_*` tag-dispatch
            // helper emits `Name` (nullary), `Name(f0, f1, ...)` (fields), and a
            // record positionally (`Point(5, 6)`), matching the interpreter's
            // `Value::Ctor` Display. Unlike enum `==`, the WAT path renders enums
            // structurally too, so all three agree.
            (
                "type Color:\n    Red\n    Green\n    RGB(Int, Int, Int)\n\ntype Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let c = RGB(1, 2, 3)\n    let p = Point(x: 5, y: 6)\n    let g = Green\n    console.print(\"${c}\")\n    console.print(\"${p}\")\n    console.print(\"${g}\")\n",
                vec!["RGB(1, 2, 3)".to_string(), "Point(5, 6)".to_string(), "Green".to_string()],
            ),
            // Render of an INLINE call result (`"${mklist()}"`) — the shape comes
            // from typeck's type table (eq_operand_shape), not just tracked locals,
            // so a compound expression renders without being bound to a `let` first.
            (
                "fn mklist() -> List(Int):\n    [1, 2, 3]\n\nfn pair() -> (Int, String):\n    (7, \"x\")\n\nfn main(console: Console):\n    console.print(\"${mklist()}\")\n    console.print(\"${pair()}\")\n",
                vec!["[1, 2, 3]".to_string(), "(7, x)".to_string()],
            ),
            // Render of a self-RECURSIVE ADT (`Node(Tree, Tree)`): the `$ts`
            // helper's name is reserved before its body is built, so the nested
            // `Tree` fields render via a recursive `call` to the same helper
            // (tying the knot) rather than bailing the cycle guard. The WAT path
            // renders enums structurally too, so all three backends agree.
            (
                "type Tree:\n    Leaf(Int)\n    Node(Tree, Tree)\n\nfn main(console: Console):\n    let t = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    console.print(\"${t}\")\n",
                vec!["Node(Node(Leaf(1), Leaf(2)), Leaf(3))".to_string()],
            ),
            // `var` parameters (the multi-value move-out ABI): the callee returns
            // its declared value plus each var param's final value, and the call
            // site (`CallStoreMulti`) writes them back into the caller's vars. Covers
            // a bare var, repeated calls, and an var alongside a non-var arg.
            (
                "fn bump(var n: Int):\n    n = n + 1\nfn add(var n: Int, by: Int):\n    n = n + by\nfn main(console: Console):\n    var a = 0\n    bump(a)\n    bump(a)\n    bump(a)\n    add(a, 10)\n    console.print(\"${a}\")\n",
                vec!["13".to_string()],
            ),
            // a `region -> String:` — a POINTER result reclaimed via `$rcopy_str`
            // (deep-copy the region-born string down to the watermark, return the
            // biased ptr). The following `let after` allocates right where the region
            // was reclaimed, so a bad copy/slide would corrupt it.
            (
                "fn main(console: Console):\n    let s = region -> String:\n        var acc = \"\"\n        for i in 0..5:\n            acc = acc + \"x\"\n        acc\n    let after = \"ok\"\n    console.print(s)\n    console.print(after)\n",
                vec!["xxxxx".to_string(), "ok".to_string()],
            ),
        ];
        assert!(!cases.is_empty(), "the WIR binary path lowered nothing — convergence regressed");
        std::thread::scope(|s| {
            let handles: Vec<_> = cases.iter().map(|(src, want)| {
                s.spawn(move || {
                    let module = parser::parse_module(src).expect("parse");
                    let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
                    typeck::check(&linked).expect("typecheck");
                    let bytes = codegen::compile_module_binary(&linked)
                        .expect_lowered(&format!(
                            "expected the WIR binary path to handle this program:\n{src}"
                        ));
                    assert_eq!(&run_bytes_print_only(&bytes), want, "binary path (print-only):\n{src}");
                    assert_eq!(&link_run(src), want, "interpreter oracle:\n{src}");
                    assert_eq!(&run_on_wasm(src), want, "legacy WAT path:\n{src}");
                })
            }).collect();
            for h in handles { h.join().unwrap(); }
        });
    }

    /// Enum (Adt) structural `==` on the binary path — the generated `$eq_*`
    /// tag-dispatch helper. Kept OUT of the 3-way corpus deliberately: the legacy
    /// WAT path pointer-compares enums (a pre-existing compiled-vs-interpreter
    /// divergence — it returns `false` even for `None == None`), so it can't be the
    /// oracle here. The binary path is structurally CORRECT, so we assert it against
    /// the INTERPRETER directly. Covers None==None, tag mismatch, nullary-variant
    /// equality, and equal/unequal nested-String payloads.
    #[test]
    fn wir_enum_eq_binary_path() {
        let src = "type CalcError derive(PartialEq):\n    StackUnderflow\n    UnknownToken(String)\n    DivByZero\n\nfn main(console: Console):\n    let a: Option(CalcError) = None\n    let b: Option(CalcError) = Some(StackUnderflow)\n    let c: Option(CalcError) = Some(UnknownToken(\"x\"))\n    let d: Option(CalcError) = Some(UnknownToken(\"y\"))\n    let cx: Option(CalcError) = Some(UnknownToken(\"x\"))\n    console.print(\"${a == None}\")\n    console.print(\"${b == None}\")\n    console.print(\"${b == Some(StackUnderflow)}\")\n    console.print(\"${c == cx}\")\n    console.print(\"${c == d}\")\n    console.print(\"${b == c}\")\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
        ];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should structurally lower enum `==`");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// Structural `==` on a self-RECURSIVE ADT (`Node(Tree, Tree)`) on the binary
    /// path. The `$eq_*` helper reserves its name before building, so a nested
    /// `Tree` field compares via a recursive `call` to the same helper. (2-way:
    /// the WAT path pointer-compares enums — a known WAT/interpreter divergence —
    /// so compare binary vs the interpreter oracle, which compares structurally.)
    #[test]
    fn wir_recursive_adt_eq_binary_path() {
        let src = "type Tree:\n    Leaf(Int)\n    Node(Tree, Tree)\n\nfn main(console: Console):\n    let a = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    let b = Node(Node(Leaf(1), Leaf(2)), Leaf(3))\n    let c = Node(Node(Leaf(1), Leaf(9)), Leaf(3))\n    console.print(\"${a == b}\")\n    console.print(\"${a == c}\")\n";
        let want = vec!["true".to_string(), "false".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should compare a recursive ADT");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// The native regex engine on the binary path: `regex.match_spans` is a host
    /// import (the Rust `regex` crate, the same native the interpreter uses)
    /// wrapped by `$regex_match_spans` (length-prefixed `fill_pending` read, like
    /// `dir_read`). Ungated (matching needs no capability), so the print-only
    /// harness instantiates it. Compared against the linked interpreter oracle.
    #[test]
    fn wir_regex_match_spans_binary_path() {
        let src = "import regex\nfn main(console: Console):\n    console.print(\"${regex.matches(\"[0-9]+\", \"order 1234\")}\")\n    console.print(\"${regex.find_all(\"[0-9]+\", \"a1 b22 c333\")}\")\n    console.print(regex.replace_all(\"[0-9]+\", \"a1b22\", \"N\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower regex via the host engine");
        let want = link_run(src);
        assert_eq!(want[0], "true", "regex.matches sanity");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path vs oracle");
    }

    /// `$dir_read` (read a file) on the binary path — the two-phase
    /// `dir_read_len` + `fill_pending` host protocol, gated behind Dir(Read).
    /// Sets up a sandbox dir with a file and reads it back.
    #[test]
    fn wir_dir_read_host_import_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wir_dirread_{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("greeting.txt"), "hello from disk").expect("write file");
        let src = "fn main(console: Console, dir: Dir[Read]):\n    console.print(dir.read(\"greeting.txt\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle dir read via the host imports");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities { print: true, dir_root: Some(root.clone()), dir_read: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn with Dir(Read)");
        actor.run().expect("run");
        let got = actor.output();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(got, vec!["hello from disk".to_string()], "binary path");
    }

    /// `$get_env` on the binary path — a host-import helper returning an
    /// `Option(String)`, consumed via `match` (now lowering via the
    /// constructor-pattern arm). The absent branch is deterministic ("unset");
    /// the present branch (PATH) takes the `Some` arm. Env grant.
    #[test]
    fn wir_get_env_option_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "fn main(console: Console, env: Env):\n    match env.get_env(\"WITCHY_UNSET_XYZZY_VAR\"):\n        Some(v) -> console.print(v)\n        None -> console.print(\"unset\")\n    match env.get_env(\"PATH\"):\n        Some(v) -> console.print(\"has\")\n        None -> console.print(\"no-path\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle get_env + match");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, env: true, quiet: true, ..Default::default() }, crate::RUN_MEMORY_PAGES)
            .expect("spawn with Env");
        actor.run().expect("run");
        let got = actor.output();
        assert_eq!(got[0], "unset", "absent var → None → unset");
        assert_eq!(got.len(), 2, "both matches print one line each");
        assert!(matches!(got[1].as_str(), "has" | "no-path"), "present-var branch: {got:?}");
    }

    /// An Int-returning `main` on the binary path: the `run` wrapper prints the
    /// result via `print_int` (the exit-code convention), matching the WAT sink.
    /// Validated against the WAT path (both compiled paths use i32 `Int`, so they
    /// agree exactly — unlike the i64 interpreter). Needs the `print_int` grant.
    #[test]
    fn wir_int_returning_main_prints_result() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "fn main(console: Console) -> Int:\n    console.print(\"hi\")\n    42\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle an Int-returning main");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, print_int: true, quiet: true, ..Default::default() }, crate::RUN_MEMORY_PAGES)
            .expect("spawn with print_int");
        actor.run().expect("run");
        let got = actor.output();
        assert_eq!(got, vec!["hi".to_string(), "42".to_string()], "binary path");
        assert_eq!(got, run_on_wasm(src), "binary path matches WAT path");
    }
