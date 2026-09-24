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

    /// Corpus 2: the SAME element aliased into two live bindings, then the container slot
    /// overwritten. Both aliases must survive (count ≥ 2 at the overwrite).
    #[test]
    fn rc_corpus_aliased_element_survives_container_mutation() {
        let src = "import list\ntype Box:\n    Box(String)\nfn unwrap(b: Box) -> String:\n    match b:\n        Box(s) -> s\nfn main(console: Console):\n    var xs = [Box(\"a\"), Box(\"b\")]\n    let a1 = list.at(xs, 0)\n    let a2 = list.at(xs, 0)\n    list.set_at(xs, 0, Box(\"z\"))\n    console.print(unwrap(a1))\n    console.print(unwrap(a2))\n    console.print(unwrap(list.at(xs, 0)))\n";
        assert_rc_corpus_stable(src, &["a", "a", "z"]);
    }

    /// Matrix: MATCH-ON-READ of an ADT with a heap payload — the executor's actual shape
    /// (`match list.at(slots, i): Active(task) -> …`). The scrutinee is a dup'd read temp
    /// (not a let-binding); its heap payload is extracted into `r`, which must survive the set_at.
    #[test]
    fn rc_corpus_match_on_read_adt_payload_survives_set_at() {
        let src = "import list\ntype W:\n    W(String)\nfn unwrap(w: W) -> String:\n    match w:\n        W(s) -> s\nfn main(console: Console):\n    var i = 1\n    var ws = [W(\"v${i}\"), W(\"v${i + 1}\"), W(\"v${i + 2}\")]\n    let r = match list.at(ws, 1):\n        W(s) -> s\n    list.set_at(ws, 1, W(\"v${i + 9}\"))\n    console.print(r)\n    console.print(unwrap(list.at(ws, 1)))\n";
        assert_rc_corpus_stable(src, &["v2", "v10"]);
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
        if !is_nightly() {
            return;
        }
        let src = "import chan\nfrom chan import Sender\nasync fn producer(tx: Sender(Int), n: Int) -> ():\n    for i in 0..n:\n        chan.send(tx, i).await\nasync fn main(console: Console):\n    let (tx, rx) = chan.channel(8).await\n    chan.spawn(producer(tx, 100)).await\n    for await v in rx:\n        let _ = chan.done(v)\n    console.print(\"100\")";
        assert_rc_corpus_stable(src, &["100"]);
    }

    /// (SEC-036) `toml.get_array` on a real manifest is sound under the
    /// shipping optimized configuration and the explicit `rc-floor` trigger.
    /// This once returned [] — a free-at-overwrite use-after-free in string `rfind`
    /// (`var rest = s; rest = string.substring(rest, …)`): the first reassignment freed `rest`'s
    /// initial buffer, which ALIASED the borrowed param `s`, so the caller's string dangled and a
    /// later allocation (routed through the Phase-A free-list) overwrote it. Fixed by excluding
    /// alias-initialized vars from `escape::confined_reassigned_vars` — a var whose first buffer it
    /// does not own is never free-at-overwrite-reclaimed. (Not the dup/drop emission: bisected to the
    /// free-at-overwrite pass with every step-1..4 dup/drop disabled.)
    #[test]
    fn rc_floor_toml_get_array_alias_init_is_sound_sec_036() {
        let src = include_str!("../../tests/fixtures/rfc0162-sec/sec036-alias-initialized-overwrite.witchy");
        let expected = ["Console,Dir[Read],Net[Connect]"];
        assert_rc_corpus_stable(src, &expected);
    }

    /// (SEC-036) Directly pin all three alias-initializer forms excluded by the
    /// fix: a variable, field projection, and index projection, including the
    /// distinct List reclamation path. This keeps the ownership regression live
    /// even if the historical TOML call chain changes.
    #[test]
    fn alias_initialized_var_field_and_index_stay_live_sec_036() {
        use crate::opt::{self, OptSet};
        let src = include_str!("../../tests/fixtures/rfc0162-sec/sec036-direct-alias-init-shapes.witchy");
        let expected = [
            "owned",
            "owned!",
            "owned",
            "OWNED",
            "owned",
            "OWNED",
            "owned",
            "[3, 5, 8]",
            "[3, 5, 8, 3, 5, 8]",
            "[3, 5, 8]",
        ];
        assert_eq!(link_run(src), expected, "SEC-036 direct-shape interpreter oracle changed");

        opt::set_for_tests(Some(OptSet::all()));
        let optimized = wasm_run(src);
        opt::set_for_tests(None);
        assert_eq!(optimized, expected, "SEC-036: optimized Wasm reclaimed a direct alias initializer");
    }
