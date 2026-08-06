use super::*;

    /// (RFC-0035) Assert an adversarial-aliasing program computes `expected` IDENTICALLY on
    /// the interpreter oracle, the compiled default build, AND the compiled build with
    /// `rc-floor` on. This is the **use-after-free corpus gate** the per-object refcount
    /// (the remaining floor: `$drop` at a `set_at` overwrite) must keep green: an element
    /// still aliased — read into a live binding, duplicated, or stored elsewhere — when its
    /// container slot is overwritten must NOT be reclaimed. The programs pass today (nothing
    /// frees the displaced element); when the refcount lands, its `$drop` must decrement to a
    /// still-positive count (a live alias holds it) and free NOTHING here. A regression flips
    /// these red — the corpus is authored FIRST, as the gate, per the goal + RFC-0035 step 3.
    fn assert_rc_corpus_stable(src: &str, expected: &[&str]) {
        use crate::opt::{self, Opt, OptSet};
        let want: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(link_run(src), want, "interpreter oracle diverged");
        assert_eq!(wasm_run(src), want, "compiled default diverged from oracle");
        opt::set_for_tests(Some(OptSet::default_set().with(Opt::RcFloor)));
        let rc = wasm_run(src);
        opt::set_for_tests(None);
        assert_eq!(rc, want, "compiled under rc-floor diverged — a premature free");
    }

    /// Corpus 1: an element read into a binding that stays live PAST the `set_at` that
    /// overwrites its slot. `held` must still observe the original element.
    #[test]
    fn rc_corpus_element_read_lives_past_set_at() {
        let src = "import list\ntype Box:\n    Box(String)\nfn unwrap(b: Box) -> String:\n    match b:\n        Box(s) -> s\nfn main(console: Console):\n    var xs = [Box(\"a\"), Box(\"b\"), Box(\"c\")]\n    let held = list.at(xs, 1)\n    list.set_at(xs, 1, Box(\"z\"))\n    console.print(unwrap(held))\n    console.print(unwrap(list.at(xs, 1)))\n";
        assert_rc_corpus_stable(src, &["b", "z"]);
    }

    /// Corpus 2: the SAME element aliased into two live bindings, then the container slot
    /// overwritten. Both aliases must survive (count ≥ 2 at the overwrite).
    #[test]
    fn rc_corpus_aliased_element_survives_container_mutation() {
        let src = "import list\ntype Box:\n    Box(String)\nfn unwrap(b: Box) -> String:\n    match b:\n        Box(s) -> s\nfn main(console: Console):\n    var xs = [Box(\"a\"), Box(\"b\")]\n    let a1 = list.at(xs, 0)\n    let a2 = list.at(xs, 0)\n    list.set_at(xs, 0, Box(\"z\"))\n    console.print(unwrap(a1))\n    console.print(unwrap(a2))\n    console.print(unwrap(list.at(xs, 0)))\n";
        assert_rc_corpus_stable(src, &["a", "a", "z"]);
    }

    /// Corpus 3: an element STORED into another container (the same shape as returning it or
    /// sending it down a channel — it escapes to a place that outlives the overwrite), then
    /// the original container slot overwritten. The stored copy must survive.
    #[test]
    fn rc_corpus_element_stored_elsewhere_survives_container_mutation() {
        let src = "import list\ntype Box:\n    Box(String)\nfn unwrap(b: Box) -> String:\n    match b:\n        Box(s) -> s\nfn main(console: Console):\n    var xs = [Box(\"a\"), Box(\"b\")]\n    var ys = []\n    list.push(ys, list.at(xs, 0))\n    list.set_at(xs, 0, Box(\"z\"))\n    console.print(unwrap(list.at(ys, 0)))\n    console.print(unwrap(list.at(xs, 0)))\n";
        assert_rc_corpus_stable(src, &["a", "z"]);
    }

    /// Matrix: a HEAP String element (built via `${…}` interpolation, so it is a real
    /// $rc_alloc'd cell, not a static literal) read into a binding that outlives the set_at.
    #[test]
    fn rc_corpus_heap_string_element_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [\"v${i}\", \"v${i + 1}\", \"v${i + 2}\"]\n    let held = list.at(xs, 1)\n    list.set_at(xs, 1, \"v${i + 9}\")\n    console.print(held)\n    console.print(list.at(xs, 1))\n";
        assert_rc_corpus_stable(src, &["v2", "v10"]);
    }

    /// Matrix: a LIST element (`List(List(Int))`, runtime-built so each inner list is a heap
    /// cell) read into a binding that outlives the set_at.
    #[test]
    fn rc_corpus_list_element_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [[i, i + 1], [i + 2, i + 3], [i + 4, i + 5]]\n    let held = list.at(xs, 1)\n    list.set_at(xs, 1, [9, 9])\n    console.print(\"${held}\")\n    console.print(\"${list.at(xs, 1)}\")\n";
        assert_rc_corpus_stable(src, &["[3, 4]", "[9, 9]"]);
    }

    /// Matrix: a TUPLE element (`List((Int, Int))`) read into a binding that outlives the set_at.
    #[test]
    fn rc_corpus_tuple_element_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [(i, i + 1), (i + 2, i + 3), (i + 4, i + 5)]\n    let held = list.at(xs, 1)\n    list.set_at(xs, 1, (9, 9))\n    console.print(\"${held}\")\n    console.print(\"${list.at(xs, 1)}\")\n";
        assert_rc_corpus_stable(src, &["(3, 4)", "(9, 9)"]);
    }

    /// Matrix: a DICT element (`List(Dict)`) — the specific case the revert trapped on, because
    /// a dict pointer is `rc_res + 4` (the hidden index word), so its rc header sits at a
    /// DIFFERENT negative offset than a plain record. Read into a binding that outlives the set_at.
    #[test]
    fn rc_corpus_dict_element_survives_set_at() {
        let src = "import list\nimport dict\nfn mkd(v: Int) -> Dict(String, Int):\n    var d = dict.new()\n    dict.insert(d, \"k\", v)\n    d\nfn main(console: Console):\n    var xs = [mkd(1), mkd(2), mkd(3)]\n    let held = list.at(xs, 1)\n    list.set_at(xs, 1, mkd(9))\n    console.print(\"${held.get(\"k\")}\")\n    let replaced: Dict(String, Int) = list.at(xs, 1)\n    console.print(\"${replaced.get(\"k\")}\")\n";
        assert_rc_corpus_stable(src, &["Some(2)", "Some(9)"]);
    }

    /// Matrix: the SAME heap-String element aliased into two live bindings, then the slot
    /// overwritten. Both aliases must survive (refcount ≥ 2 at the displaced drop).
    #[test]
    fn rc_corpus_aliased_heap_string_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [\"v${i}\", \"v${i + 1}\"]\n    let a1 = list.at(xs, 0)\n    let a2 = list.at(xs, 0)\n    list.set_at(xs, 0, \"v${i + 9}\")\n    console.print(a1)\n    console.print(a2)\n    console.print(list.at(xs, 0))\n";
        assert_rc_corpus_stable(src, &["v1", "v1", "v10"]);
    }

    /// Matrix: MATCH-ON-READ of an ADT with a heap payload — the executor's actual shape
    /// (`match list.at(slots, i): Active(task) -> …`). The scrutinee is a dup'd read temp
    /// (not a let-binding); its heap payload is extracted into `r`, which must survive the set_at.
    #[test]
    fn rc_corpus_match_on_read_adt_payload_survives_set_at() {
        let src = "import list\ntype W:\n    W(String)\nfn unwrap(w: W) -> String:\n    match w:\n        W(s) -> s\nfn main(console: Console):\n    var i = 1\n    var ws = [W(\"v${i}\"), W(\"v${i + 1}\"), W(\"v${i + 2}\")]\n    let r = match list.at(ws, 1):\n        W(s) -> s\n    list.set_at(ws, 1, W(\"v${i + 9}\"))\n    console.print(r)\n    console.print(unwrap(list.at(ws, 1)))\n";
        assert_rc_corpus_stable(src, &["v2", "v10"]);
    }

    /// Matrix: a heap-String element STORED into another container (escapes past the set_at,
    /// same shape as returning it or sending it down a channel). The stored copy must survive.
    #[test]
    fn rc_corpus_heap_string_element_stored_elsewhere_survives() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var xs = [\"v${i}\", \"v${i + 1}\"]\n    var ys = []\n    list.push(ys, list.at(xs, 0))\n    list.set_at(xs, 0, \"v${i + 9}\")\n    console.print(list.at(ys, 0))\n    console.print(list.at(xs, 0))\n";
        assert_rc_corpus_stable(src, &["v1", "v10"]);
    }

    /// Matrix (executor): the async channel path — a spawned producer sends N ints over a bounded
    /// channel and the consumer drains them (chan_throughput's shape, N=100 for a fast test). This
    /// is THE residual the RC floor must bound: the cooperative executor does not reset its arena
    /// per scheduling step, so the per-message garbage (the displaced Slot / continuation closure)
    /// leaks. With emission off this proves the executor path stays byte-identical under rc-floor;
    /// when the dup/drop lands, a wrong dec here traps or diverges — it did, at ~8k, in 5e9e167,
    /// which the record-only corpus + fuzzer MISSED. This is why the executor is in the gate.
    #[test]
    fn rc_corpus_channel_executor_is_stable() {
        let src = "import chan\nfrom chan import Sender\nasync fn producer(tx: Sender(Int), n: Int) -> Nil:\n    for i in 0..n:\n        chan.send(tx, i).await\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(8).await\n    chan.spawn(producer(tx, 100)).await\n    for await v in rx:\n        chan.done(v)\n    console.print(\"100\")";
        assert_rc_corpus_stable(src, &["100"]);
    }

    /// Matrix: NESTED match-on-read — `match list.at(xs,0): W(s1) -> match list.at(ys,0): Box(s2)
    /// -> …`. Both scrutinees are dup'd reads that must drop after their arms; the shared MATCH_TMP
    /// is clobbered by the inner match, so this exercises the per-depth `__witchy_scrut_save` pool.
    /// Both displaced elements must survive through their bindings.
    #[test]
    fn rc_corpus_nested_match_on_read_uses_the_scrut_pool() {
        let src = "import list\ntype W:\n    W(String)\ntype Box:\n    Box(String)\nfn main(console: Console):\n    var i = 1\n    var xs = [W(\"a${i}\"), W(\"b${i}\")]\n    var ys = [Box(\"c${i}\"), Box(\"d${i}\")]\n    let r = match list.at(xs, 0):\n        W(s1) ->\n            match list.at(ys, 0):\n                Box(s2) -> s1 + s2\n    list.set_at(xs, 0, W(\"z${i}\"))\n    list.set_at(ys, 0, Box(\"q${i}\"))\n    console.print(r)\n";
        assert_rc_corpus_stable(src, &["a1c1"]);
    }

    /// Matrix: a NESTED `List(List(String))` element read into a binding, then the outer slot
    /// overwritten. The displaced inner list (and its heap strings) must survive via the binding —
    /// dup/drop on a List element whose own children are heap.
    #[test]
    fn rc_corpus_nested_list_element_survives_set_at() {
        let src = "import list\nfn main(console: Console):\n    var i = 1\n    var ls = [[\"p${i}\", \"q${i}\"], [\"r${i}\"]]\n    let inner = list.at(ls, 0)\n    list.set_at(ls, 0, [\"z${i}\"])\n    console.print(list.at(inner, 1))\n    console.print(list.at(list.at(ls, 0), 0))\n";
        assert_rc_corpus_stable(src, &["q1", "z1"]);
    }

    /// Regression: `toml.get_array` on a real manifest is sound under `WITCHY_OPT=rc-floor`.
    /// This once returned [] — a free-at-overwrite use-after-free in `std/string.last_index_of`
    /// (`var rest = s; rest = string.substring(rest, …)`): the first reassignment freed `rest`'s
    /// initial buffer, which ALIASED the borrowed param `s`, so the caller's string dangled and a
    /// later allocation (routed through the Phase-A free-list) overwrote it. Fixed by excluding
    /// alias-initialized vars from `escape::confined_reassigned_vars` — a var whose first buffer it
    /// does not own is never free-at-overwrite-reclaimed. (Not the dup/drop emission: bisected to the
    /// free-at-overwrite pass with every step-1..4 dup/drop disabled.)
    #[test]
    fn rc_floor_toml_get_array_is_sound() {
        let src = "import toml\nimport list\nfn main(console: Console):\n    let m = \"[capabilities]\\nruntime = [\\\"Console\\\", \\\"Dir[Read]\\\", \\\"Net[Connect]\\\"]\\n\"\n    let declared = toml.get_array(m, \"capabilities.runtime\")\n    console.print(list.join(declared, \",\"))\n";
        assert_rc_corpus_stable(src, &["Console,Dir[Read],Net[Connect]"]);
    }
