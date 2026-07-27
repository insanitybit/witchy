use super::*;
use crate::{codegen, interpreter, parser, typeck};

fn assert_mutation_backends(src: &str, expected: &[&str], label: &str) {
    let expected: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(interp(src), expected, "{label}: interpreter");
    assert_eq!(run_on_wasm(src), expected, "{label}: compiled WASM");
}

    /// The parameter conventions (`var`/`let`/`own` + `move`) behave identically
    /// on both the interpreter and WASM backends — value semantics are
    /// preserved regardless of which knob the author reaches for. `var` writes
    /// back, `let` borrows (read-only), `own` consumes, a bare param is owned, and
    /// `move x` transfers ownership.
    #[test]
    fn conventions_backends_agree() {
        let src = "fn bump(var n: Int):\n    n = n + 1\n\nfn total(let xs: List(Int)) -> Int:\n    var s = 0\n    for x in xs:\n        s = s + x\n    s\n\nfn drain(own xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn doubled(xs: List(Int)) -> Int:\n    list.at(xs, 0) * 2\n\nfn main(console: Console):\n    var c = 0\n    bump(c)\n    bump(c)\n    console.print(\"${c}\")\n    let nums = [10, 20, 30]\n    console.print(\"${total(nums)}\")\n    console.print(\"${doubled(nums)}\")\n    console.print(\"${list.length(nums)}\")\n    let g = [1, 2, 3, 4]\n    console.print(\"${drain(move g)}\")\n";
        let expected = ["2", "60", "20", "3", "4"];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// `let` borrows extend past `List` to the other heap types: a `String`
    /// parameter (the recursive-parser shape that motivated this — char ops on a
    /// borrowed string, no clone) and a `Dict`. Native emits `&String` / `&WMap`
    /// and the output matches every backend.
    #[test]
    fn convention_string_and_dict_borrow() {
        let strs = "fn first_char(let s: String) -> String:\n    if s.char_count() > 0:\n        s.substring(0, 1)\n    else:\n        \"\"\nfn main(c: Console):\n    let txt = \"héllo\"\n    c.print(first_char(txt))\n    c.print(\"${txt.char_count()}\")\n";
        assert_eq!(interpreter::run(strs).expect("interp str"), ["h", "5"]);
        assert_eq!(run_linked_on_wasm(&[("main", strs)], "main"), ["h", "5"], "wasm str");

        let dict = "fn lookup(let d: Dict(String, Int)) -> Int:\n    dict.get_or(d, \"a\", -1)\nfn main(c: Console):\n    var m = dict.new()\n    dict.insert(m, \"a\", 42)\n    c.print(\"${lookup(m)}\")\n    c.print(\"${dict.length(m)}\")\n";
        assert_eq!(link_run(dict), ["42", "1"]);
        assert_eq!(run_linked_on_wasm(&[("main", dict)], "main"), ["42", "1"], "wasm dict");
    }

    /// `move` works in every value position (let value, list element, call
    /// argument), forcing a move; the moved binding can't be reused (rejected by
    /// the type checker, uniformly).
    #[test]
    fn convention_move_value_positions() {
        let prog = "fn main(console: Console):\n    let a = [1, 2, 3]\n    let b = move a\n    console.print(\"${list.length(b)}\")\n";
        assert_eq!(interpreter::run(prog).expect("interp"), ["3"]);
        assert_eq!(run_linked_on_wasm(&[("main", prog)], "main"), ["3"], "wasm");
        // Reuse after move is rejected everywhere.
        let reuse = "fn main(console: Console):\n    let a = [1, 2, 3]\n    let b = move a\n    console.print(\"${list.length(b) + list.length(a)}\")\n";
        assert!(typeck::check_str(reuse).is_err(), "reuse after move must fail");
    }

    /// A borrow can't escape: returning a `let` parameter transpiles, but Rust's
    /// borrow checker rejects it at compile time (the opt-in contract — drop `let`
    /// or use `own`). A non-escaping borrow compiles fine.
    #[test]
    fn convention_borrow_cannot_escape() {
        // Returning a `let` parameter escapes the borrow — a TYPE error on
        // every backend (the rule moved from the removed native backend's
        // borrow checker into typeck).
        let escapes = "fn id(let xs: List(Int)) -> List(Int):\n    xs\nfn main(c: Console):\n    c.print(\"${list.length(id([1, 2, 3]))}\")\n";
        let err = typeck::check_str(escapes).expect_err("escaping borrow must be rejected");
        assert!(err.to_string().contains("cannot be returned"), "{err}");
        // Reading it (no escape) is fine.
        let reads = "fn count(let xs: List(Int)) -> Int:\n    list.length(xs)\nfn main(c: Console):\n    c.print(\"${count([1, 2, 3])}\")\n";
        assert!(typeck::check_str(reads).is_ok(), "a read-only borrow should check");
    }

    /// Conventions apply to a method's receiver too: `let self` borrows it
    /// (read-only), and `own self` consumes it (the value can't be used after the
    /// call). Both run identically on interpreter and native.
    #[test]
    fn convention_method_receivers() {
        // `let self` — borrow the receiver, return a fresh value (functional style).
        let borrow_self = "type Counter:\n    Counter(Int)\nimpl Counter:\n    fn incremented(let self) -> Counter:\n        match self:\n            Counter(n) -> Counter(n + 1)\nfn main(c: Console):\n    let a = Counter(5)\n    match a.incremented():\n        Counter(n) -> c.print(\"${n}\")\n";
        // `own self` — consume the receiver.
        let own_self = "import list\ntype Buffer:\n    Buffer(List(Int))\nimpl Buffer:\n    fn drain(own self) -> Int:\n        match self:\n            Buffer(xs) -> list.sum(xs)\nfn main(c: Console):\n    let buf = Buffer([1, 2, 3])\n    c.print(\"${buf.drain()}\")\n";
        for (tag, src) in [("let_self", borrow_self), ("own_self", own_self)] {
            assert_eq!(link_run(src), vec!["6"], "{tag} interp");
            assert_eq!(wasm_run(src), vec!["6"], "{tag} wasm");
        }
    }

    /// A borrow can be forwarded BOTH ways: to another borrow parameter it passes
    /// straight through (`&T` -> `&T`, no copy), and to an owned parameter it is
    /// deref-cloned (you can't move out of a borrow). Same result on every backend.
    #[test]
    fn convention_borrow_forwarding() {
        let src = "fn owned_first(xs: List(Int)) -> Int:\n    list.at(xs, 0) * 2\n\nfn borrowed_len(let ys: List(Int)) -> Int:\n    list.length(ys)\n\nfn report(let xs: List(Int)) -> Int:\n    borrowed_len(xs) + owned_first(xs)\n\nfn main(c: Console):\n    let data = [5, 6, 7]\n    c.print(\"${report(data)}\")\n    c.print(\"${list.length(data)}\")\n";
        assert_eq!(interpreter::run(src).expect("interp"), ["13", "3"]);
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), ["13", "3"], "wasm");
    }

    /// THE UNIQUENESS ANALYSIS, observable: an alias taken BEFORE the loop
    /// zeroes the ownership token once — the first push re-owns (one copy)
    /// and everything after runs in place. The old syntactic whitelist
    /// disqualified the variable outright (O(n²), memory-cap trap at this
    /// size). The alias still sees its snapshot.
    #[test]
    fn analysis_alias_before_loop_stays_linear() {
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    let snapshot = xs\n    var i = 0\n    while i < 50000:\n        list.push(xs, i)\n        i = i + 1\n    console.print(\"${snapshot}\")\n    console.print(\"${list.length(xs)}\")\n";
        let want = vec!["[1, 2, 3]".to_string(), "50003".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "expected O(1) re-owns, got {reowns}");
    }

    /// FUNCTION SUMMARIES: a read-only helper called in the hot loop no
    /// longer kills the token (the bottom-up pass proves its parameter never
    /// aliases out). Under the whitelist this was an instant disqualification.
    #[test]
    fn analysis_readonly_call_keeps_loop_linear() {
        let src = "fn peek(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    var ws = []\n    var m = 0\n    var probe = 0\n    while m < 3000:\n        list.push(ws, m)\n        probe = peek(ws)\n        m = m + 1\n    console.print(\"${probe}\")\n";
        let want = vec!["3000".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, reowns) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
        assert!(reowns <= 2, "the summary must keep the loop in place, got {reowns}");
    }

    /// A function that RETURNS its parameter (may_alias_out) still kills:
    /// the bound result whole-aliases the buffer, so the next push copies —
    /// and the alias keeps its snapshot.
    #[test]
    fn analysis_alias_returning_call_still_kills() {
        let src = "fn same(xs: List(Int)) -> List(Int):\n    xs\n\nfn main(console: Console):\n    var xs = [1]\n    var i = 0\n    while i < 100:\n        list.push(xs, i)\n        i = i + 1\n    let held = same(xs)\n    list.push(xs, 999)\n    console.print(\"${list.length(held)}\")\n    console.print(\"${list.length(xs)}\")\n";
        let want = vec!["101".to_string(), "102".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        let (out, _) = wasm_run_reowns(src);
        assert_eq!(out, want, "wasm");
    }

    /// DIRTY SITES: a self-assign whose RHS embeds the variable (`s = s + s`,
    /// a pushed snapshot stored into a dict) runs through the copying path
    /// and stays value-semantic on both backends.
    #[test]
    fn analysis_dirty_shapes_stay_value_semantic() {
        let src = "fn main(console: Console):\n    var s = \"ab\"\n    var k = 0\n    while k < 5:\n        s = s + s\n        k = k + 1\n    console.print(\"${s.length()}\")\n    var d = dict.new()\n    var zs = [1]\n    dict.insert(d, \"snap\", zs)\n    list.push(zs, 2)\n    console.print(\"${list.length(dict.get_or(d, \"snap\", []))}\")\n    console.print(\"${list.length(zs)}\")\n";
        let want: Vec<String> = ["64", "1", "2"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// A lambda body is its own analysis unit: an accumulator inside one gets
    /// its own ownership token (this used to emit an undeclared `__cap`
    /// local — a loud compile failure).
    #[test]
    fn analysis_lambda_accumulator_compiles() {
        let src = "fn main(console: Console):\n    let build = fn(n: Int):\n        var acc = [0]\n        var t = 0\n        while t < n:\n            list.push(acc, t)\n            t = t + 1\n        list.length(acc)\n    console.print(\"${build(1000)}\")\n";
        let want = vec!["1001".to_string()];
        assert_eq!(link_run(src), want, "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// An own-ABI callee that returns its parameter only on SOME paths: the
    /// other paths return a zero token (the caller re-owns later) — always
    /// correct, never corrupting.
    #[test]
    fn analysis_own_abi_partial_return_paths_are_sound() {
        let src = "fn cap_at(own xs: List(Int), n: Int) -> List(Int):\n    if list.length(xs) >= n:\n        []\n    else:\n        list.push(xs, n)\n        xs\n\nfn main(console: Console):\n    var xs = [0]\n    var i = 0\n    while i < 50:\n        xs = cap_at(move xs, i)\n        i = i + 1\n    console.print(\"${xs}\")\n";
        let interp = link_run(src);
        assert_eq!(wasm_run(src), interp, "wasm must agree on the mixed paths");
    }

    /// (BUG-558 sharpened) The same loop-watermark escape must be rejected when
    /// the `var` callee writes back a record field directly. The list case used
    /// to corrupt the length header; the dict case read garbage memory.
    #[test]
    fn loop_watermark_rejects_outer_var_record_field_writeback() {
        let list_src = "import list\n\ntype Buf:\n    items: List(Int)\n\nfn add(var b: Buf, x: Int):\n    list.push(b.items, x)\n\nfn main(console: Console):\n    var b = Buf([])\n    var i = 0\n    while i < 16:\n        add(b, i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n";
        let list_expected = vec!["15".to_string(), "16".to_string()];
        assert_eq!(link_run(list_src), list_expected, "interpreter: list field");
        assert_eq!(wasm_run(list_src), list_expected, "wasm: list field");

        let dict_src = "import dict\n\ntype Tally:\n    counts: Dict(Int, Int)\n\nfn bump(var t: Tally, k: Int):\n    dict.insert(t.counts, k, k * 2)\n\nfn main(console: Console):\n    var t = Tally(dict.new())\n    var i = 0\n    while i < 50:\n        bump(t, i)\n        i = i + 1\n    console.print(\"${dict.get_or(t.counts, 49, 0)}\")\n    console.print(\"${dict.length(t.counts)}\")\n";
        let dict_expected = vec!["98".to_string(), "50".to_string()];
        assert_eq!(link_run(dict_src), dict_expected, "interpreter: dict field");
        assert_eq!(wasm_run(dict_src), dict_expected, "wasm: dict field");

        let sibling_src = "import dict\nimport set\n\ntype Bag:\n    counts: Dict(String, Int)\n    seen: Set(String)\n\nfn inc(n: Int) -> Int:\n    n + 1\n\nfn main(console: Console):\n    var bag = Bag(dict.new(), set.new())\n    var i = 0\n    while i < 16:\n        bag.counts.update(\"hit\", 0, inc)\n        bag.seen.insert(\"k${i}\")\n        i = i + 1\n    console.print(\"${dict.get_or(bag.counts, \"hit\", 0)}\")\n    console.print(\"${set.length(bag.seen)}\")\n";
        let sibling_expected = vec!["16".to_string(), "16".to_string()];
        assert_eq!(link_run(sibling_src), sibling_expected, "interpreter: sibling fields");
        assert_eq!(wasm_run(sibling_src), sibling_expected, "wasm: sibling fields");
    }

    /// ARENA WATERMARK RESETS: a loop whose body lets nothing escape an
    /// iteration (only scalar outer assignments) reclaims each iteration's
    /// allocations — 200k iterations that would otherwise demand ~6 GB run in
    /// constant memory. WASM-only: the interpreter's clone-per-push is
    /// quadratic and would take far too long at this scale.
    #[test]
    fn arena_resets_keep_escape_free_loops_constant_memory() {
        let src = "fn main(console: Console):\n    var total = 0\n    for i in 0..200000:\n        var row = []\n        var j = 0\n        for j in 0..1000:\n            list.push(row, j)\n        total = total + list.at(row, 999)\n    console.print(\"${total}\")\n";
        assert_eq!(wasm_run(src), vec!["199800000"]);
    }

    /// Closure capture is pruned to the names the body mentions (the
    /// interpreter used to clone the entire environment per closure — itself a
    /// quadratic cost in accumulation loops). Calling through a captured
    /// closure variable still works, and capture remains a snapshot: a later
    /// reassignment of the source variable is invisible to the closure.
    #[test]
    fn closure_capture_pruned_and_snapshot() {
        let src = "fn main(console: Console):\n    let add = fn(x: Int): x + 1\n    let twice = fn(y: Int): add(add(y))\n    console.print(\"${twice(3)}\")\n    var n = 10\n    let snap = fn(): n\n    n = 99\n    console.print(\"${snap()}\")\n";
        let want: Vec<String> = ["5", "10"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "compiled WASM must agree");
    }

    /// A function parameter stays genuinely indirect: `drive` cannot know which
    /// closure-table slot `f` carries. The dynamic edge and `step -> drive` form a
    /// recursive component that both backends must trampoline without Wasm tail calls.
    #[test]
    fn proper_indirect_closure_cycle_uses_constant_stack_on_both_backends() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn drive(bounce: Bounce, n: Int) -> Int:
    match bounce:
        Bounce(f) -> f(bounce, n)

fn step(bounce: Bounce, n: Int) -> Int:
    if n == 0:
        5000000007
    else:
        drive(bounce, n - 1)

fn main(console: Console):
    let bounce = Bounce(step)
    console.print("${drive(bounce, 250001)}")
"#;
        assert_mutation_backends(src, &["5000000007"], "callable trampoline");
    }

    #[test]
    fn proper_singleton_indirect_cycle_uses_constant_stack_on_both_backends() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn main(console: Console):
    let bounce = Bounce(fn(b: Bounce, n: Int) -> Int:
        if n == 0:
            9
        else:
            match b:
                Bounce(f) -> f(b, n - 1)
    )
    match bounce:
        Bounce(f) -> console.print("${f(bounce, 30001)}")
"#;
        assert_mutation_backends(src, &["9"], "singleton trampoline");
    }

    #[test]
    fn proper_dynamic_cycle_survives_multiple_named_hops_on_both_backends() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn first(bounce: Bounce, n: Int) -> Int:
    second(bounce, n)

fn second(bounce: Bounce, n: Int) -> Int:
    match bounce:
        Bounce(f) -> f(bounce, n)

fn step(bounce: Bounce, n: Int) -> Int:
    if n == 0:
        5000000007
    else:
        first(bounce, n - 1)

fn main(console: Console):
    let bounce = Bounce(step)
    console.print("${first(bounce, 30001)}")
"#;
        assert_mutation_backends(src, &["5000000007"], "dynamic chain");
    }

    #[test]
    fn proper_indirect_cycles_adapt_scalar_result_slots_on_both_backends() {
        let src = r#"
type StringBounce:
    StringBounce(fn(StringBounce, Int) -> String)

type BoolBounce:
    BoolBounce(fn(BoolBounce, Int) -> Bool)

type FloatBounce:
    FloatBounce(fn(FloatBounce, Int) -> Float)

fn drive_string(bounce: StringBounce, n: Int) -> String:
    match bounce:
        StringBounce(f) -> f(bounce, n)

fn drive_bool(bounce: BoolBounce, n: Int) -> Bool:
    match bounce:
        BoolBounce(f) -> f(bounce, n)

fn drive_float(bounce: FloatBounce, n: Int) -> Float:
    match bounce:
        FloatBounce(f) -> f(bounce, n)

fn main(console: Console):
    let answer = "done"
    let strings = StringBounce(fn(b: StringBounce, n: Int) -> String:
        if n == 0:
            answer
        else:
            drive_string(b, n - 1)
    )
    let bools = BoolBounce(fn(b: BoolBounce, n: Int) -> Bool:
        if n == 0:
            true
        else:
            drive_bool(b, n - 1)
    )
    let floats = FloatBounce(fn(b: FloatBounce, n: Int) -> Float:
        if n == 0:
            1.5
        else:
            drive_float(b, n - 1)
    )
    console.print(drive_string(strings, 30001))
    console.print("${drive_bool(bools, 30001)}")
    console.print("${drive_float(floats, 30001)}")
"#;
        assert_mutation_backends(src, &["done", "true", "1.5"], "scalar envelopes");
    }

    #[test]
    fn proper_indirect_dispatcher_preserves_outside_component_fallback() {
        let src = r#"
type Bounce:
    Bounce(fn(Bounce, Int) -> Int)

fn drive(bounce: Bounce, n: Int) -> Int:
    match bounce:
        Bounce(f) -> f(bounce, n)

fn finish(bounce: Bounce, n: Int) -> Int:
    99

fn step(bounce: Bounce, n: Int) -> Int:
    if n == 0:
        drive(Bounce(finish), 0)
    else:
        drive(bounce, n - 1)

fn main(console: Console):
    let bounce = Bounce(step)
    console.print("${drive(bounce, 30001)}")
"#;
        assert_mutation_backends(src, &["99"], "outside target fallback");
    }

    /// RFC-0087 structured returns commit every final `var` value together. A
    /// callee-side `?` is ordinary early return, so mutations completed before
    /// propagation remain visible on both the success and error paths.
    #[test]
    fn rfc0087_multi_var_try_commits_partial_progress_on_both_backends() {
        let src = "import result\n\nfn step(var left: Int, var right: Int, r: Result(Int, String)) -> Result(Int, String):\n    left = left + 100\n    right = right + 10\n    let got = r?\n    left = left + got\n    right = right + got * 2\n    Ok(left + right)\n\nfn main(console: Console):\n    var a = 1\n    var b = 10\n    let ok = step(a, b, Ok(5))\n    console.print(\"${a}\")\n    console.print(\"${b}\")\n    console.print(\"${ok.unwrap_or(0)}\")\n\n    var c = 2\n    var d = 20\n    let failed = step(c, d, Err(\"stop\"))\n    console.print(\"${c}\")\n    console.print(\"${d}\")\n    console.print(\"${failed.unwrap_or(-1)}\")\n";
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("value-returning multi-var functions are valid");
        let want = vec!["106", "30", "136", "102", "30", "-1"];
        assert_eq!(
            interpreter::run_module(linked.clone(), ".", Vec::new()).expect("interpreter"),
            want,
            "interpreter commits every var on success and callee-side ?",
        );
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers multi-var structured returns");
        assert_eq!(
            crate::run_wasm_bytes(&bytes).expect("wasm run"),
            want,
            "compiled backend matches the interpreter's ? write-back",
        );
    }

    #[test]
    fn rfc0087_structured_return_spellings_and_caller_propagation_agree() {
        let src = r#"import option

fn option_receiver_try(var state: Option(Int), var count: Int) -> Option(Int):
    count = count + 1
    let value = state?
    state = Some(value + 1)
    Some(value)

fn update_or_none(var n: Int, succeeds: Bool) -> Option(Int):
    n = n + 10
    if succeeds:
        Some(n)
    else:
        None

fn caller_try(var n: Int) -> Option(Int):
    let value = update_or_none(n, false)?
    Some(value)

fn main(console: Console):
    var state: Option(Int) = None
    var count = 0
    let option_result = option_receiver_try(state, count)
    console.print("${state} ${count} ${option_result}")

    var propagated = 1
    let propagated_result = caller_try(propagated)
    console.print("${propagated} ${propagated_result}")

    var fallback_state = 2
    let fallback = update_or_none(fallback_state, false) ?? fallback_state + 100
    console.print("${fallback_state} ${fallback}")
"#;
        let want = ["None 1 None", "11 None", "12 112"];
        assert_eq!(link_run(src), want, "interpreter structured returns");
        assert_eq!(wasm_run(src), want, "compiled structured returns");
    }

    /// (RFC-0051 I2) The single-allocator invariant on LOWERED PROGRAMS: assemble
    /// full WIR modules (helpers + user code) for representative programs that
    /// exercise the heap-touching lowering shapes — accumulation (`list.push`/
    /// dict insert self-assigns → the `*_cap` in-place paths), string building,
    /// a scalar `region:` reclaim, a pointer `region:` copy-out, and a loop-arena
    /// reset — and walk every function body: any `SetGlobal { global: "heap" }`
    /// outside `$bump_alloc` and the named watermark REWINDS
    /// (`heap = __witchy_wm_*` / `heap = wm + copied_len`, which move `$heap`
    /// down to or below an already-ensured frontier) fails with the offending
    /// function's name. Because all WIR construction funnels through
    /// `assemble_wir_module`, the walk sees everything — including future
    /// helpers — so the `ensure()` convention cannot be silently forgotten
    /// (the `int_to_string` OOB class). Registry-wide helper coverage lives in
    /// witchy-wir's `single_allocator_invariant_holds_across_helper_registry`.
    #[test]
    fn single_allocator_invariant_holds_on_lowered_programs() {
        let progs = [
            // accumulators: list push / dict insert / string concat self-assigns
            "fn main(console: Console):\n    var xs = []\n    var d = dict.new()\n    var s = \"\"\n    for i in 0..50:\n        list.push(xs, i)\n        dict.insert(d, \"k${i}\", i)\n        s = s + \"x\"\n    list.set_at(xs, 0, 9)\n    console.print(\"${list.length(xs)} ${dict.length(d)} ${s.length()}\")\n",
            // scalar region reclaim (the watermark rewind exemption)
            "\nfn main(console: Console):\n    let n = region -> Int:\n        var parts = []\n        for i in 0..20:\n            list.push(parts, \"p${i}\")\n        list.length(parts)\n    console.print(\"${n}\")\n",
            // pointer region copy-out (the `heap = wm + copied_len` advance-rewind)
            "\nfn main(console: Console):\n    let summary: String = region:\n        var parts = []\n        for i in 0..20:\n            list.push(parts, \"p${i}\")\n        list.join(parts, \",\")\n    console.print(\"${summary.length()}\")\n",
        ];
        for src in progs {
            let linked = resolve_std_src(src);
            typeck::check(&linked).expect("typecheck");
            let m = codegen::assemble_wir_module(&linked)
                .expect_lowered(&format!("expected the WIR binary path to handle:\n{src}"));
            let violations = witchy_wir::wir::heap_write_violations(&m);
            assert!(
                violations.is_empty(),
                "RFC-0051 I2 violated — these functions write `$heap` outside \
                 `$bump_alloc`/the watermark rewinds: {violations:?}\nprogram:\n{src}"
            );
        }
    }

    /// The own-ABI (`own` buffer param threaded through `move`) on the binary
    /// path: `grow` takes `own xs`, appends in place, and returns it. The callee
    /// gains a trailing `$xs__cap` i32 param and a second i32 result (the
    /// ownership token); the self-call `xs = grow(move xs, i)` lowers to a
    /// CallStoreMulti capturing (value → xs, cap → xs__cap). (2-way: `list.push`
    /// isn't resolvable by the WAT-leg `check_str`, so compare binary vs oracle.)
    #[test]
    fn wir_own_abi_move_pipeline_binary_path() {
        let src = "fn grow(own xs: List(Int), n: Int) -> List(Int):\n    list.push(xs, n)\n    xs\n\nfn main(console: Console):\n    var xs = []\n    for i in 1..6:\n        xs = grow(move xs, i)\n    console.print(\"${xs}\")\n";
        let want = vec!["[1, 2, 3, 4, 5]".to_string()];
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the WIR binary path should lower the own-ABI move pipeline");
        assert_eq!(run_bytes_print_only(&bytes), want, "binary path");
        assert_eq!(link_run(src), want, "interpreter oracle");
    }

    /// (RFC-0043 Failure 1) An unrelated `impl Bag: fn push(self, v) -> Bag`
    /// declared elsewhere in the program used to poison the whole-program name
    /// census: `push` entered the `shadowed` set, so a List `xs.push(1)`
    /// statement silently stopped writing back (`parity` agreed, so nothing
    /// caught it). Now resolution is PER RECEIVER TYPE: a List receiver resolves
    /// to `list.push` (a mutator), never to `Bag.push`, so the write-back fires.
    /// Both backends must print `[1]` — the census bug is dead by construction.
    #[test]
    fn rfc0043_unrelated_impl_no_longer_shadows_list_push() {
        let src = "import list\n\
                   type Bag:\n\
                   \x20   n: Int\n\
                   \n\
                   impl Bag:\n\
                   \x20   fn push(self, v: Int) -> Bag:\n\
                   \x20       Bag(self.n + v)\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   var xs = []\n\
                   \x20   xs.push(1)\n\
                   \x20   console.print(\"${xs}\")\n";
        let want = vec!["[1]".to_string()];
        assert_eq!(link_run(src), want, "interpreter: List push must write back despite `impl Bag: push`");
        assert_eq!(wasm_run(src), want, "compiled: List push must write back despite `impl Bag: push`");
    }

    /// (RFC-0043 Failure 2) The old census keyed write-back on the *generic
    /// declared* return type equalling the receiver's: `filter : List(a) ->
    /// List(a)` qualified and silently MUTATED, while `map : List(a) -> List(b)`
    /// did not and silently no-op'd — same syntax, opposite effects, no
    /// diagnostic. Neither is a mutator (`var` receiver) now, so a `filter`/`map`
    /// statement whose result is discarded is a LOUD compile error on both
    /// backends (the check the two backends share), naming the fix.
    #[test]
    fn rfc0043_filter_and_map_statements_are_discard_errors() {
        for method in ["filter", "map"] {
            let body = if method == "filter" {
                "xs.filter(fn(n: Int) -> Bool: n > 2)"
            } else {
                "xs.map(fn(n: Int) -> Int: n * 10)"
            };
            let src = format!(
                "import list\n\
                 fn main(console: Console):\n\
                 \x20   var xs = [1, 2, 3, 4]\n\
                 \x20   {body}\n\
                 \x20   console.print(\"${{xs}}\")\n"
            );
            let linked = resolve_std_src(&src);
            let err = typeck::check(&linked)
                .expect_err(&format!("a discarded `{method}` statement must be a compile error"))
                .to_string();
            assert!(
                err.contains(&format!("result of `{method}` is discarded")),
                "the {method} discard error must name the method and the fix, got: {err}"
            );
        }
    }

    /// (RFC-0043) `let _ = expr` is the explicit-discard escape: it turns the
    /// discard error off while running the call for its effects, and leaves the
    /// receiver untouched. Both backends run and agree (`xs` unchanged).
    #[test]
    fn rfc0043_let_underscore_is_the_discard_escape() {
        let src = "import list\n\
                   fn main(console: Console):\n\
                   \x20   var xs = [1, 2, 3, 4]\n\
                   \x20   let _ = xs.filter(fn(n: Int) -> Bool: n > 2)\n\
                   \x20   console.print(\"${xs}\")\n";
        let want = vec!["[1, 2, 3, 4]".to_string()];
        assert_eq!(link_run(src), want, "interpreter: `let _ =` discards and leaves xs unchanged");
        assert_eq!(wasm_run(src), want, "compiled: `let _ =` discards and leaves xs unchanged");
    }

    /// (RFC-0043) The real mutators still write back in statement form on both
    /// backends — the declared-`var`-receiver path is the same self-assign shape
    /// (`xs = list.push(xs, …)`) the uniqueness pass already optimizes, so
    /// push/insert/set_at/remove all mutate in place and agree.
    #[test]
    fn rfc0043_real_mutators_still_write_back_both_backends() {
        // list.push, list.set_at, dict.insert, dict.remove — one program, mixed.
        let src = "import list\nimport dict\n\
                   fn main(console: Console):\n\
                   \x20   var xs = [1, 2, 3]\n\
                   \x20   xs.push(4)\n\
                   \x20   xs.set_at(0, 9)\n\
                   \x20   console.print(\"${xs}\")\n\
                   \x20   var d = dict.new()\n\
                   \x20   d.insert(\"a\", 1)\n\
                   \x20   d.insert(\"b\", 2)\n\
                   \x20   d.remove(\"a\")\n\
                   \x20   console.print(\"${dict.contains_key(d, \"a\")}\")\n\
                   \x20   console.print(\"${dict.get_or(d, \"b\", 0)}\")\n";
        let want = vec!["[9, 2, 3, 4]".to_string(), "false".to_string(), "2".to_string()];
        assert_eq!(link_run(src), want, "interpreter: real mutators must write back");
        assert_eq!(wasm_run(src), want, "compiled: real mutators must write back");
    }

    /// (BUG-575 / RFC-0043) A match arm used in statement position inherits
    /// statement-position mutator semantics. The arm expression `out.push(value)`
    /// must write back exactly like a bare statement in a block; it is not a value
    /// result to be discarded by the surrounding `match`.
    #[test]
    fn rfc0043_match_arm_mutators_write_back_both_backends() {
        let src = "import list\nimport option\n\
                   fn collect(items: List(Option(String))) -> Result(List(String), String):\n\
                   \x20   var out: List(String) = []\n\
                   \x20   for item in items:\n\
                   \x20       match item:\n\
                   \x20           None -> return Err(\"missing\")\n\
                   \x20           Some(value) -> out.push(value)\n\
                   \x20   Ok(out)\n\
                   \n\
                   fn main(console: Console):\n\
                   \x20   match collect([Some(\"x\"), Some(\"y\")]):\n\
                   \x20       Ok(xs) -> console.print(\"${xs}\")\n\
                   \x20       Err(e) -> console.print(e)\n\
                   \x20   match collect([Some(\"ok\"), None]):\n\
                   \x20       Ok(xs) -> console.print(\"${xs}\")\n\
                   \x20       Err(e) -> console.print(e)\n";
        let want = vec!["[x, y]".to_string(), "missing".to_string()];
        assert_eq!(link_run(src), want, "interpreter: match-arm mutator writes back");
        assert_eq!(wasm_run(src), want, "compiled: match-arm mutator writes back");
    }

    /// (RFC-0043) A mutator in statement form on an IMMUTABLE `let` place has no
    /// `var` to write back to — a compile error naming the fix (declare `var`,
    /// or bind the result), not a silent discard.
    #[test]
    fn rfc0043_mutator_on_immutable_place_is_an_error() {
        let src = "import list\n\
                   fn main(console: Console):\n\
                   \x20   let xs = [1, 2, 3]\n\
                   \x20   xs.push(4)\n\
                   \x20   console.print(\"${xs}\")\n";
        let linked = resolve_std_src(src);
        let err = typeck::check(&linked)
            .expect_err("a mutator on a `let` place must be an error")
            .to_string();
        assert!(err.contains("immutable") && err.contains("`let`"),
            "the immutable-place error must explain the fix, got: {err}");
    }

    /// The ordinary result and `var` write-back are independent channels. Neither
    /// parameter position nor a result matching the mutable argument classifies
    /// the call, and both backends commit the same final values.
    #[test]
    fn rfc0087_former_row3_shapes_write_back_on_both_backends() {
        let non_first = "import list\n\
                         fn foo(x: Int, var xs: List(Int)) -> List(Int):\n\
                         \x20   list.push(xs, x)\n\
                         \x20   xs\n\
                         fn main(console: Console):\n\
                         \x20   var xs = [1]\n\
                         \x20   let ys = foo(9, xs)\n\
                         \x20   console.print(\"${xs}\")\n\
                         \x20   console.print(\"${ys}\")\n";
        let unrelated = "import list\n\
                         fn foo(var xs: List(Int)) -> Int:\n\
                         \x20   xs.push(9)\n\
                         \x20   list.length(xs)\n\
                         fn main(console: Console):\n\
                         \x20   var xs = [1]\n\
                         \x20   let n = foo(xs)\n\
                         \x20   console.print(\"${xs}\")\n\
                         \x20   console.print(\"${n}\")\n";
        for (src, want) in [
            (non_first, vec!["[1, 9]".to_string(), "[1, 9]".to_string()]),
            (unrelated, vec!["[1, 9]".to_string(), "2".to_string()]),
        ] {
            assert_eq!(link_run(src), want, "interpreter writes back and returns");
            assert_eq!(wasm_run(src), want, "compiled backend agrees");
        }
    }

    /// Return inference never selects mutation semantics. An inferred result uses
    /// the same move-in/move-out ABI as an explicitly annotated result.
    #[test]
    fn rfc0087_elided_var_result_writes_back_on_both_backends() {
        let elided = "import list\n\
                      fn bump(var xs: List(Int), by: Int):\n\
                      \x20   list.push(xs, by)\n\
                      \x20   list.length(xs)\n\
                      fn main(console: Console):\n\
                      \x20   var xs = [1, 2, 3]\n\
                      \x20   let n = bump(xs, 5)\n\
                      \x20   console.print(\"${xs}\")\n\
                      \x20   console.print(\"${n}\")\n";
        let want = vec!["[1, 2, 3, 5]".to_string(), "4".to_string()];
        assert_eq!(link_run(elided), want, "interpreter inferred return");
        assert_eq!(wasm_run(elided), want, "compiled inferred return");
    }

    /// RFC-0087's discard rule depends on the resolved `var` convention, not call
    /// syntax. Free and method calls both commit write-back when their result is
    /// discarded explicitly or implicitly.
    #[test]
    fn rfc0087_discard_rule_is_effect_based() {
        let free_std = "import list\n\
                        fn main(console: Console):\n\
                        \x20   var xs = [1, 2, 3]\n\
                        \x20   list.push(xs, 2)\n\
                        \x20   console.print(\"${xs}\")\n";
        let free_user = "import list\n\
                         fn bump(var xs: List(Int), by: Int) -> Int:\n\
                         \x20   xs.push(by)\n\
                         \x20   list.length(xs)\n\
                         fn main(console: Console):\n\
                         \x20   var xs = [1, 2, 3]\n\
                         \x20   bump(xs, 5)\n\
                         \x20   console.print(\"${xs}\")\n";
        for (src, want) in [(free_std, "[1, 2, 3, 2]"), (free_user, "[1, 2, 3, 5]")] {
            assert_eq!(link_run(src), [want], "interpreter commits free var call");
            assert_eq!(wasm_run(src), [want], "compiled backend commits free var call");
        }

        let escaped = "import list\n\
                       fn main(console: Console):\n\
                       \x20   var xs = [1, 2, 3]\n\
                       \x20   let _ = list.push(xs, 2)\n\
                       \x20   console.print(\"${xs}\")\n";
        let want = vec!["[1, 2, 3, 2]".to_string()];
        assert_eq!(link_run(escaped), want, "interpreter explicit discard still writes back");
        assert_eq!(wasm_run(escaped), want, "compiled explicit discard still writes back");
    }

    /// (RFC-0044 rule 1) `string.index_of` returns `Option(Int)`, not a
    /// `-1` sentinel. The other string search contracts have dedicated tests.
    #[test]
    fn rfc0044_string_index_absence_is_option() {
        let src = "\
                   fn main(console: Console):\n\
                   \x20   console.print(\"${\"hello\".index_of(\"ll\") ?? -1}\")\n\
                   \x20   console.print(\"${\"hello\".index_of(\"z\") ?? -1}\")\n";
        let expected = ["2", "-1"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }
