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

    /// Both `own` arguments and explicit `move` consume in every mode.
    /// A bare `let` borrow imposes no restriction.
    #[test]
    fn conventions_reuse_after_move_rejected() {
        // Passing a binding to `own` consumes it in both source modes.
        let after_own = "fn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    let d = [1, 2, 3]\n    c.print(\"${drain(d)}\")\n    c.print(\"${list.length(d)}\")\n";
        for mode in ["", "mode opt\n\n"] {
            let source = format!("{mode}{after_own}");
            let error = typeck::check_str(&source).expect_err("reuse after own should fail");
            assert!(error.to_string().contains("after it was moved"), "got: {error:?}");
        }
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
        let src = include_str!("../../tests/fixtures/rfc0162-sec/sec039-literal-overwrite.witchy");
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

    /// (SEC-078) A dependency call may return its String input unchanged. The
    /// returned binding therefore does not prove fresh ownership: reclaiming it
    /// at overwrite would free the publisher's retained alias and let the next
    /// same-sized allocation replace the publisher's trusted path.
    #[test]
    fn rc_free_at_overwrite_does_not_free_an_aliased_call_result_sec_078() {
        use crate::opt::{self, Opt, OptSet};
        // Recursive `slice` cannot be erased as a trivial identity call. Folding
        // is disabled so both concatenations allocate same-sized runtime buffers:
        // the pre-fix free-at-overwrite path recycled `trusted_path` for the
        // attacker allocation and printed `evil.txt` in the first slot.
        let src = "fn slice(text: String, depth: Int) -> String:\n    if depth <= 0:\n        text\n    else:\n        slice(text, depth - 1)\n\nfn suffix() -> String:\n    \".txt\"\n\nfn main(console: Console):\n    let trusted_path = \"safe\" + suffix()\n    var dependency_result = slice(trusted_path, 0)\n    dependency_result = string.substring(dependency_result, 1, string.length(dependency_result))\n    let attacker_allocation = \"evil\" + suffix()\n    console.print(trusted_path)\n    console.print(dependency_result)\n    console.print(attacker_allocation)\n";
        let oracle = link_run(src);
        assert_eq!(oracle, vec!["safe.txt", "afe.txt", "evil.txt"]);

        opt::set_for_tests(Some(OptSet::all().without(Opt::Fold)));
        let optimized = wasm_run(src);
        opt::set_for_tests(None);
        assert_eq!(optimized, oracle, "SEC-078: aliased call result freed publisher storage");

        // Pin the load-bearing optimizer judgment independently of allocator
        // reuse order: an ordinary call result is not authenticated fresh
        // storage, even when the call is direct and its result stays confined.
        let analysis_src = "fn identity(x: String) -> String:\n    x\nfn damage(x: String) -> ():\n    var working = identity(x)\n    working = string.substring(working, 1, string.length(working))\n";
        let module = parser::parse_module(analysis_src).expect("parse SEC-078 analysis fixture");
        let summaries = witchy_lower::analysis::Summaries::of_module(&module);
        let damage = module
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                ast::Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("damage function");
        let candidates = witchy_lower::escape::confined_reassigned_vars(damage, &summaries);
        assert!(
            !candidates.contains("working"),
            "SEC-078: arbitrary call result admitted as fresh storage: {candidates:?}"
        );
    }

    /// (SEC-078) Casing helpers are copy-on-write: an already-lowercase input
    /// is returned unchanged and therefore cannot authenticate fresh storage.
    #[test]
    fn rc_free_at_overwrite_does_not_free_a_cow_casing_alias_sec_078() {
        use crate::opt::{self, Opt, OptSet};
        let src = "fn suffix() -> String:\n    \".txt\"\n\nfn main(console: Console):\n    let trusted_path = \"safe\" + suffix()\n    var working = string.to_lower(trusted_path)\n    working = string.substring(working, 1, string.length(working))\n    let attacker_allocation = \"evil\" + suffix()\n    console.print(trusted_path)\n    console.print(working)\n    console.print(attacker_allocation)\n";
        let oracle = link_run(src);
        assert_eq!(oracle, vec!["safe.txt", "afe.txt", "evil.txt"]);

        opt::set_for_tests(Some(OptSet::all().without(Opt::Fold)));
        let optimized = wasm_run(src);
        opt::set_for_tests(None);
        assert_eq!(optimized, oracle, "SEC-078: COW casing result freed publisher storage");

        let analysis_src = "fn damage(x: String) -> ():\n    var working = string.to_lower(x)\n    working = string.substring(working, 1, string.length(working))\n";
        let module = parser::parse_module(analysis_src).expect("parse COW analysis fixture");
        let summaries = witchy_lower::analysis::Summaries::of_module(&module);
        let damage = module
            .items
            .iter()
            .find_map(|item| match item {
                ast::Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("damage function");
        let candidates = witchy_lower::escape::confined_reassigned_vars(damage, &summaries);
        assert!(
            !candidates.contains("working"),
            "SEC-078: COW casing result admitted as fresh storage: {candidates:?}"
        );
    }

    /// (SEC-078) Freshness must survive every assignment. A binding that later
    /// receives a borrowed pointer cannot be reclaimed on a subsequent update.
    #[test]
    fn rc_free_at_overwrite_disqualifies_a_later_alias_assignment_sec_078() {
        use crate::opt::{self, Opt, OptSet};
        let src = "fn suffix() -> String:\n    \".txt\"\n\nfn main(console: Console):\n    let trusted_path = \"safe\" + suffix()\n    var working = \"fresh.txt\"\n    working = trusted_path\n    working = string.substring(working, 1, string.length(working))\n    let attacker_allocation = \"evil\" + suffix()\n    console.print(trusted_path)\n    console.print(working)\n    console.print(attacker_allocation)\n";
        let oracle = link_run(src);
        assert_eq!(oracle, vec!["safe.txt", "afe.txt", "evil.txt"]);

        opt::set_for_tests(Some(OptSet::all().without(Opt::Fold)));
        let optimized = wasm_run(src);
        opt::set_for_tests(None);
        assert_eq!(optimized, oracle, "SEC-078: later alias assignment freed publisher storage");

        let analysis_src = "fn damage(trusted: String) -> ():\n    var working = \"fresh\"\n    working = trusted\n    working = string.substring(working, 1, string.length(working))\n";
        let module = parser::parse_module(analysis_src).expect("parse later-alias analysis fixture");
        let summaries = witchy_lower::analysis::Summaries::of_module(&module);
        let damage = module
            .items
            .iter()
            .find_map(|item| match item {
                ast::Item::Function(function) => Some(function),
                _ => None,
            })
            .expect("damage function");
        let candidates = witchy_lower::escape::confined_reassigned_vars(damage, &summaries);
        assert!(
            !candidates.contains("working"),
            "SEC-078: later alias assignment preserved stale freshness: {candidates:?}"
        );
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
        let src = "type Stack:\n    Empty\n    Push(a, Stack(a))\n\ntype Reading:\n    sensor: String\n    values: List(Int)\n\nfn main(console: Console):\n    let st = region -> Stack(Int):\n        Push(1, Push(2, Empty))\n    console.print(\"${st == Push(1, Push(2, Empty))}\")\n    let r = region -> Reading:\n        var vs = []\n        for i in 0..50:\n            list.push(vs, i * i)\n        Reading(sensor: \"t\" + \"0\", values: vs)\n    console.print(r.sensor)\n    console.print(\"${list.at(r.values, 49)}\")\n    let d = region -> Dict(String, Int):\n        var m = dict.new()\n        for i in 0..100:\n            let _ = dict.insert(m, \"k\" + \"${i}\", i)\n        m\n    console.print(\"${dict.get_or(d, \"k42\", 0 - 1)}\")\n    let shared = \"parent-side\"\n    let s = region -> String:\n        shared\n    console.print(s)\n    let nested = region -> Int:\n        let inner: String = region -> String:\n            \"abc\" + \"def\"\n        inner.length()\n    console.print(\"${nested}\")\n";
        let want: Vec<String> = ["true", "t0", "2401", "42", "parent-side", "6"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
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

    /// RFC-0167 row L is deliberately three-routed.  The interpreter is the
    /// source-value oracle; the legacy binary copy counters above cover its
    /// historical forced-copy path; this test alone covers the admitted
    /// canonical artifact.  A canonical region may promote scalars and a
    /// supported normal-mode owned result, but opt must not turn a region-born
    /// pointer into a hidden copy. Fixed packed values carry an exact
    /// source-derived descriptor, and a must result may leave only as the
    /// one fresh constructor obligation the checker retained.
    #[test]
    fn rfc0167_region_canonical_matrix_admits_supported_copy_out_and_refuses_hidden_copy() {
        use crate::runtime::{Capabilities, Runtime};

        for (label, source, expected) in [
            (
                "ordinary scalar region",
                "fn main(console: Console):\n    let value = region -> Int:\n        42\n    console.print(\"${value}\")\n",
                vec!["42".to_string()],
            ),
            (
                "normal owned pointer copy-out",
                "fn main(console: Console):\n    let value: String = region -> String:\n        \"copy\" + \"-out\"\n    console.print(value)\n",
                vec!["copy-out".to_string()],
            ),
            (
                "normal aliased input keeps a snapshot across promotion",
                "fn main(console: Console):\n    let parent = [1]\n    let value: List(Int) = region -> List(Int):\n        var local = parent\n        list.push(local, 2)\n        local\n    console.print(\"${list.length(parent)}\")\n    console.print(\"${list.length(value)}\")\n",
                vec!["1".to_string(), "2".to_string()],
            ),
            (
                "nested scalar promotion",
                "fn main(console: Console):\n    let value = region -> Int:\n        region -> Int:\n            42\n    console.print(\"${value}\")\n",
                vec!["42".to_string()],
            ),
            (
                "opt parent-owned pointer pass-through",
                "mode opt\n\nfn main(console: Console):\n    let parent = \"parent-owned\"\n    let value: String = region -> String:\n        parent\n    console.print(value)\n",
                vec!["parent-owned".to_string()],
            ),
            (
                "opt packed region result uses its admitted descriptor",
                "mode opt\n\ntype Point packed:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    let point: Point = region -> Point:\n        Point(40, 2)\n    console.print(\"${point.x + point.y}\")\n",
                vec!["42".to_string()],
            ),
            (
                "normal must region transfers one fresh constructor result",
                "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    region -> Ticket:\n        Ticket(42)\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\nfn main():\n    finish(make())\n",
                Vec::new(),
            ),
        ] {
            let checked = witchy::resolve_std_only_checked(source)
                .unwrap_or_else(|error| panic!("{label}: source must check: {error}"));
            let source_output = link_run(source);
            assert_eq!(source_output, expected, "{label}: interpreter");
            let artifact = codegen::compile_checked_module_artifact(&checked)
                .expect_lowered(label);
            let mut runtime = Runtime::batch().expect("runtime");
            let mut vm = runtime
                .spawn_admitted(
                    &artifact,
                    Capabilities { print: true, quiet: true, ..Default::default() },
                    256,
                )
                .unwrap_or_else(|error| panic!("{label}: admitted spawn: {error}"));
            vm.run().unwrap_or_else(|error| panic!("{label}: admitted run: {error}"));
            assert!(
                vm.canonical_heap_leaks().expect("canonical heap audit") == Some(0),
                "{label}: admitted canonical result must not leak",
            );
            assert_eq!(vm.output(), source_output, "{label}: admitted canonical output");
        }

        for (label, source, required_terms) in [
            (
                "opt region-born pointer needs a copy-free proof",
                "mode opt\n\nfn main(console: Console):\n    let value: String = region -> String:\n        \"must-not-hide\" + \"-copy\"\n    console.print(value)\n",
                ["region", "copy-free"],
            ),
            (
                "must-bearing region local lacks an exact must-transfer identity",
                "must type Ticket:\n    Ticket(Int)\n\nfn make() -> Ticket:\n    region -> Ticket:\n        let ticket = Ticket(1)\n        ticket\n\nfn finish(own ticket: Ticket):\n    let _ = 0\n\nfn main():\n    finish(make())\n",
                ["region", "must-bearing"],
            ),
        ] {
            let checked = witchy::resolve_std_only_checked(source)
                .unwrap_or_else(|error| panic!("{label}: source must check before canonical refusal: {error}"));
            let refusal = codegen::compile_checked_module_artifact(&checked).expect_unsupported(label);
            assert!(
                required_terms.iter().all(|term| refusal.message.contains(*term)),
                "{label}: refusal must state its exact unsupported proof boundary, got: {}",
                refusal.message,
            );
        }
    }

    /// The type contract distinguishes the source cases before lowering: a
    /// region may return an owner borrowed from its parent and may transfer a
    /// fresh unique/frozen value, but it may neither export a reference rooted
    /// in region storage nor suspend that storage past the rewind.  Capability
    /// aggregates are likewise not a copy-out fallback.
    #[test]
    fn rfc0167_region_type_matrix_keeps_parent_owners_and_rejects_escapes() {
        for (label, source) in [
            (
                "parent-owned shared reference",
                "mode opt\n\nfn relay(parent: &Int) -> &Int:\n    region -> &Int:\n        parent\n",
            ),
            (
                "region-local handle rooted in parent reference",
                "mode opt\n\nfn relay(parent: &Int) -> &Int:\n    region -> &Int:\n        let handle = parent\n        handle\n",
            ),
            (
                "fresh unique owner",
                "mode opt\n\nfn make() -> unique List(Int):\n    region -> unique List(Int):\n        [1, 2, 3]\n",
            ),
            (
                "frozen transfer",
                "mode opt\n\nfn keep(own parent: frozen String) -> frozen String:\n    region -> frozen String:\n        parent\n",
            ),
        ] {
            typeck::check_str(source)
                .unwrap_or_else(|error| panic!("{label}: valid region relation rejected: {error}"));
        }

        let local_reference = typeck::check_str(
            "mode opt\n\nfn bad() -> &String:\n    region:\n        let local = \"region-owned\"\n        let view = &local\n        view\n",
        )
        .expect_err("a region-born owner cannot satisfy the returned rooted view");
        assert!(
            local_reference.to_string().contains("region-born view"),
            "the rooted-view escape must be rejected at the region boundary, got: {local_reference}",
        );

        let retired_lifetime = typeck::check_str(
            "fn bad(value: &'a Int) -> &'a Int:\n    value\n",
        )
        .expect_err("explicit reference lifetimes are outside the rooted-view contract");
        assert!(
            retired_lifetime.to_string().contains("explicit reference lifetimes")
                && retired_lifetime.to_string().contains("&T"),
                "legacy lifetime syntax must be rejected explicitly, got: {retired_lifetime}",
        );

        let suspension = typeck::check_str(
            "async fn ready() -> Int:\n    7\n\nasync fn main(console: Console):\n    region:\n        let resumed = ready().await\n        console.print(\"${resumed}\")\n",
        )
        .expect_err("a suspension cannot carry region storage beyond the lexical rewind");
        assert!(
            suspension.to_string().contains("region:")
                && suspension.to_string().contains("async"),
            "the suspension rejection must name the region boundary, got: {suspension}",
        );

        let capability_source =
            "fn main(console: Console):\n    let escaped = region:\n        [console]\n    let _ = escaped\n";
        let checked = witchy::resolve_std_only_checked(capability_source)
            .expect("capability aggregate remains source-valid before canonical admission");
        let refusal = codegen::compile_checked_module_artifact(&checked)
            .expect_unsupported("capability authority cannot cross canonical region copy-out");
        assert!(
            refusal.message.contains("capability") || refusal.message.contains("region"),
            "the canonical refusal must name the capability/region boundary, got: {}",
            refusal.message,
        );

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

