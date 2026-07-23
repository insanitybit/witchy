use super::*;
use crate::{ast, codegen, parser, typeck};

    /// (BUG-428) Generator lowering must not erase the source-level
    /// no-`yield`-inside-`region:` safety rule before type checking can enforce
    /// it.
    #[test]
    fn gen_fn_rejects_yield_inside_region_before_lowering() {
        for body in [
            "region:\n        yield 1\n        0",
            "region:\n        if true:\n            yield 1\n        0",
            "if true:\n        region:\n            yield 1\n            0",
        ] {
            let src = format!(
                "import iter\n\ngen fn bad() -> Iter(Int):\n    {body}\n    yield 2\n\nfn main(console: Console):\n    let xs: List(Int) = iter.collect(bad())\n    console.print(\"${{xs}}\")\n"
            );
            let module = parser::parse_module(&src).expect("parse");
            let err = crate::pipeline::link(vec![("main".into(), module)], "main")
                .expect_err("yield inside region must be rejected during generator lowering");
            assert!(
                err.message.contains("cannot `yield` inside `region:`")
                    && err.message.contains("generator frame"),
                "diagnostic should explain the region/generator safety rule, got: {}",
                err.message
            );
        }
    }

    /// Misusing the ownership conventions is rejected up front by the type checker
    /// (so the same program fails on every backend, never just native): using a
    /// value after it was consumed by `own`, or after `move`. A bare `let` borrow
    /// imposes no such restriction.
    #[test]
    fn conventions_reuse_after_move_rejected() {
        // Reuse after an `own` parameter consumes it.
        let after_own = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    c.print(\"${drain(d)}\")\n    c.print(\"${list.length(d)}\")\n";
        let e1 = typeck::check_str(after_own).expect_err("reuse after own should fail");
        assert!(e1.to_string().contains("after it was moved"), "got: {e1:?}");
        // Reuse after an explicit `move`.
        let after_move = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    c.print(\"${drain(move d)}\")\n    c.print(\"${list.length(d)}\")\n";
        assert!(
            typeck::check_str(after_move).is_err(),
            "reuse after move should fail"
        );
        // A `let` borrow does NOT consume — reuse is fine.
        let after_borrow = "fn peek(let xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    c.print(\"${peek(d)}\")\n    c.print(\"${list.length(d)}\")\n";
        assert!(typeck::check_str(after_borrow).is_ok(), "borrow reuse should be fine");
    }

    /// An alias RE-TAKEN inside the loop forces the copying path each
    /// iteration (the kill re-zeroes the token) — correct, O(n) re-owns, and
    /// exactly what the cliff diagnostic exists to flag.
    #[test]
    fn analysis_alias_inside_loop_reowns_per_iteration() {
        let src = "fn main(console: Console):\n    var ys = []\n    var last = [9]\n    var j = 0\n    while j < 200:\n        list.push(ys, j)\n        last = ys\n        j = j + 1\n    console.print(\"${list.length(last)}\")\n";
        let want = vec!["200".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns >= 150, "every iteration must re-own, got {reowns}");
    }

    /// THE OWN-ABI: `xs = grow(move xs, i)` is a linear pipeline — the
    /// ownership token crosses the call in both directions (an extra cap
    /// param and result), so a cross-function builder stays O(n). Without
    /// the transfer each call re-owned by copy: O(n²) — the reowns counter
    /// (not timing) is the proof. (The interpreter leg stays small: it
    /// clones at every call by design.)
    #[test]
    fn analysis_own_abi_pipelines_in_place() {
        let src = "fn grow(own xs: List(Int), n: Int) -> List(Int):\n    list.push(xs, n)\n    xs\n\nfn main(console: Console):\n    var xs = [0]\n    var i = 0\n    while i < 3000:\n        xs = grow(move xs, i)\n        i = i + 1\n    console.print(\"${list.length(xs)}\")\n    console.print(\"${list.at(xs, 3000)}\")\n";
        let want = vec!["3001".to_string(), "2999".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "the token must survive the calls, got {reowns} re-owns");
    }

    /// (RFC-0035) LAST-USE DROP — observable + differential. A dead per-iteration
    /// scratch buffer (`list.concat` read exactly once, then dead) sits in a loop that
    /// is NOT arena-resettable: the dict `acc` escapes each iteration, so the RFC-0030
    /// watermark is OFF and the scratch would otherwise leak O(iterations). Under
    /// `rc-floor` the `last_use` analysis frees it right after its last use. Three
    /// obligations, all asserted: (1) output IDENTICAL to the interpreter oracle and to
    /// the default build — the free is sound, never observable; (2) the free-list is
    /// actually recycled (`rc_reused_bytes > 0`) — the drop fired, it is not a no-op;
    /// (3) the heap frontier stays flat instead of growing with the leak — an order of
    /// magnitude below the default. This is exactly the niche the heap-reset-boundary
    /// guard (`wm_level == 0`) preserves: rc-floor reclaims where the watermark cannot,
    /// and cedes (never double-frees) where it can.
    #[test]
    fn rc_floor_last_use_drop_is_differential_and_bounds_the_leak() {
        use crate::opt::{self, Opt, OptSet};
        let src = "import list\nimport dict\nfn main(console: Console):\n    var acc = dict.new()\n    var i = 0\n    let base = [1, 2, 3, 4, 5]\n    while i < 2000:\n        let scratch = list.concat(base, base)\n        let n = list.length(scratch)\n        dict.insert(acc, i % 8, n)\n        i = i + 1\n    console.print(\"${dict.length(acc)}\")\n";
        let oracle = link_run(src);

        // rc-floor OFF (explicit — it is default-on now): correct, but the scratch leaks each iteration.
        opt::set_for_tests(Some(OptSet::default_set().without(Opt::RcFloor)));
        let (default_out, default_heap, _default_reused) = wasm_run_heap(src);
        opt::set_for_tests(None);
        assert_eq!(default_out, oracle, "default build diverged from the interpreter oracle");

        // rc-floor ON: identical output, heap bounded, free-list recycled.
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        let (rc_out, rc_heap, rc_reused) = wasm_run_heap(src);
        opt::set_for_tests(None);
        assert_eq!(rc_out, oracle, "rc-floor diverged from the interpreter oracle");
        assert_eq!(rc_out, default_out, "rc-floor changed observable output — unsound");
        assert!(rc_reused > 0, "rc-floor never recycled: the last_use drop did not fire (reused={rc_reused})");
        assert!(
            rc_heap.saturating_mul(10) < default_heap,
            "rc-floor did not bound the leak: rc_heap={rc_heap} default_heap={default_heap}"
        );
    }

    /// (RFC-0051 I1 / SEC-039) Regression: the free-at-overwrite path must not free a
    /// non-owning-object pointer. The 7-line repro — `var t = "abc"; t = t.trim()`
    /// — reassigns a `var` whose FIRST buffer is a string LITERAL (a data-segment pointer
    /// BELOW `heap_base`, not an `$rc_alloc` object). Under `inplace + rc-floor` the
    /// free-at-overwrite emitted `$rc_free(old)` directly on that literal; `$rc_free` had
    /// NO `heap_base` guard (only `$dup`/`$drop` did), so it linked the literal into the
    /// free-list and corrupted its length word — a later `$rc_alloc` reuse handed out the
    /// poisoned pointer and `string.trim`'s result rendered MEGABYTES of raw heap
    /// (an in-guest disclosure). I1's categorical `ptr >= heap_base` floor on `$rc_free`
    /// (matching `$dup`/`$drop`) kills the class. Assert byte-identical output across the
    /// FULL opt sweep — the leak fired under `rc-floor` alone, so the sweep is the net.
    #[test]
    fn rc_free_at_overwrite_does_not_free_a_literal_sec_039() {
        use crate::opt::{self, Opt, OptSet};
        let src = "fn main(console: Console):\n    var xs = [3, 1, 2]\n    list.sort(xs)\n    console.print(\"${xs}\")\n    var t = \"abc\"\n    t = t.trim()\n    console.print(\"[${t}]\")\n";
        let oracle = link_run(src);
        assert_eq!(oracle, vec!["[1, 2, 3]", "[abc]"], "oracle shape changed");
        let mut settings: Vec<(String, OptSet)> = vec![
            ("none".into(), OptSet::none()),
            ("all".into(), OptSet::all()),
            ("default".into(), OptSet::default_set()),
        ];
        for o in Opt::ALL {
            settings.push((format!("-{}", o.name()), OptSet::default_set().without(o)));
        }
        for (label, set) in settings {
            opt::set_for_tests(Some(set));
            let out = wasm_run(src);
            opt::set_for_tests(None);
            assert_eq!(out, oracle, "SEC-039: WITCHY_OPT={label} leaked/diverged (freed a non-object literal)");
        }
    }

    /// `region:` Phase 1 (rfcs/regions.md): the syntax parses (with optional
    /// `-> T` ascription), the block's value escapes, scalar outer
    /// assignments are allowed, and both backends agree — a region NEVER
    /// changes observable behavior, only when memory is reclaimed.
    #[test]
    fn region_blocks_value_escape_and_parity() {
        let src = "\nfn main(console: Console):\n    let summary: String = region:\n        var parts = []\n        for i in 0..50:\n            list.push(parts, \"${i}\")\n        list.join(parts, \",\")\n    console.print(\"${summary.length()}\")\n    var n = 0\n    let direct = region -> Int:\n        n = n + 42\n        n\n    console.print(\"${direct}\")\n";
        let want: Vec<String> = ["139", "42"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `region:` Phase 2: the copy-out handles every shape — string, record
    /// with a nested list, recursive generic ADT, dict (whose hidden index is
    /// dropped on the way out), nested regions — and parent-side values pass
    /// through shared, all agreeing with the interpreter.
    #[test]
    fn region_copy_out_shapes_agree_on_both_backends() {
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\ntype Reading:\n    sensor: String\n    values: List(Int)\n\nfn main(console: Console):\n    let st = region -> Stack(Int):\n        Push(1, Push(2, Empty))\n    console.print(\"${st == Push(1, Push(2, Empty))}\")\n    let r = region -> Reading:\n        var vs = []\n        for i in 0..50:\n            list.push(vs, i * i)\n        Reading(sensor: \"t\" + \"0\", values: vs)\n    console.print(r.sensor)\n    console.print(\"${list.at(r.values, 49)}\")\n    let d = region -> Dict(String, Int):\n        var m = dict.new()\n        for i in 0..100:\n            dict.insert(m, \"k\" + \"${i}\", i)\n        m\n    console.print(\"${dict.get_or(d, \"k42\", 0 - 1)}\")\n    let shared = \"parent-side\"\n    let s = region -> String:\n        shared\n    console.print(s)\n    let nested = region -> Int:\n        let inner: String = region -> String:\n            \"abc\" + \"def\"\n        inner.length()\n    console.print(\"${nested}\")\n";
        let want: Vec<String> = ["true", "t0", "2401", "42", "parent-side", "6"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// `region:` reclamation: 100k regions each churning a 1000-element
    /// throwaway list (~1.6 GB cumulative, past the 1 GB cap) run in constant
    /// memory — inside a loop the automatic reset cannot help (an outer list
    /// grows), so only the region machinery reclaims. WASM-only for speed.
    #[test]
    fn region_reclaims_inside_nonresettable_loops() {
        let src = "fn main(console: Console):\n    var total = 0\n    var keep = []\n    for i in 0..100000:\n        let last = region -> Int:\n            var row = []\n            var j = 0\n            for j in 0..1000:\n                list.push(row, j)\n            list.at(row, 999)\n        total = total + last\n        list.push(keep, i)\n    console.print(\"${total}\")\n    console.print(\"${list.length(keep)}\")\n";
        assert_eq!(wasm_run(src), vec!["99900000", "100000"]);
    }

    /// `region:` Phase 3: the `__region_copy_bytes` counter proves the
    /// watermark short-circuit — a parent-side passthrough copies ZERO bytes,
    /// and a region-born string copies exactly its own block.
    #[test]
    fn region_copy_counter_proves_passthrough_is_free() {
        use crate::runtime::{Capabilities, Runtime};
        let run_and_count = |src: &str| -> (Vec<String>, i64) {
            let module = parser::parse_module(src).expect("parse");
            let bytes = codegen::compile_module_binary(&module)
                .expect_lowered("the binary path lowers this program");
            let mut rt = Runtime::batch().expect("rt");
            let mut actor = rt
                .spawn(
                    &bytes,
                    Capabilities { print: true, quiet: true, ..Default::default() },
                    64,
                )
                .expect("spawn");
            actor.run().expect("run");
            (actor.output(), actor.region_copy_bytes().expect("counter"))
        };
        // Parent-side value: shared, not copied.
        let (out, copied) = run_and_count(
            "fn main(console: Console):\n    let shared = \"twelve chars\"\n    let s = region -> String:\n        shared\n    console.print(s)\n",
        );
        assert_eq!(out, vec!["twelve chars"]);
        assert_eq!(copied, 0, "parent passthrough must copy nothing");
        // Region-born value: exactly its own block (4-byte header + 6 bytes).
        let (out, copied) = run_and_count(
            "fn main(console: Console):\n    let s = region -> String:\n        \"abc\" + \"def\"\n    console.print(s)\n",
        );
        assert_eq!(out, vec!["abcdef"]);
        assert_eq!(copied, 10, "a region-born string copies header + bytes");
    }

    /// (BUG-407) A `region -> SomeRecord:` result reclaims on the compiled backend
    /// (records were silently falling back to a plain block with NO reclaim). The
    /// `__region_copy_bytes` counter proves the record's block is copied out (> 0,
    /// was 0), and the value agrees with the interpreter.
    #[test]
    fn region_copy_out_reclaims_record_result() {
        use crate::runtime::{Capabilities, Runtime};
        let src = "type Point:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let p = region -> Point:\n        var acc = Point(x: 0, y: 0)\n        for i in [1, 2, 3, 4, 5]:\n            acc = Point(x: acc.x + i, y: acc.y + i * 2)\n        acc\n    console.print(\"${p.x}\")\n    console.print(\"${p.y}\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("lowers");
        let mut rt = Runtime::batch().expect("rt");
        let mut actor = rt.spawn(&bytes, Capabilities { print: true, quiet: true, ..Default::default() }, 64).expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), vec!["15", "30"]);
        assert!(actor.region_copy_bytes().expect("counter") > 0, "record region must copy its block out (was 0 = plain-block fallback)");
        assert_eq!(link_run(src), vec!["15", "30"], "interp agrees on the value");
    }

    /// `region:` rejections: an outer pointer-typed assignment and a `yield`
    /// are type errors — the region's only pointer escape is its value.
    #[test]
    fn region_rejects_outer_pointer_assign_and_yield() {
        let leak = "fn main(console: Console):\n    var leak = [1]\n    let x = region:\n        list.push(leak, 2)\n        7\n    console.print(\"${x}\")\n";
        let module = parser::parse_module(leak).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        let err = typeck::check(&linked).expect_err("outer pointer assign must be rejected");
        assert!(err.to_string().contains("inside `region:`"), "{err}");
    }

    /// IN-PLACE SET_AT: `xs = list.set_at(xs, i, v)` mutates the owned buffer's
    /// slot in place (O(1)) via `$list_set_cap`, instead of rebuilding the whole
    /// list each set — which is O(n^2) memory that traps the WASM bump allocator
    /// at ~10k. An aliased list keeps the copying set_at (the alias still sees the
    /// original); a set does not change the length. (An out-of-range index traps —
    /// see `oob_list_set_at_traps_on_both_backends`, BUG-315.)
    #[test]
    fn inplace_set_at_is_fast_and_alias_safe() {
        let src = |n: u32| {
            format!(
                "fn main(console: Console):\n    var xs = []\n    for i in 0..{n}:\n        list.push(xs, 0)\n    var k = 0\n    while k < {n}:\n        list.set_at(xs, k, k * 2)\n        k = k + 1\n    console.print(\"${{list.at(xs, {last})}}\")\n    list.set_at(xs, {last}, 7)\n    console.print(\"${{list.length(xs)}}\")\n    var ys = [1, 2, 3]\n    let alias = ys\n    list.set_at(ys, 1, 99)\n    console.print(\"${{list.at(ys, 1)}}\")\n    console.print(\"${{list.at(alias, 1)}}\")\n",
                last = n - 1
            )
        };
        let want = |n: u32| -> Vec<String> {
            vec![((n - 1) * 2).to_string(), n.to_string(), "99".to_string(), "2".to_string()]
        };
        // Parity (incl. alias semantics) on both backends at small n; the O(n^2)
        // rebuild trap is compiled-only, so only WASM pays the at-scale run.
        assert_eq!(link_run(&src(500)), want(500), "interpreter");
        assert_eq!(wasm_run(&src(500)), want(500), "compiled WASM must agree");
        assert_eq!(wasm_run(&src(5000)), want(5000), "compiled at 5k must stay in place");
    }

    /// IN-PLACE UPDATE_AT: `xs = list.update_at(xs, i, f)` applies the closure to
    /// the owned buffer's slot in place (O(1)) via `$list_update_cap`, instead of
    /// rebuilding the whole list each update (O(n^2), OOM-prone). Alias-safe (a
    /// shared list keeps the copy); an update does not change the length. (An
    /// out-of-range index traps — see `oob_list_set_at_traps_on_both_backends`, BUG-315.)
    #[test]
    fn inplace_update_at_is_fast_and_alias_safe() {
        let src = |n: u32| {
            format!(
                "fn main(console: Console):\n    var xs = []\n    for i in 0..{n}:\n        list.push(xs, 1)\n    var k = 0\n    while k < {n}:\n        list.update_at(xs, k, fn(v: Int): v + 1)\n        k = k + 1\n    console.print(\"${{list.at(xs, {last})}}\")\n    list.update_at(xs, {last}, fn(v: Int): v + 1)\n    console.print(\"${{list.length(xs)}}\")\n    var ys = [1, 2, 3]\n    let alias = ys\n    list.update_at(ys, 1, fn(v: Int): v + 100)\n    console.print(\"${{list.at(ys, 1)}}\")\n    console.print(\"${{list.at(alias, 1)}}\")\n",
                last = n - 1
            )
        };
        let want = |n: u32| -> Vec<String> {
            vec!["2".to_string(), n.to_string(), "102".to_string(), "2".to_string()]
        };
        // Parity (incl. alias semantics) on both backends at small n; the O(n^2)
        // rebuild trap is compiled-only, so only WASM pays the at-scale run.
        assert_eq!(link_run(&src(500)), want(500), "interpreter");
        assert_eq!(wasm_run(&src(500)), want(500), "compiled WASM must agree");
        assert_eq!(wasm_run(&src(5000)), want(5000), "compiled at 5k must stay in place");
    }

    /// IN-PLACE DICT INSERT: `d = dict.insert(d, k, v)` updates/appends into owned
    /// entry slack (no per-insert table copy); an aliased dict keeps the
    /// copying insert, so the alias still sees the original.
    #[test]
    fn inplace_dict_insert_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..2000:\n        dict.insert(d, i, i * 2)\n    console.print(\"${dict.length(d)}\")\n    console.print(\"${dict.get_or(d, 1999, 0 - 1)}\")\n    var e = dict.new()\n    let alias = e\n    dict.insert(e, 1, 10)\n    console.print(\"${dict.length(alias)}\")\n    console.print(\"${dict.length(e)}\")\n";
        let want: Vec<String> =
            ["2000", "3998", "0", "1"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE STRING APPEND: the builder pattern `s = s + piece` appends
    /// into owned byte slack (amortized O(1)); a literal-seeded alias keeps
    /// the copying path, so the interned literal is never mutated.
    #[test]
    fn inplace_string_append_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var s = \"\"\n    for i in 0..20000:\n        s = s + \"ab\"\n    console.print(\"${s.length()}\")\n    var t = \"seed\"\n    let alias = t\n    t = t + \"!\"\n    console.print(alias)\n    console.print(t)\n";
        let want: Vec<String> =
            ["40000", "seed", "seed!"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE PUSH (the linear-update optimization): an unaliased
    /// accumulate-in-loop appends into owned slack — 50k pushes complete
    /// instantly instead of O(n²) copying — while an ALIASED list keeps the
    /// copying push, so value semantics hold: `ys` still sees the original.
    #[test]
    fn inplace_push_is_fast_and_alias_safe() {
        // 50k would take minutes under clone-per-push on either backend; both
        // have an in-place fast path for the unaliased self-assign shape.
        let src = "fn main(console: Console):\n    var xs = []\n    for i in 0..50000:\n        list.push(xs, i)\n    console.print(\"${list.length(xs)}\")\n    console.print(\"${list.at(xs, 49999)}\")\n    var small = [1]\n    let alias = small\n    list.push(small, 2)\n    console.print(\"${alias}\")\n    console.print(\"${small}\")\n";
        let want: Vec<String> = ["50000", "49999", "[1]", "[1, 2]"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// IN-PLACE DICT ACCUMULATION: the `d = dict.insert(d, k, v)` and
    /// `d = dict.update(d, k, dflt, f)` self-assign shapes mutate the slot in place
    /// on both backends; an aliased dict keeps the copying path so value
    /// semantics hold.
    #[test]
    fn inplace_dict_upsert_is_fast_and_alias_safe() {
        let src = "fn main(console: Console):\n    var d = dict.new()\n    for i in 0..10000:\n        dict.insert(d, i, i)\n    console.print(\"${dict.length(d)}\")\n    var counts = dict.new()\n    for i in 0..30000:\n        dict.update(counts, i % 3, 0, fn(n: Int): n + 1)\n    console.print(\"${dict.get_or(counts, 0, 0)}\")\n    var small = dict.new()\n    dict.insert(small, 1, 10)\n    let alias = small\n    dict.insert(small, 2, 20)\n    console.print(\"${dict.length(alias)}\")\n    console.print(\"${dict.length(small)}\")\n";
        let want: Vec<String> = ["10000", "10000", "1", "2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    #[test]
    fn examples_agree_under_inplace_and_forced_copy() {
        // Metamorphic, NO-ORACLE codegen check: the in-place update machinery and
        // the forced-copy fallback are two lowerings of the same program and must
        // produce identical output. This catches an in-place aliasing bug on the
        // compiled backend WITHOUT consulting the interpreter — the kind of
        // self-consistency guard that lets the differential oracle be retired.
        // Restricted to console-only, `main`-bearing programs so output is
        // self-contained and deterministic.
        let entries = example_entries();
        let diverged: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    let Ok((linked, _)) = crate::link_file(p) else {
                        return None;
                    };
                    if typeck::check(&linked).is_err() {
                        return None;
                    }
                    let has_main = linked
                        .items
                        .iter()
                        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
                    let console_only = crate::capabilities::analyze(&linked)
                        .total
                        .keys()
                        .all(|k| *k == "Console");
                    if !has_main || !console_only || main_declares_console_read(&linked) {
                        return None;
                    }
                    let compile_with = |force_copy: bool| {
                        codegen::set_force_copy_for_tests(Some(force_copy));
                        let bytes = codegen::compile_module_binary(&linked);
                        codegen::set_force_copy_for_tests(None);
                        bytes
                    };
                    if let (
                        codegen::LoweringOutcome::Lowered(inplace),
                        codegen::LoweringOutcome::Lowered(copy),
                    ) = (compile_with(false), compile_with(true)) {
                        let a = crate::run_wasm_bytes(&inplace);
                        let b = crate::run_wasm_bytes(&copy);
                        if a != b {
                            return Some(format!("{p}: in-place {a:?} vs forced-copy {b:?}"));
                        }
                    }
                    None
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            diverged.is_empty(),
            "in-place and forced-copy codegen diverge:\n{}",
            diverged.join("\n")
        );
    }

    /// Criterion-3 MEASURABLE: a clean in-place accumulator builds in place on the
    /// binary path — amortized O(1) re-owns (the exported `__witchy_reowns`
    /// counter ≤ 2), not O(n) copies — and prints the right element.
    #[test]
    fn wir_inplace_accumulator_is_o1_reowns() {
        let src = "fn build(n: Int) -> List(Int):\n    var xs: List(Int) = []\n    for i in 0..n:\n        list.push(xs, i)\n    xs\n\nfn main(console: Console):\n    let ys = build(500)\n    console.print(\"${list.at(ys, 499)}\")\n";
        let want = vec!["499".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the accumulator program takes the WIR binary path");
        let (out, reowns) = binary_run_reowns(&bytes);
        assert_eq!(out, want, "binary output");
        assert!(reowns <= 2, "expected O(1) re-owns on the binary path, got {reowns}");
    }

    /// Criterion-3: an in-place accumulator (`xs = list.push(xs, i)` in a loop)
    /// lowers to the cap ABI (`$list_push_cap` via CallStoreMulti) on the binary
    /// path. Consumed via `list.at` so the whole program stays on the pruned
    /// binary path; runs identically to the interpreter oracle AND the WAT path.
    #[test]
    fn wir_inplace_accumulator_runs_and_agrees() {
        let src = "fn build(n: Int) -> List(Int):\n    var xs: List(Int) = []\n    for i in 0..n:\n        list.push(xs, i)\n    xs\n\nfn main(console: Console):\n    let ys = build(3)\n    console.print(\"${list.at(ys, 0)}\")\n    console.print(\"${list.at(ys, 1)}\")\n    console.print(\"${list.at(ys, 2)}\")\n";
        let want = vec!["0".to_string(), "1".to_string(), "2".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        assert_eq!(run_on_wasm(src), want, "legacy WAT path");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the accumulator program takes the WIR binary path");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path (in-place accumulator)");
    }

    /// The in-place DICT accumulator on the binary path: `d = dict.insert(d, k, v)`
    /// in a loop lowers to `$dict_insert_cap` (O(1) amortized into owned entry
    /// slack) instead of copying the whole dict each insert. Proven the same two
    /// ways as the list accumulator: the values agree with the interpreter, AND
    /// the observable `$__witchy_reowns` counter stays O(1) (one re-own, not one
    /// per insert) — the timing-free proof the copy-per-insert path was avoided.
    #[test]
    fn wir_inplace_dict_insert_is_o1_reowns() {
        let src = "fn build(n: Int) -> Dict(String, Int):\n    var d = dict.new()\n    for i in 0..n:\n        dict.insert(d, \"k\" + \"${i}\", i)\n    d\n\nfn main(console: Console):\n    let m = build(500)\n    console.print(\"${dict.get_or(m, \"k499\", 0 - 1)}\")\n    console.print(\"${dict.length(m)}\")\n";
        let want = vec!["499".to_string(), "500".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the dict accumulator program takes the WIR binary path");
        let (out, reowns) = binary_run_reowns(&bytes);
        assert_eq!(out, want, "binary output");
        assert!(reowns <= 2, "expected O(1) re-owns for the in-place dict insert, got {reowns}");
    }

    /// The in-place STRING builder on the binary path: `s = s + piece` in a loop
    /// lowers to `$str_append_cap` (append bytes into owned slack) instead of
    /// re-concatenating the whole string each statement. Proven both ways: values
    /// agree with the interpreter, AND `$__witchy_reowns` stays O(1).
    #[test]
    fn wir_inplace_str_append_is_o1_reowns() {
        let src = "fn build(n: Int) -> String:\n    var s = \"\"\n    var i = 0\n    while i < n:\n        s = s + \"x\"\n        i = i + 1\n    s\n\nfn main(console: Console):\n    let r = build(500)\n    console.print(\"${r.length()}\")\n";
        let want = vec!["500".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the string builder takes the WIR binary path");
        let (out, reowns) = binary_run_reowns(&bytes);
        assert_eq!(out, want, "binary output");
        assert!(reowns <= 2, "expected O(1) re-owns for the in-place string builder, got {reowns}");
    }

    /// The in-place dict.update accumulator (the word-count shape) on the binary
    /// path: `d = dict.update(d, k, dflt, f)` in a loop lowers to
    /// `$dict_update_cap` (apply the closure, reinsert into owned slack) instead
    /// of copying the dict each update. Values agree with the interpreter AND
    /// `$__witchy_reowns` stays O(1).
    #[test]
    fn wir_inplace_dict_update_is_o1_reowns() {
        let src = "fn build(n: Int) -> Dict(String, Int):\n    var d = dict.new()\n    var i = 0\n    while i < n:\n        dict.update(d, \"k\" + \"${i % 10}\", 0, fn(c: Int): c + 1)\n        i = i + 1\n    d\n\nfn main(console: Console):\n    let d = build(500)\n    console.print(\"${dict.get_or(d, \"k0\", 0 - 1)}\")\n    console.print(\"${dict.length(d)}\")\n";
        let want = vec!["50".to_string(), "10".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typeck");
        assert_eq!(link_run(src), want, "interpreter oracle");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the dict.update accumulator takes the WIR binary path");
        let (out, reowns) = binary_run_reowns(&bytes);
        assert_eq!(out, want, "binary output");
        assert!(reowns <= 2, "expected O(1) re-owns for the in-place dict update, got {reowns}");
    }

    /// An in-place accumulator INSIDE a lifted lambda on the binary path: the
    /// lambda's own `var acc = [...]` + self-push loop needs its `$acc__cap`
    /// ownership-token shadow declared as a local in the lifted `$__lamw{i}`. The
    /// builder snapshots the lambda's `inplace_push` set before restoring the
    /// enclosing function's, so the cap local isn't dropped (was: encode panic
    /// "unknown local $acc__cap"). (2-way: list.push isn't WAT-leg resolvable.)
    #[test]
    fn wir_lambda_inplace_accumulator_binary_path() {
        let src = "fn main(console: Console):\n    let build = fn(n: Int):\n        var acc = [0]\n        var t = 0\n        while t < n:\n            list.push(acc, t)\n            t = t + 1\n        list.length(acc)\n    console.print(\"${build(5)}\")\n";
        let want = vec!["6".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower a lambda-local accumulator");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }
