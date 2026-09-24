use super::*;
use crate::{ast, codegen, interpreter, parser, typeck};

    /// (BUG-535) Lists are ordinary comparison-protocol values: if their elements
    /// satisfy `PartialEq`/`Eq`, the list itself satisfies the same bound instead
    /// of relying on one-off direct-operator magic.
    #[test]
    fn list_equality_satisfies_partial_eq_bounds_on_both_backends() {
        let src = "import cmp\nimport testing\n\ntype Key derive(Show, Eq):\n    id: Int\n    cache: Int\n\nimpl PartialEq for Key:\n    fn eq(self, other: Key) -> Bool:\n        self.id == other.id\n\nfn same(x: a, y: a) -> Bool where a: PartialEq:\n    x == y\n\nfn total_same(x: a, y: a) -> Bool where a: Eq:\n    x == y\n\nfn main(console: Console):\n    console.print(\"${same([1, 2, 3], [1, 2, 3])}\")\n    console.print(\"${same([Key(1, 10)], [Key(1, 20)])}\")\n    console.print(\"${total_same([Key(1, 10)], [Key(1, 20)])}\")\n    testing.assert_value_eq([Key(1, 10)], [Key(1, 20)])\n";
        let expected = ["true", "true", "true"];
        assert_eq!(link_run(src), expected, "interp: list PartialEq bounds");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: list PartialEq bounds",
        );
    }

    /// (BUG-395 / RFC-0047) Public `std/dict` helpers expose the same key
    /// equality contract as direct native dict operations. Concrete key/value
    /// types without `Eq` cannot route through `dict.get`, `from_pairs`,
    /// `map_values`, `filter`, `merge`, or `invert` on the public `witchy check`
    /// path (type check + compiled-backend acceptance); supported `Eq` key
    /// shapes can.
    #[test]
    fn dict_wrapper_key_operations_require_visible_eq_bounds() {
        let resolve_fs_std = |src: &str| -> ast::Module {
            use std::collections::{HashSet, VecDeque};
            let entry = parser::parse_module(src).expect("parse");
            let mut modules: Vec<(String, ast::Module)> = vec![("main".to_string(), entry.clone())];
            let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
            let mut queue: VecDeque<ast::Module> = VecDeque::from([entry]);
            while let Some(module) = queue.pop_front() {
                for name in module.imports.clone() {
                    if !loaded.insert(name.clone()) {
                        continue;
                    }
                    let source = std::fs::read_to_string(format!("std/{name}.witchy"))
                        .expect("std module source");
                    let parsed = parser::parse_module(&source).expect("parse std module");
                    queue.push_back(parsed.clone());
                    modules.push((name, parsed));
                }
            }
            crate::pipeline::link(modules, "main").expect("link")
        };

        let rejected = [
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn main(console: Console):\n    let d: Dict(Key, Int) = dict.new()\n    let _x = d.get(Key(1))\n    console.print(\"bad\")\n",
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn main(console: Console):\n    let _x = dict.from_pairs([(Key(1), 1)])\n    console.print(\"bad\")\n",
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn id(x: Int) -> Int:\n    x\n\nfn main(console: Console):\n    let d: Dict(Key, Int) = dict.new()\n    let _x = d.map_values(id)\n    console.print(\"bad\")\n",
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn keep(_k: Key, _v: Int) -> Bool:\n    true\n\nfn main(console: Console):\n    let d: Dict(Key, Int) = dict.new()\n    let _x = d.filter(keep)\n    console.print(\"bad\")\n",
            "import dict\n\ntype Key:\n    Key(Int)\n\nfn main(console: Console):\n    let d: Dict(Key, Int) = dict.new()\n    let _x = d.merge(d)\n    console.print(\"bad\")\n",
            "import dict\n\ntype Value:\n    Value(Int)\n\nfn main(console: Console):\n    let d: Dict(String, Value) = dict.new()\n    let _x = d.invert()\n    console.print(\"bad\")\n",
        ];
        for src in rejected {
            let linked = resolve_fs_std(src);
            match typeck::check(&linked) {
                Err(err) => assert!(
                    err.message.contains("Eq"),
                    "expected visible Eq-bound error, got: {}",
                    err.message
                ),
                Ok(()) => {
                    let result = codegen::compile_module_binary(&linked);
                    assert!(
                        matches!(result, codegen::LoweringOutcome::Rejected(_)),
                        "non-Eq dict wrapper must be a hard compiled rejection"
                    );
                }
            }
        }

        let erased_wrapper = "import dict\n\npub fn wrapped(d: Dict(k, v), key: k) -> Option(v):\n    d.get(key)\n";
        let linked = resolve_fs_std(erased_wrapper);
        let err = typeck::check(&linked).expect_err("generic wrapper must forward dict.get's Eq bound");
        assert!(err.message.contains("requires `k: Eq`"), "expected forwarded Eq-bound error, got: {}", err.message);

        let bounded_wrapper = "import dict\n\npub fn wrapped(d: Dict(k, v), key: k) -> Option(v) where k: Eq:\n    d.get(key)\n";
        let linked = resolve_fs_std(bounded_wrapper);
        typeck::check(&linked).expect("generic wrapper can forward dict.get's Eq bound");

        let accepted = "import dict\n\nfn id(x: Int) -> Int:\n    x\n\nfn keep(_k: String, _v: Int) -> Bool:\n    true\n\nfn main(console: Console):\n    let d: Dict(String, Int) = dict.new()\n    let values: Dict(String, Int) = dict.new()\n    let _a = d.get(\"one\")\n    let _b = dict.from_pairs([(\"one\", 1)])\n    let _c = d.map_values(id)\n    let _d = d.filter(keep)\n    let _e = d.merge(d)\n    let _f = values.invert()\n    console.print(\"ok\")\n";
        let linked = resolve_fs_std(accepted);
        typeck::check(&linked).expect("bounded dict wrappers type-check");
        codegen::compile_module_binary(&linked)
            .expect_lowered("bounded dict wrappers lower");
    }

    /// VALUE EQUALITY, ALWAYS (the learning log's F15): dict lookups with
    /// RUNTIME-BUILT keys (trim/split/concat-sourced) — the case literal-key
    /// tests pass vacuously through interning. dict.get/has must find them
    /// by CONTENT on both backends; the compiled tier used to silently
    /// pointer-compare and return None.
    #[test]
    fn runtime_built_dict_keys_compare_by_content() {
        let src = "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    let _ = dict.insert(d, \"  host  \".trim(), \"localhost\")\n    let parts = \"port=8080\".split(\"=\")\n    let _ = dict.insert(d, list.at(parts, 0), list.at(parts, 1))\n    let _ = dict.insert(d, \"lit\" + \"eral\", \"joined\")\n    match d.get(\"host\"):\n        Some(v) -> console.print(\"host=\" + v)\n        None -> console.print(\"host MISSING\")\n    match d.get(\"port\"):\n        Some(v) -> console.print(\"port=\" + v)\n        None -> console.print(\"port MISSING\")\n    console.print(\"${dict.contains_key(d, \"literal\")}\")\n    console.print(\"${dict.length(d)}\")\n";
        let want: Vec<String> = ["host=localhost", "port=8080", "true", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-315, RFC-0044 rule 3) An out-of-range (or negative) `xs[i] = v` /
    /// `list.set_at` / `list.update_at` is a runtime error on BOTH backends,
    /// matching the `xs[i]` READ trap — never a silent no-op. In-bounds still agrees.
    #[test]
    fn oob_list_set_at_traps_on_both_backends() {
        let compile = |src: &str| -> (ast::Module, Vec<u8>) {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let bytes = codegen::compile_module_binary(&linked)
                .expect_lowered("lowers");
            (linked, bytes)
        };
        for prog in [
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    xs[5] = 9\n    console.print(\"${xs}\")\n",
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    xs[0 - 1] = 9\n    console.print(\"${xs}\")\n",
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    list.set_at(xs, 5, 9)\n    console.print(\"${xs}\")\n",
            "fn main(console: Console):\n    var xs = [1, 2, 3]\n    list.update_at(xs, 9, fn(x: Int): x + 1)\n    console.print(\"${xs}\")\n",
        ] {
            let (lmod, wasm) = compile(prog);
            assert!(interpreter::run_module(lmod, ".", Vec::new()).is_err(), "interp must trap: {prog}");
            assert!(crate::run_wasm_bytes(&wasm).is_err(), "wasm must trap: {prog}");
        }
        let ok = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    xs[1] = 9\n    console.print(\"${xs}\")\n";
        assert_eq!(link_run(ok), ["[1, 9, 3]"], "interp in-bounds");
        assert_eq!(wasm_run(ok), ["[1, 9, 3]"], "wasm in-bounds");
    }

    /// (RFC-0047) A Dict keyed by `Float` is a compile-time error — keys require
    /// `Eq`, and `Float` is only `PartialEq` (NaN != NaN, so a NaN key is
    /// unretrievable and `0.1 + 0.2` is a precision trap). This closes the NaN-key
    /// hole wholesale (breaking change: Float keys used to compile and run). The
    /// error teaches the standard escapes (a scaled Int, or a String rendering).
    #[test]
    fn dict_float_keys_are_a_compile_error() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    let _ = dict.insert(d, 1.5, \"a\")\n    console.print(dict.get_or(d, 1.5, \"?\"))\n";
        let e = typeck::check(&resolve_std_src(src))
            .expect_err("a Float-keyed dict must be rejected")
            .to_string();
        assert!(
            e.contains("requires `Float: Eq`") && e.contains("dict.insert"),
            "teaching error naming the Eq requirement, got: {e}"
        );
        // The NaN case (the original hole) is rejected by the same type rule,
        // before any runtime lookup can silently miss.
        let nan = "fn main(console: Console):\n    var d = dict.new()\n    let _ = dict.insert(d, 0.0 / 0.0, \"nan\")\n    console.print(dict.get_or(d, 0.0 / 0.0, \"missing\"))\n";
        assert!(
            typeck::check(&resolve_std_src(nan)).expect_err("a NaN Float key must be rejected").to_string().contains("requires `Float: Eq`"),
            "the NaN-key hole is closed by the type rule"
        );
        // An Int-keyed dict (the suggested escape) still works on both backends.
        let ok = "fn main(console: Console):\n    var d = dict.new()\n    let _ = dict.insert(d, 3, \"a\")\n    console.print(dict.get_or(d, 3, \"?\"))\n";
        assert_eq!(interp(ok), vec!["a"], "interpreter (Int key)");
        assert_eq!(run_on_wasm(ok), vec!["a"], "compiled WASM (Int key)");
    }

    /// (RFC-0047) A `Set` of `Float` is likewise a compile-time error — members
    /// require `Eq`. The Set stdlib already documents this doctrine; the type rule
    /// makes it true.
    #[test]
    fn set_float_members_are_a_compile_error() {
        let src = "import set\n\nfn main(console: Console):\n    var s = set.new()\n    let _ = set.insert(s, 1.5)\n    console.print(\"${set.length(s)}\")\n";
        let linked = resolve_std_src(src);
        let e = typeck::check(&linked).expect_err("a Float-membered set must be rejected").to_string();
        assert!(e.contains("requires `Float: Eq`") && e.contains("Set__insert"), "teaching error, got: {e}");
    }

    /// (RFC-0047) A CUSTOM `PartialEq` impl is honored at EVERY depth — top level
    /// AND inside a `List`, `Option`, tuple, and as a `Dict` value. Before, a
    /// custom impl silently vanished below the surface (the container did a
    /// structural memcmp): `P(1) == P(2)` called the impl (`true`) but
    /// `[P(1)] == [P(2)]` was `false`. Both backends must now honor it uniformly.
    /// (The impl here is always-`true`, so any honored comparison yields `true`;
    /// a structural memcmp of differing fields would yield `false` — the tell.)
    #[test]
    fn custom_partial_eq_is_honored_at_every_depth() {
        let src = "type P:\n    P(Int)\n\nimpl PartialEq for P:\n    fn eq(self, other: P) -> Bool:\n        true\n\nfn main(console: Console):\n    console.print(\"${P(1) == P(2)}\")\n    console.print(\"${[P(1)] == [P(2)]}\")\n    console.print(\"${Some(P(1)) == Some(P(2))}\")\n    console.print(\"${(P(1), 0) == (P(2), 0)}\")\n    var a = dict.new()\n    let _ = dict.insert(a, 1, P(1))\n    var b = dict.new()\n    let _ = dict.insert(b, 1, P(2))\n    console.print(\"${a == b}\")\n";
        let want = vec![
            "true".to_string(), // top-level impl (as before)
            "true".to_string(), // inside a List — NEW: was false
            "true".to_string(), // inside an Option — NEW
            "true".to_string(), // inside a tuple — NEW
            "true".to_string(), // as a Dict value — NEW
        ];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "compiled WASM must agree");
    }

    /// Indexing a list out of bounds must FAIL on both backends, not silently
    /// read adjacent heap on WASM. The compiled `$list_at` bounds-checks and traps
    /// (like division-by-zero), matching the interpreter's "index out of bounds"
    /// error. In-bounds indexing still agrees. (Regression for a silent OOB-read
    /// divergence.)
    #[test]
    fn list_index_out_of_bounds_errors_on_both_backends() {
        let oob = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    console.print(\"${list.at(xs, 5)}\")\n";
        let module = parser::parse_module(oob).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        assert!(interpreter::run(oob).is_err(), "interpreter must error on OOB index");
        assert!(crate::run_wasm_bytes(&bytes).is_err(), "WASM must trap on OOB index");
        // A negative index likewise traps (it used to read backwards into the heap).
        let neg = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    console.print(\"${list.at(xs, 0 - 1)}\")\n";
        let nmod = parser::parse_module(neg).expect("parse");
        let nbytes = codegen::compile_module_binary(&nmod)
            .expect_lowered("the binary path lowers this program");
        assert!(interpreter::run(neg).is_err(), "interpreter must error on negative index");
        assert!(crate::run_wasm_bytes(&nbytes).is_err(), "WASM must trap on negative index");
        // In-bounds indexing still agrees.
        let ok = "fn main(console: Console):\n    let xs = [10, 20, 30]\n    console.print(\"${list.at(xs, 2)}\")\n";
        assert_eq!(interp(ok), vec!["30".to_string()], "interpreter");
        assert_eq!(run_on_wasm(ok), vec!["30".to_string()], "compiled WASM must agree");
    }

    /// A loop condition proves an index only at body entry.  Reassigning the
    /// index before an access, widening a saved bound, shrinking the sequence,
    /// or reading `upper == length` before its decrement must retain the normal
    /// trap.  These were capability-free optimized-Wasm OOB reads while the
    /// interpreter and `WITCHY_OPT=none` correctly rejected every program.
    #[test]
    fn bounds_elision_rejects_unstable_loop_facts() {
        if !is_nightly() {
            return;
        }
        let cases = [
            (
                "increment before access",
                "fn main(console: Console):\n    let xs = [111]\n    var i = 0\n    while i < xs.length():\n        i = i + 1\n        console.print(\"${xs[i]}\")\n",
            ),
            (
                "match-hidden reassignment",
                "fn main(console: Console):\n    let xs = [111]\n    var i = 0\n    while i < xs.length():\n        match i:\n            0 -> i = 1\n            _ -> ()\n        console.print(\"${xs[i]}\")\n        i = i + 1\n",
            ),
            (
                "branch facts do not flow into sibling paths",
                "fn main(console: Console):\n    var i = 0\n    if true:\n        i = 0 - 1\n    else:\n        i = 0\n    let xs = [111]\n    while i < xs.length():\n        console.print(\"${xs[i]}\")\n        i = i + 1\n",
            ),
            (
                "widened saved bound",
                "fn main(console: Console):\n    let xs = [111]\n    var n = xs.length()\n    var i = 0\n    while i < n:\n        n = 3\n        console.print(\"${xs[i]}\")\n        i = i + 1\n",
            ),
            (
                "upper used before decrement",
                "fn main(console: Console):\n    let xs = [111]\n    var upper = xs.length()\n    var i = 0\n    while i < upper:\n        console.print(\"${xs[upper]}\")\n        upper = upper - 1\n        i = i + 1\n",
            ),
            (
                "while root shrunk after guard",
                "import list\n\nfn main(console: Console):\n    var xs = [111]\n    var i = 0\n    while i < xs.length():\n        let _removed = xs.pop()\n        console.print(\"${xs[i]}\")\n        i = i + 1\n",
            ),
            (
                "for root shrunk after range creation",
                "import list\n\nfn main(console: Console):\n    var xs = [111]\n    for i in 0..xs.length():\n        let _removed = xs.pop()\n        console.print(\"${xs[i]}\")\n",
            ),
            (
                "byte index hidden below zero",
                "import bytes\n\nfn main(console: Console):\n    let xs = bytes.from_string(\"x\")\n    var i = 0\n    while i < bytes.length(xs):\n        match i:\n            0 -> i = 0 - 1\n            _ -> ()\n        console.print(\"${bytes.at(xs, i)}\")\n        i = i + 3\n",
            ),
            (
                "out-of-bounds store after hidden reassignment",
                "fn main(console: Console):\n    var xs = [111]\n    var i = 0\n    while i < xs.length():\n        match i:\n            0 -> i = 1\n            _ -> ()\n        xs[i] = 222\n        i = i + 1\n    console.print(\"unreachable\")\n",
            ),
            (
                "wide store index must not narrow before its check",
                "fn main(console: Console):\n    var xs = [111]\n    var i = 0 - 4294967296\n    while i < 1:\n        xs[i] = 222\n        console.print(\"${xs[0]}\")\n        return ()\n        i = i + 1\n",
            ),
            (
                "saved length invalidated before loop",
                "import list\n\nfn main(console: Console):\n    var xs = [111]\n    let stale = xs.length()\n    let _removed = xs.pop()\n    var i = 0\n    while i < stale:\n        console.print(\"${xs[i]}\")\n        i = i + 1\n",
            ),
            (
                "guarded range fact invalidated between loops",
                "import list\n\nfn attack(let n: Int) -> Int:\n    var count = []\n    for i in 0..n:\n        list.push(count, 0)\n    var r = n\n    while r != 1:\n        list.set_at(count, r - 1, r)\n        r = r - 1\n    let _first = count.pop()\n    let _second = count.pop()\n    var advanced = false\n    while !advanced:\n        if r == n:\n            return 0\n        list.set_at(count, r, 777)\n        if list.at(count, r) > 0:\n            advanced = true\n        else:\n            r = r + 1\n    555\n\nfn main(console: Console):\n    console.print(\"${attack(3)}\")\n",
            ),
            (
                "nonnegative fact invalidated by var call",
                "fn make_negative(var value: Int):\n    value = 0 - 1\n\nfn main(console: Console):\n    let xs = [111]\n    var i = 0\n    make_negative(i)\n    while i < xs.length():\n        console.print(\"${xs[i]}\")\n        i = i + 1\n",
            ),
            (
                "length-bound fact invalidated by var call",
                "fn widen(var value: Int):\n    value = 3\n\nfn main(console: Console):\n    let xs = [111]\n    var limit = xs.length()\n    widen(limit)\n    var i = 0\n    while i < limit:\n        console.print(\"${xs[i]}\")\n        i = i + 1\n",
            ),
            (
                "capability-free dependency corrupts caller path",
                "fn slice(text: String) -> String:\n    let padding = [0]\n    var scratch = [text]\n    var i = 0\n    while i < scratch.length():\n        match i:\n            0 -> i = 0 - 5\n            _ -> ()\n        scratch[i] = \"evil.json\"\n        i = i + 7\n    if padding.length() == 0:\n        return \"unreachable\"\n    text\n\nfn main(console: Console):\n    let publisher_path = [\"safe.json\"]\n    let _ignored = slice(\"input\")\n    console.print(publisher_path[0])\n",
            ),
            (
                "wrapping addition invalidates nonnegative fact",
                "fn main(console: Console):\n    let xs = [111]\n    var i = 9223372036854775807\n    i = i + 9223372032559808513\n    while i < xs.length():\n        console.print(\"${xs[i]}\")\n        return ()\n        i = i + 1\n",
            ),
            (
                "local facts do not cross function units",
                "fn seed_fact() -> Int:\n    let xs = [1]\n    var i = 0\n    var observed = 0\n    while i < xs.length():\n        observed = xs[i]\n        i = i + 1\n    observed\n\nfn victim(var i: Int) -> Int:\n    let xs = [111]\n    while i < xs.length():\n        return xs[i]\n        i = i + 1\n    0\n\nfn main(console: Console):\n    let _seed = seed_fact()\n    var attack_i = 0 - 4294967296\n    console.print(\"${victim(attack_i)}\")\n",
            ),
        ];

        for (name, src) in cases {
            let linked = resolve_std_src(src);
            typeck::check(&linked).unwrap_or_else(|error| panic!("{name} typechecks: {error}"));
            assert!(
                interpreter::run_module(linked.clone(), ".", Vec::new()).is_err(),
                "interpreter must reject {name}",
            );

            crate::opt::set_for_tests(Some(crate::opt::OptSet::all()));
            let compiled = codegen::compile_module_binary(&linked);
            crate::opt::set_for_tests(None);
            let bytes = compiled.expect_lowered(name);
            assert!(
                crate::run_wasm_bytes(&bytes).is_err(),
                "optimized Wasm must retain the bounds trap for {name}",
            );
        }
    }

    /// Dict `update` (single-lookup upsert) must agree on both backends, including
    /// nested updates and a big-`Int` value. WASM lowers it to a `$dict_update`
    /// helper that reads the current value (or default), applies the closure via
    /// `call_indirect`, and reinserts — equivalent to the interpreter's
    /// `dict.insert(d, k, f(dict.get_or(d, k, default)))`. (Regression for the
    /// interpreter-only dict-upsert gap.)
    #[test]
    fn dict_update_upsert_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    let _ = dict.insert(d, \"a\", 1)\n    let _ = dict.insert(d, \"b\", 2)\n    let _ = dict.update(d, \"a\", 0, fn(x: Int): x + 10)\n    let _ = dict.update(d, \"c\", 100, fn(x: Int): x + 1)\n    console.print(\"${dict.get_or(d, \"a\", -1)}\")\n    console.print(\"${dict.get_or(d, \"b\", -1)}\")\n    console.print(\"${dict.get_or(d, \"c\", -1)}\")\n    console.print(\"${dict.length(d)}\")\n    var counts = dict.new()\n    let _ = dict.update(counts, \"hit\", 0, fn(n: Int): n + 1)\n    let _ = dict.update(counts, \"hit\", 0, fn(n: Int): n + 1)\n    console.print(\"${dict.get_or(counts, \"hit\", -1)}\")\n";
        let want = vec![
            "11".to_string(),
            "2".to_string(),
            "101".to_string(),
            "3".to_string(),
            "2".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// The std `list` library is the most-exercised witchy code; verify a broad
    /// slice of it (reverse/take/drop/sort_by/zip/enumerate/map/filter/fold/
    /// index_of/contains/any/all) produces identical results in the interpreter
    /// and compiled to WASM. Int element lists keep this clear of the known
    /// generic-`==`-on-strings limitation (compiled compares those by pointer).
    #[test]
    fn std_list_library_backends_agree() {
        let client = r#"
import list

fn main(console: Console):
    let xs = [5, 3, 8, 1, 9, 2]
    var rev = xs
    list.reverse(rev)
    console.print((("${list.at(rev, 0)}" + ",") + "${list.at(rev, 5)}"))
    console.print((("${list.length(list.take(xs, 3))}" + ":") + "${list.at(list.take(xs, 3), 2)}"))
    console.print("${list.at(list.drop(xs, 4), 0)}")
    var sorted = xs
    list.sort_by(sorted, fn(a: Int, b: Int): (a < b))
    console.print((("${list.at(sorted, 0)}" + "..") + "${list.at(sorted, 5)}"))
    let pairs = list.zip([1, 2, 3], [10, 20, 30])
    let (pa, pb) = list.at(pairs, 1)
    console.print("${(pa + pb)}")
    let en = list.enumerate([100, 200])
    let (ei, ev) = list.at(en, 1)
    console.print("${((ei * 1000) + ev)}")
    let doubled = list.map(xs, fn(n: Int): (n * 2))
    let evens = list.filter(xs, fn(n: Int): ((n % 2) == 0))
    console.print("${list.fold(doubled, 0, fn(a: Int, b: Int): (a + b))}")
    console.print("${list.length(evens)}")
    console.print("${list.index_of(xs, 8)}")
    console.print("${list.contains(xs, 9)}")
    console.print("${list.any(xs, fn(n: Int): (n > 8))}")
    console.print("${list.all(xs, fn(n: Int): (n > 0))}")
    let empty = list.filter([1], fn(n: Int): (n > 100))
    console.print("${list.all(empty, fn(n: Int): (n > 0))}")
    console.print("${list.any(empty, fn(n: Int): (n > 0))}")
    let ps = list.zip([1, 2, 3], ["a", "b"])
    console.print("${list.length(ps)}")
    let (pn, ps_text) = list.at(ps, 0)
    console.print("${pn}" + ps_text)
    let words = ["a", "bb", "ccc"]
    console.print("${list.contains(words, "bb")}")
    console.print("${list.index_of(words, "ccc") ?? -1}")
    console.print("${list.sum(xs)}")
    console.print("${list.is_empty(xs)}")
    console.print("${list.is_empty(list.filter(xs, fn(n: Int): (n > 100)))}")
    console.print("${list.count_where(xs, fn(n: Int): ((n % 2) == 0))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std list library diverged between interpreter and compiled");
        assert_eq!(
            compiled,
            vec![
                "2,5", "3:8", "9", "1..9", "22", "1200", "56", "2", "Some(2)", "true",
                "true", "true", "true", "false", "2", "1a", "true", "2", "28", "false", "true", "2",
            ],
            "std list library golden output drifted",
        );
    }

    #[test]
    fn std_set_operations_backends_agree() {
        // Set ops dispatch through Eq (cross-module: set -> eq.member, both
        // bounded generics), so they are content-correct on both backends for
        // runtime-built strings and a user Eq type (Id), and dedupe along the way.
        let client = r#"
import set

type Id:
    Id(Int)

impl PartialEq for Id:
    fn eq(self, other: Self) -> Bool:
        match self:
            Id(a) -> match other:
                Id(b) -> (a == b)

impl Eq for Id

fn build(s: String) -> String:
    var acc = ""
    var i = 0
    while (i < s.char_count()):
        acc = (acc + s.substring(i, (i + 1)))
        i = (i + 1)
    acc

fn main(console: Console):
    let a = set.from_list([build("x"), build("y"), build("x")])
    let b = set.from_list([build("y"), build("z")])
    let u = set.union(a, b)
    let i = set.intersection(a, b)
    let d = set.difference(a, b)
    console.print(list.join(set.to_list(u), ","))
    console.print(list.join(set.to_list(i), ","))
    console.print(list.join(set.to_list(d), ","))
    console.print("${set.is_subset(set.from_list([build("y")]), a)}")
    console.print("${set.is_subset(set.from_list([build("z")]), a)}")
    let ids = set.union(set.from_list([Id(1), Id(2), Id(1)]), set.from_list([Id(2), Id(3)]))
    console.print("${set.length(ids)}")
"#;
        let sources = [
            ("set", crate::bundled_module("set").unwrap()),
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "std set ops diverged");
        assert_eq!(
            compiled,
            vec!["x,y,z", "y", "x", "true", "false", "3"]
        );
    }

    #[test]
    fn merge_sort_is_stable_on_both_backends() {
        // list.sort_by is a stable merge sort: equal keys keep their original
        // order. Sort (key, tag) items by key only; ties must preserve insertion
        // order. Both backends agree.
        let client = r#"
import list
type Item:
    Item(Int, String)
fn key(it: Item) -> Int:
    match it:
        Item(k, _t) -> k
fn tag(it: Item) -> String:
    match it:
        Item(_k, t) -> t
fn main(console: Console):
    var xs = [Item(2, "a"), Item(1, "b"), Item(2, "c"), Item(1, "d"), Item(2, "e")]
    list.sort_by(xs, fn(p: Item, q: Item): key(p) < key(q))
    for it in xs:
        console.print("${key(it)}" + tag(it))
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "merge sort diverged");
        assert_eq!(compiled, vec!["1b", "1d", "2a", "2c", "2e"]);
    }

    #[test]
    fn dict_undetermined_key_is_rejected() {
        // A key with no `Eq` implementation errors clearly
        // rather than picking a wrong comparison.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    let _ = dict.insert(d, console, 5)
    console.print("${dict.length(d)}")
"#;
        let linked = resolve_std_src(src);
        let err = typeck::check(&linked).expect_err("a capability has no Eq dictionary-key contract");
        assert!(
            err.to_string().contains("requires `Console: Eq`")
                && err.to_string().contains("dict.insert"),
            "unexpected error: {err}"
        );
    }

    /// Regression for a compiled-dict parity violation (FIXED in `dict_remove`):
    /// removing a key then re-inserting it, followed by any `dict.keys`/`values`/
    /// `pairs` iteration, used to corrupt the re-inserted entry on the COMPILED
    /// backend so `get_or` returned the default (the interpreter oracle was
    /// always correct). Root cause: `dict_remove` allocated `count` entry slots
    /// but advanced `heap` only past the `n` surviving entries, leaving the
    /// `count-n` slack the own-ABI tracks as capacity UNRESERVED — so the next
    /// in-place insert appended into it and the following allocation stomped the
    /// entry. Fixed by reserving the full allocated capacity. Both backends now
    /// agree on "5","1","5".
    #[test]
    fn dict_remove_reinsert_then_iterate_keeps_entry() {
        let src = r#"
import dict
fn main(console: Console):
    var b = dict.new()
    let _ = dict.insert(b, "x", 1)
    let _ = dict.remove(b, "x")
    let _ = dict.insert(b, "x", 5)
    console.print("${dict.get_or(b, "x", -1)}")
    console.print("${list.length(dict.keys(b))}")
    console.print("${dict.get_or(b, "x", -1)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "compiled backend diverges from the interpreter oracle");
        assert_eq!(run_on_wasm(src), vec!["5", "1", "5"]);
    }

