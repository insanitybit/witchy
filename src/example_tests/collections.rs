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

    /// Dicts should be ordinary protocol values too: direct dict equality is not
    /// enough if a generic helper cannot ask for `PartialEq`/`Eq` over a concrete
    /// `Dict(k, v)`.
    #[test]
    fn dict_equality_satisfies_protocol_bounds_on_both_backends() {
        let src = "import cmp\nimport dict\nimport testing\n\ntype Key derive(Show, Eq):\n    id: Int\n    cache: Int\n\nimpl PartialEq for Key:\n    fn eq(self, other: Key) -> Bool:\n        self.id == other.id\n\ntype Val derive(Show, Eq):\n    label: String\n    noise: Int\n\nimpl PartialEq for Val:\n    fn eq(self, other: Val) -> Bool:\n        self.label == other.label\n\nfn same(x: a, y: a) -> Bool where a: PartialEq:\n    x == y\n\nfn total_same(x: a, y: a) -> Bool where a: Eq:\n    x == y\n\nfn make_left() -> Dict(Key, Val):\n    var d = dict.new()\n    dict.insert(d, Key(1, 10), Val(\"one\", 100))\n    dict.insert(d, Key(2, 20), Val(\"two\", 200))\n    d\n\nfn make_right() -> Dict(Key, Val):\n    var d = dict.new()\n    dict.insert(d, Key(2, 99), Val(\"two\", 999))\n    dict.insert(d, Key(1, 42), Val(\"one\", 111))\n    d\n\nfn main(console: Console):\n    let left = make_left()\n    let right = make_right()\n    console.print(\"${left == right}\")\n    console.print(\"${same(left, right)}\")\n    console.print(\"${total_same(left, right)}\")\n    testing.assert_value_eq(left, right)\n";
        let expected = ["true", "true", "true"];
        assert_eq!(link_run(src), expected, "interp: dict PartialEq bounds");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: dict PartialEq bounds",
        );
    }

    /// (BUG-557) A generic container equality specialization can need a second
    /// generated generic impl for the element type. List(tuple5) must therefore
    /// emit the tuple `PartialEq` specialization even when no source call compares
    /// the tuple directly.
    #[test]
    fn list_of_tuple_equality_satisfies_protocol_bounds_on_both_backends() {
        let src = "import cmp\n\nfn total_same(x: a, y: a) -> Bool where a: Eq:\n    x == y\n\nfn main(console: Console):\n    let xs = [(1, \"x\", true, 90s, Greater)]\n    let ys = [(1, \"x\", true, 90s, Greater)]\n    let zs = [(1, \"x\", false, 90s, Greater)]\n    console.print(\"${total_same(xs, ys)}\")\n    console.print(\"${total_same(xs, zs)}\")\n";
        let expected = ["true", "false"];
        assert_eq!(link_run(src), expected, "interp: List(tuple5) Eq");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: List(tuple5) Eq",
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
        let src = "import dict\n\nfn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"  host  \".trim(), \"localhost\")\n    let parts = \"port=8080\".split(\"=\")\n    dict.insert(d, list.at(parts, 0), list.at(parts, 1))\n    dict.insert(d, \"lit\" + \"eral\", \"joined\")\n    match d.get(\"host\"):\n        Some(v) -> console.print(\"host=\" + v)\n        None -> console.print(\"host MISSING\")\n    match d.get(\"port\"):\n        Some(v) -> console.print(\"port=\" + v)\n        None -> console.print(\"port MISSING\")\n    console.print(\"${dict.contains_key(d, \"literal\")}\")\n    console.print(\"${dict.length(d)}\")\n";
        let want: Vec<String> = ["host=localhost", "port=8080", "true", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Std containers may expose real inherent methods without reopening bare
    /// std functions or losing the owner-module in-place path. `List.push` and
    /// `List.concat` are declared as `impl List(a)` methods, but receiver calls
    /// resolve to `list.push`/`list.concat` when those owner functions exist.
    #[test]
    fn std_list_impl_methods_and_free_functions_coexist_on_both_backends() {
        let src = "import list\n\ntype Buf:\n    items: List(Int)\n\nfn main(console: Console):\n    var b = Buf([])\n    var i = 0\n    while i < 16:\n        b.items.push(i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n\n    var xs = [1]\n    xs.push(2)\n    xs = xs.concat([3, 4])\n    console.print(\"${xs}\")\n\n    list.push(xs, 5)\n    let ys = list.concat(xs, [6])\n    console.print(\"${ys}\")\n";
        let expected = vec![
            "15".to_string(),
            "16".to_string(),
            "[1, 2, 3, 4]".to_string(),
            "[1, 2, 3, 4, 5, 6]".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter: std List impl/free methods");
        assert_eq!(wasm_run(src), expected, "wasm: std List impl/free methods");
    }

    /// The same owner-module method pattern applies to the other mutable core
    /// containers: receiver syntax is available, but the stable `dict.*`/`set.*`
    /// functions still exist and remain the in-place backend target.
    #[test]
    fn std_dict_set_impl_methods_and_free_functions_coexist_on_both_backends() {
        let src = "import dict\nimport set\n\ntype Bag:\n    counts: Dict(String, Int)\n    seen: Set(String)\n\nfn inc(n: Int) -> Int:\n    n + 1\n\nfn main(console: Console):\n    var bag = Bag(dict.new(), set.new())\n    var i = 0\n    while i < 20:\n        bag.counts.update(\"hit\", 0, inc)\n        bag.seen.insert(\"k${i}\")\n        i = i + 1\n    bag.counts.insert(\"extra\", 7)\n    bag.counts.remove(\"extra\")\n    bag.seen.remove(\"k0\")\n    console.print(\"${dict.get_or(bag.counts, \"hit\", 0)}\")\n    console.print(\"${dict.length(bag.counts)}\")\n    console.print(\"${set.length(bag.seen)}\")\n    console.print(\"${set.contains(bag.seen, \"k0\")}\")\n\n    var d = dict.new()\n    dict.insert(d, \"x\", 1)\n    dict.remove(d, \"missing\")\n    var s = set.new()\n    set.insert(s, \"x\")\n    set.remove(s, \"missing\")\n    console.print(\"${dict.get_or(d, \"x\", 0)}\")\n    console.print(\"${set.contains(s, \"x\")}\")\n";
        let expected = vec![
            "20".to_string(),
            "1".to_string(),
            "19".to_string(),
            "false".to_string(),
            "1".to_string(),
            "true".to_string(),
        ];
        assert_eq!(link_run(src), expected, "interpreter: std Dict/Set impl/free methods");
        assert_eq!(wasm_run(src), expected, "wasm: std Dict/Set impl/free methods");
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

    /// open-addressing table over the (insertion-ordered) entry array, so
    /// get_or/has/insert lookups probe instead of scanning. String and Int
    /// keys, growth rebuilds, removal (index dropped, rebuilt on next
    /// growth), and a missing-key probe all agree with the interpreter.
    #[test]
    fn dict_hash_index_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..3000:\n        dict.insert(d, \"k\" + \"${i}\", i * 2)\n    console.print(\"${dict.length(d)}\")\n    console.print(\"${dict.get_or(d, \"k2999\", 0 - 1)}\")\n    console.print(\"${dict.get_or(d, \"absent\", 0 - 1)}\")\n    console.print(\"${dict.contains_key(d, \"k1500\")}\")\n    dict.remove(d, \"k0\")\n    console.print(\"${dict.length(d)}\")\n    dict.insert(d, \"again\", 7)\n    console.print(\"${dict.get_or(d, \"again\", 0 - 1)}\")\n";
        let want: Vec<String> = ["3000", "5998", "-1", "true", "2999", "7"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// REGRESSION GUARD: `list.reverse`/`flatten`/`flat_map` are O(n), not O(n^2).
    /// They used to accumulate with `list.concat`, which copies the whole growing
    /// result each iteration — O(n^2) time AND allocation, which traps the WASM
    /// bump allocator (out-of-bounds) at ~20k elements. At 50k the linear
    /// push-loop forms stay far under the heap ceiling; an O(n^2) regression would
    /// trap on the compiled backend and fail here. The 50k run is compiled-only:
    /// the guard's teeth are that WASM heap trap, and the interpreter's
    /// clone-per-push is quadratic by design (it is the semantic oracle, not a
    /// perf target — see the arena watermark test), so the oracle verifies the
    /// same program at parity scale instead of burning a minute at 50k.
    #[test]
    fn list_reverse_flatten_flat_map_are_linear_at_scale() {
        let src = |n: u32| {
            format!(
                "fn main(console: Console):\n    var xs = []\n    for i in 0..{n}:\n        list.push(xs, i)\n    var r = xs\n    list.reverse(r)\n    console.print(\"${{list.at(r, 0)}}\")\n    console.print(\"${{list.at(r, {last})}}\")\n    console.print(\"${{list.flatten([[1, 2], [], [3]])}}\")\n    console.print(\"${{list.flat_map([1, 2, 3], fn(x: Int): [x, x * 10])}}\")\n",
                last = n - 1
            )
        };
        let want = |n: u32| -> Vec<String> {
            vec![
                (n - 1).to_string(),
                "0".to_string(),
                "[1, 2, 3]".to_string(),
                "[1, 10, 2, 20, 3, 30]".to_string(),
            ]
        };
        assert_eq!(link_run(&src(1000)), want(1000), "interpreter");
        assert_eq!(wasm_run(&src(1000)), want(1000), "compiled WASM must agree");
        assert_eq!(wasm_run(&src(50000)), want(50000), "compiled at 50k must stay linear");
    }

    /// A negative `Int` that enters a list through a *generic* function (the
    /// element type is a type variable, so it crosses the i32 generic ABI) and is
    /// then read back through *concrete* `List(Int)` code must keep its sign on
    /// WASM. `to_slot` used to zero-extend, turning -1 into 4294967295 when a
    /// concrete reader loaded the i64 slot; it now sign-extends (pointers/Bools
    /// are < 2^31, so they're unaffected). Regression for the generic-list bug
    /// found via `list.repeat(-1, n)`.
    #[test]
    fn wasm_negative_int_survives_the_generic_list_abi() {
        let src = "fn fill(x: a, n: Int) -> List(a):\n    var out = []\n    var i = 0\n    while i < n:\n        list.push(out, x)\n        i = i + 1\n    out\n\nfn show(xs: List(Int)) -> String:\n    var out = \"\"\n    for v in xs:\n        out = out + \"${v}\" + \" \"\n    out\n\nfn main(console: Console):\n    console.print(show(fill(-1, 3)))\n";
        let want = vec!["-1 -1 -1 ".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// (RFC-0047) A Dict keyed by `Float` is a compile-time error — keys require
    /// `Eq`, and `Float` is only `PartialEq` (NaN != NaN, so a NaN key is
    /// unretrievable and `0.1 + 0.2` is a precision trap). This closes the NaN-key
    /// hole wholesale (breaking change: Float keys used to compile and run). The
    /// error teaches the standard escapes (a scaled Int, or a String rendering).
    #[test]
    fn dict_float_keys_are_a_compile_error() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, 1.5, \"a\")\n    console.print(dict.get_or(d, 1.5, \"?\"))\n";
        let e = typeck::check(&resolve_std_src(src))
            .expect_err("a Float-keyed dict must be rejected")
            .to_string();
        assert!(
            e.contains("not a valid `Dict` key") && e.contains("Eq"),
            "teaching error naming the Eq requirement, got: {e}"
        );
        // The NaN case (the original hole) is rejected by the same type rule,
        // before any runtime lookup can silently miss.
        let nan = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, 0.0 / 0.0, \"nan\")\n    console.print(dict.get_or(d, 0.0 / 0.0, \"missing\"))\n";
        assert!(
            typeck::check(&resolve_std_src(nan)).expect_err("a NaN Float key must be rejected").to_string().contains("not a valid `Dict` key"),
            "the NaN-key hole is closed by the type rule"
        );
        // An Int-keyed dict (the suggested escape) still works on both backends.
        let ok = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, 3, \"a\")\n    console.print(dict.get_or(d, 3, \"?\"))\n";
        assert_eq!(interp(ok), vec!["a"], "interpreter (Int key)");
        assert_eq!(run_on_wasm(ok), vec!["a"], "compiled WASM (Int key)");
    }

    /// (RFC-0047) A `Set` of `Float` is likewise a compile-time error — members
    /// require `Eq`. The Set stdlib already documents this doctrine; the type rule
    /// makes it true.
    #[test]
    fn set_float_members_are_a_compile_error() {
        let src = "import set\n\nfn main(console: Console):\n    var s = set.new()\n    set.insert(s, 1.5)\n    console.print(\"${set.length(s)}\")\n";
        let linked = resolve_std_src(src);
        let e = typeck::check(&linked).expect_err("a Float-membered set must be rejected").to_string();
        assert!(e.contains("not a valid `Set` member") && e.contains("Eq"), "teaching error, got: {e}");
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
        let src = "type P:\n    P(Int)\n\nimpl PartialEq for P:\n    fn eq(self, other: P) -> Bool:\n        true\n\nfn main(console: Console):\n    console.print(\"${P(1) == P(2)}\")\n    console.print(\"${[P(1)] == [P(2)]}\")\n    console.print(\"${Some(P(1)) == Some(P(2))}\")\n    console.print(\"${(P(1), 0) == (P(2), 0)}\")\n    var a = dict.new()\n    dict.insert(a, 1, P(1))\n    var b = dict.new()\n    dict.insert(b, 1, P(2))\n    console.print(\"${a == b}\")\n";
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

    /// `Option` `==` is structural on both backends: a single-parameter generic
    /// ADT is instantiated at the comparison site from a constructor literal
    /// (sound for both operands — the type checker guarantees they share a
    /// type). Dict `==` compares by key/value contents, not insertion order.
    /// (Closes the former loud-error gaps.)
    #[test]
    fn option_and_dict_equality_agree_on_both_backends() {
        let src = "import option\n\nfn pair(a: Int, b: Int) -> Dict(String, Int):\n    var d = dict.new()\n    dict.insert(d, \"k\", a)\n    dict.insert(d, \"j\", b)\n    d\n\nfn main(console: Console):\n    let none_i: Option(Int) = None\n    console.print(\"${Some(5) == Some(5)}\")\n    console.print(\"${Some(5) == Some(6)}\")\n    console.print(\"${Some(5) == None}\")\n    console.print(\"${none_i == None}\")\n    console.print(\"${Some(\"a\") == Some(\"a\")}\")\n    console.print(\"${Some(\"a\") == Some(\"b\")}\")\n    let a = pair(1, 2)\n    let b = pair(1, 2)\n    let c = pair(1, 9)\n    var rev = dict.new()\n    dict.insert(rev, \"j\", 2)\n    dict.insert(rev, \"k\", 1)\n    console.print(\"${a == b}\")\n    console.print(\"${a == c}\")\n    console.print(\"${a == rev}\")\n";
        let want = vec![
            "true".to_string(),
            "false".to_string(),
            "false".to_string(),
            "true".to_string(),
            "true".to_string(),
            "false".to_string(),
            "true".to_string(),  // identical insert order + contents
            "false".to_string(), // differing value
            "true".to_string(),  // same pairs, different insertion order
        ];
        // Dict `==` now lowers on the binary path as a content comparison,
        // matching the interpreter and the std `PartialEq` contract.
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
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

    /// `trim` must strip exactly the same whitespace on both backends. The WASM
    /// `$is_ws` helper handles the 6 ASCII whitespace bytes (incl. VT/FF); Rust's
    /// `str::trim` would also strip Unicode whitespace (e.g. NBSP), which WASM does
    /// not — so the interpreter is pinned to the ASCII set. Here a NBSP (U+00A0)
    /// must survive on BOTH backends, while VT/FF are stripped by both. (Regression
    /// for a silent Unicode-whitespace trim divergence.)
    #[test]
    fn trim_whitespace_set_agrees_on_both_backends() {
        // "  \t\n hi \r\x0b" -> "hi"; "\x0c x \x0c" -> "x"; NBSP stays around 'y'.
        let src = "fn main(console: Console):\n    console.print(\"[\" + \"  \\t\\n hi \\r\u{0b}\".trim() + \"]\")\n    console.print(\"[\" + \"\u{0c} x \u{0c}\".trim() + \"]\")\n    console.print(\"[\" + \"\u{a0}y\u{a0}\".trim() + \"]\")\n";
        let want = vec![
            "[hi]".to_string(),
            "[x]".to_string(),
            "[\u{a0}y\u{a0}]".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// Dict `update` (single-lookup upsert) must agree on both backends, including
    /// nested updates and a big-`Int` value. WASM lowers it to a `$dict_update`
    /// helper that reads the current value (or default), applies the closure via
    /// `call_indirect`, and reinserts — equivalent to the interpreter's
    /// `dict.insert(d, k, f(dict.get_or(d, k, default)))`. (Regression for the
    /// interpreter-only dict-upsert gap.)
    #[test]
    fn dict_update_upsert_agrees_on_both_backends() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    dict.insert(d, \"a\", 1)\n    dict.insert(d, \"b\", 2)\n    dict.update(d, \"a\", 0, fn(x: Int): x + 10)\n    dict.update(d, \"c\", 100, fn(x: Int): x + 1)\n    console.print(\"${dict.get_or(d, \"a\", -1)}\")\n    console.print(\"${dict.get_or(d, \"b\", -1)}\")\n    console.print(\"${dict.get_or(d, \"c\", -1)}\")\n    console.print(\"${dict.length(d)}\")\n    var counts = dict.new()\n    dict.update(counts, \"hit\", 0, fn(n: Int): n + 1)\n    dict.update(counts, \"hit\", 0, fn(n: Int): n + 1)\n    console.print(\"${dict.get_or(counts, \"hit\", -1)}\")\n";
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

    /// `std/dict` adds the compositional layer over the builtin Dict: a `get`
    /// returning `Option`, `from_pairs`, and the `map_values`/`filter`/`merge`
    /// transforms — verified against the builtin `size`/`get_or`.
    #[test]
    fn dict_module_higher_level_operations() {
        let src = r#"import dict

fn main(console: Console):
    let d = dict.from_pairs([("a", 1), ("b", 2), ("c", 3)])
    console.print("${dict.length(d)}")
    console.print(oi(d.get("b")))
    console.print(oi(d.get("z")))
    let m = d.merge(dict.from_pairs([("b", 20), ("d", 4)]))
    console.print("${dict.get_or(m, "b", 0)}" + "," + "${dict.get_or(m, "d", 0)}")
    let tens = d.map_values(fn(v: Int): v * 10)
    console.print(oi(tens.get("c")))
    let evens = d.filter(fn(k: String, v: Int): v % 2 == 0)
    console.print("${dict.length(evens)}")
    let fresh: Dict(String, Int) = dict.new()
    console.print(bs(fresh.is_empty()))

fn oi(o: Option(Int)) -> String:
    match o:
        Some(n) -> "${n}"
        None -> "none"

fn bs(b: Bool) -> String:
    if b: "yes" else: "no"
"#;
        assert_eq!(
            link_run(src),
            vec!["3", "2", "none", "20,4", "30", "1", "yes"]
        );
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
    console.print("${list.sum(xs)}")
    console.print("${list.is_empty(xs)}")
    console.print("${list.is_empty(list.filter(xs, fn(n: Int): (n > 100)))}")
    console.print("${list.count_where(xs, fn(n: Int): ((n % 2) == 0))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(
            interpreted, compiled,
            "std list library diverged between interpreter and compiled"
        );
    }

    #[test]
    fn std_list_transformations_backends_agree() {
        // RFC-0049: `find_index` is deleted; `position` is the by-PREDICATE
        // Option-index form (it took over the role find_index vacated).
        // `?? -1` (RFC-0048) recovers the old sentinel for a compact assertion.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 10, 4, 5]
    console.print("${list.position(xs, fn(n: Int): (n > 5)) ?? -1}")
    console.print("${list.position(xs, fn(n: Int): (n > 100)) ?? -1}")
    console.print("${list.position(xs, fn(n: Int): (n == 1)) ?? -1}")
    let sums = list.zip_with([1, 2, 3], [10, 20], fn(a: Int, b: Int): (a + b))
    console.print("${list.length(sums)}")
    console.print("${list.sum(sums)}")
    let spaced = list.intersperse([5, 6, 7], 0)
    console.print("${list.length(spaced)}")
    console.print("${list.sum(spaced)}")
    console.print("${list.length(list.intersperse([9], 0))}")
    console.print("${list.length(list.intersperse([], 0))}")
    console.print("${list.sum(list.take_while(xs, fn(n: Int): (n < 5)))}")
    console.print("${list.sum(list.drop_while(xs, fn(n: Int): (n < 5)))}")
    let threes = list.repeat(7, 3)
    console.print("${list.sum(threes)}")
    console.print("${list.length(threes)}")
    console.print("${list.length(list.repeat(9, 0))}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "list transformations diverged");
        assert_eq!(compiled, vec!["3", "-1", "0", "2", "33", "5", "18", "1", "0", "6", "19", "21", "3", "0"]);
    }

    // flatten collapses Option(Option(a)) one level; zip pairs two options into
    // Option((a, b)) only when both are Some. Both backends agree.
    #[test]
    fn std_option_flatten_zip_backends_agree() {
        let client = r#"
import option

fn nested(n: Int) -> Option(Option(Int)):
    if (n > 0):
        Some(Some(n))
    else:
        Some(None)

fn main(console: Console):
    console.print("${option.unwrap_or(option.flatten(nested(7)), (0 - 1))}")
    console.print("${option.unwrap_or(option.flatten(nested(0)), (0 - 1))}")
    match option.zip(Some(3), Some(4)):
        Some(pair) ->
            let (x, y) = pair
            console.print("${(x + y)}")
        None -> console.print("none")
    console.print("${option.is_none(option.zip(Some(1), None))}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option flatten/zip diverged");
        assert_eq!(compiled, vec!["7", "-1", "7", "true"]);
    }

    #[test]
    fn std_option_or_mapor_backends_agree() {
        // The fallback combinators: `or` / `or_else` keep a Some or supply an
        // alternative (eagerly / lazily), and `map_or` transforms a Some or
        // returns the default for None. Both backends agree.
        let client = r#"
import option

fn main(console: Console):
    console.print("${option.unwrap_or(option.or(Some(5), Some(9)), 0)}")
    console.print("${option.unwrap_or(option.or(None, Some(9)), 0)}")
    console.print("${option.unwrap_or(option.or_else(None, fn(): Some(7)), 0)}")
    console.print("${option.unwrap_or(option.or_else(Some(3), fn(): Some(7)), 0)}")
    console.print("${option.map_or(Some(10), 0, fn(x: Int): (x * 2))}")
    console.print("${option.map_or(None, 99, fn(x: Int): (x * 2))}")
"#;
        let sources = [("option", crate::bundled_module("option").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "option or/map_or diverged");
        assert_eq!(compiled, vec!["5", "9", "7", "3", "20", "99"]);
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

    /// The first-class `Set(a)` type: construction, membership, `for x in set`
    /// iteration (IntoIter-style), removal, and collecting an iterator into a set
    /// (`set.from_list(iter.collect(...))`) — identical on both backends.
    #[test]
    fn std_set_type_iteration_and_collect_agree() {
        let client = "import set\nimport iter\nimport show\n\nfn main(console: Console):\n    var s = set.from_list([3, 1, 2, 3, 1])\n    console.print(\"${set.length(s)}\")\n    console.print(\"${set.contains(s, 2)}\")\n    var total = 0\n    for x in s:\n        total = (total + x)\n    console.print(\"${total}\")\n    set.remove(s, 2)\n    console.print(show(s))\n    let cs: Set(Int) = iter.collect(iter.range(1, 4))\n    console.print(show(cs))\n";
        let sources = [
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("iter", crate::bundled_module("iter").unwrap()),
            ("set", crate::bundled_module("set").unwrap()),
            ("show", crate::bundled_module("show").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "set type diverged");
        assert_eq!(compiled, vec!["3", "true", "6", "{3, 1}", "{1, 2, 3}"]);
    }

    #[test]
    fn set_symmetric_difference_and_disjoint_backends_agree() {
        // symmetric_difference composes difference+union (so it de-dups);
        // is_disjoint is true exactly when the intersection is empty.
        let client = r#"
import set
import list
fn show_ints(xs: List(Int)) -> String:
    list.join(list.map(xs, fn(n: Int): "${n}"), ",")
fn main(console: Console):
    let sd1 = set.symmetric_difference(set.from_list([1, 2, 3]), set.from_list([2, 3, 4]))
    let sd2 = set.symmetric_difference(set.from_list([1, 1, 2]), set.from_list([2, 2, 3]))
    console.print(show_ints(set.to_list(sd1)))
    console.print(show_ints(set.to_list(sd2)))
    let d1a = set.from_list([1, 2])
    console.print(if set.is_disjoint(d1a, set.from_list([3, 4])): "yes" else: "no")
    console.print(if set.is_disjoint(d1a, set.from_list([2, 3])): "yes" else: "no")
"#;
        let sources = [
            ("cmp", crate::bundled_module("cmp").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("set", crate::bundled_module("set").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "set ops diverged");
        assert_eq!(compiled, vec!["1,4", "1,3", "yes", "no"]);
    }

    #[test]
    fn list_transpose_backends_agree() {
        // transpose swaps rows and columns; a ragged input is truncated to the
        // shortest row, and an empty input gives an empty result.
        let client = r#"
import list
fn show_row(r: List(Int)) -> String:
    list.join(list.map(r, fn(n: Int): "${n}"), ",")
fn show_grid(g: List(List(Int))) -> String:
    list.join(list.map(g, show_row), ";")
fn main(console: Console):
    console.print(show_grid(list.transpose([[1, 2, 3], [4, 5, 6]])))
    console.print(show_grid(list.transpose([[1, 2], [3, 4, 5]])))
    console.print(show_grid(list.transpose([])))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "transpose diverged");
        assert_eq!(compiled, vec!["1,4;2,5;3,6", "1,3;2,4", ""]);
    }

    #[test]
    fn result_partition_backends_agree() {
        // partition splits a list of Results into the Ok values and the Err
        // values, each in order.
        let client = r#"
import result
import list
fn main(console: Console):
    let (oks, errs) = result.partition([Ok(1), Err("a"), Ok(2), Err("b"), Ok(3)])
    console.print(list.join(list.map(oks, fn(n: Int): "${n}"), ","))
    console.print(list.join(errs, ","))
"#;
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("result", crate::bundled_module("result").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", client),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result.partition diverged");
        assert_eq!(compiled, vec!["1,2,3", "a,b"]);
    }

    #[test]
    fn subscript_example_runs_on_wasm() {
        // `xs[i]` desugars to `list.at(xs, i)`; chained subscripts index nested lists.
        // The dot product and 2D-grid diagonal match on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../../examples/subscript/src/subscript.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "subscript diverged");
        assert_eq!(
            compiled,
            vec!["dot = 32", "grid[1][2] = 6", "diagonal sum = 15"]
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
    fn std_result_or_mapor_backends_agree() {
        // The fallback combinators mirror Option's: `or` / `or_else` keep an Ok
        // or supply an alternative (eagerly / error-aware lazily), and `map_or`
        // transforms an Ok or returns the default for an Err. Both backends agree.
        let client = r#"
import result

fn checked(n: Int) -> Result(Int, String):
    if (n > 0):
        Ok(n)
    else:
        Err("bad")

fn main(console: Console):
    console.print("${result.unwrap_or(result.or(checked(5), Ok(9)), 0)}")
    console.print("${result.unwrap_or(result.or(checked((0 - 1)), Ok(9)), 0)}")
    console.print("${result.unwrap_or(result.or_else(checked((0 - 1)), fn(e: String): Ok(e.length())), 0)}")
    console.print("${result.map_or(checked(5), 0, fn(x: Int): (x * 2))}")
    console.print("${result.map_or(checked((0 - 1)), 99, fn(x: Int): (x * 2))}")
"#;
        let sources = [("result", crate::bundled_module("result").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "result or/map_or diverged");
        assert_eq!(compiled, vec!["5", "9", "3", "10", "99"]);
    }

    /// `$dir_list` (list a directory) on the binary path — the host reports the
    /// marshaled-list size (`dir_list_size`) then writes it (`write_pending_list`),
    /// gated behind Dir(Read). Counts the directory's entries.
    #[test]
    fn wir_dir_list_host_import_binary_path() {
        use crate::runtime::{Capabilities, Runtime};
        let root = std::env::temp_dir().join(format!("witchy_wir_dirlist_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("mkdir");
        std::fs::write(root.join("one.txt"), "1").expect("write");
        std::fs::write(root.join("two.txt"), "2").expect("write");
        let src = "fn main(console: Console, dir: Dir[Read]):\n    console.print(\"${list.length(dir.list())}\")\n";
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should handle dir list via the host imports");
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
        assert_eq!(got, vec!["2".to_string()], "binary path: 2 entries");
    }

    #[test]
    fn dict_undetermined_key_is_rejected() {
        // A key with no `Eq` implementation errors clearly
        // rather than picking a wrong comparison.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, console, 5)
    console.print("${dict.length(d)}")
"#;
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("the generic Dict surface remains type-checkable");
        let err = codegen::compile_module_binary(&linked).expect_rejected("should reject");
        assert!(
            err.to_string().contains("Dict key type"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn dict_remove_backends_agree() {
        // `remove` (string and int keys) — present, absent, and the surviving
        // entries — agrees across the interpreter and compiled backends.
        let src = r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    dict.insert(d, "c", 3)
    var d2 = d
    dict.remove(d2, "b")
    console.print("${dict.length(d2)}")
    console.print("${if dict.contains_key(d2, "b"): 1 else: 0}")
    console.print("${dict.get_or(d2, "a", 0)}")
    console.print("${dict.get_or(d2, "c", 0)}")
    var d3 = d
    dict.remove(d3, "missing")
    console.print("${dict.length(d3)}")
    console.print("${dict.length(d)}")
    var nums = dict.new()
    dict.insert(nums, 10, 100)
    dict.insert(nums, 20, 200)
    var nums2 = nums
    dict.remove(nums2, 10)
    console.print("${dict.length(nums2)}")
    console.print("${dict.get_or(nums2, 20, 0)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["2", "0", "1", "3", "3", "3", "1", "200"]);
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
    dict.insert(b, "x", 1)
    dict.remove(b, "x")
    dict.insert(b, "x", 5)
    console.print("${dict.get_or(b, "x", -1)}")
    console.print("${list.length(dict.keys(b))}")
    console.print("${dict.get_or(b, "x", -1)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "compiled backend diverges from the interpreter oracle");
        assert_eq!(run_on_wasm(src), vec!["5", "1", "5"]);
    }

    #[test]
    fn std_list_partition_unzip_backends_agree() {
        // partition splits by a predicate in one pass; unzip is the inverse of
        // zip. Both return tuples of lists, so this also exercises tuple-valued
        // returns from generic std functions across backends.
        let client = r#"
import list

fn main(console: Console):
    let xs = [1, 2, 3, 4, 5, 6]
    let (evens, odds) = list.partition(xs, fn(n: Int): ((n % 2) == 0))
    console.print("${list.sum(evens)}")
    console.print("${list.sum(odds)}")
    let pairs = list.zip([10, 20, 30], [1, 2, 3])
    let (a, b) = list.unzip(pairs)
    console.print("${list.sum(a)}")
    console.print("${list.sum(b)}")
"#;
        let sources = [("list", crate::bundled_module("list").unwrap()), ("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "partition/unzip diverged between backends");
        assert_eq!(compiled, vec!["12", "9", "60", "6"]);
    }

    // Integer division/modulo truncate toward zero, and their signs must agree
    // for negative operands across the i64 interpreter and i32 codegen (the
    // results here stay well within i32). Also locks in dict insert-overwrite,
    // removing an absent key, and `get_or`'s default path.
    #[test]
    fn negative_arithmetic_and_dict_mutation_backends_agree() {
        let src = r#"
fn main(console: Console):
    console.print("${(0 - (7 / 2))}")
    console.print("${((0 - 7) % 2)}")
    console.print("${(7 / (0 - 2))}")
    console.print("${(7 % (0 - 2))}")
    console.print("${((0 - 7) / (0 - 2))}")
    var d = dict.new()
    dict.insert(d, "k", 1)
    dict.insert(d, "k", 2)
    console.print("${dict.get_or(d, "k", 0)}")
    console.print("${dict.length(d)}")
    dict.remove(d, "missing")
    console.print("${dict.length(d)}")
    console.print("${dict.get_or(d, "absent", 99)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "int/dict edges diverged");
    }
