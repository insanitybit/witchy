use super::*;
use crate::{interpreter, typeck};

    /// (BUG-319, parity) Implicit structural `==` on a GENERIC record instantiation
    /// (`Box(Int)`, std `Set(Int)`) works on BOTH backends. The compiled record-eq
    /// arm dropped the type arguments (unlike the ADT arm), so a fully-annotated
    /// `Box(Int) == Box(Int)` passed `check` and ran on the interpreter but was
    /// rejected at codegen ("unresolved generic payload"). Now generic records carry
    /// their argument shapes (`RecInst`) and resolve fields under the substitution.
    #[test]
    fn generic_record_eq_agrees_on_both_backends() {
        // A user generic record whose fields are declared OUT of type-parameter
        // order — pins the DECLARED-parameter mapping (a field-order subst renders
        // `Rev(8, )` instead of `Rev(x, 1)`).
        let src = "type Rev(a, b):\n    second: b\n    first: a\n\nfn main(console: Console):\n    let r1: Rev(Int, String) = Rev(\"x\", 1)\n    let r2: Rev(Int, String) = Rev(\"x\", 1)\n    let r3: Rev(Int, String) = Rev(\"y\", 2)\n    console.print(\"${r1 == r2}\")\n    console.print(\"${r1 == r3}\")\n    console.print(\"${r1}\")\n";
        let expected = ["true", "false", "Rev(x, 1)"];
        assert_eq!(link_run(src), expected, "interp generic-record eq/render");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled generic-record eq/render must agree",
        );
        // std `Set(a)` is itself a generic record, so `Set == Set` must run compiled.
        let set_src = "import set\n\nfn main(console: Console):\n    let s1: Set(Int) = set.from_list([1, 2, 3])\n    let s2: Set(Int) = set.from_list([1, 2, 3])\n    let s3: Set(Int) = set.from_list([1, 2])\n    console.print(\"${s1 == s2}\")\n    console.print(\"${s1 == s3}\")\n";
        let set_expected = ["true", "false"];
        assert_eq!(link_run(set_src), set_expected, "interp Set == Set");
        assert_eq!(
            run_linked_on_wasm(&[("main", set_src)], "main"),
            set_expected,
            "compiled Set == Set must agree",
        );
    }

    /// (BUG-300/318, parity) Check-accepted core expressions must not be
    /// interpreter-only. Field projection through a list/call result and
    /// anonymous-record equality/rendering used to check and run under the
    /// interpreter, then fail during compiled lowering.
    #[test]
    fn compiled_backend_runs_call_chain_fields_and_anonymous_record_protocols() {
        let src = "type Top derive(PartialEq):\n    label: String\n\nfn rows() -> List(Top):\n    [Top(\"call\"), Top(\"tail\")]\n\nfn main(console: Console):\n    console.print(list.at(rows(), 0).label)\n    console.print(list.at([Top(\"literal\"), Top(\"tail\")], 0).label)\n    let a = .{x: 1, y: \"hi\"}\n    let b = .{x: 1, y: \"hi\"}\n    console.print(\"${a == b}\")\n    console.print(\"${a}\")\n";
        let interp = link_run(src);
        assert_eq!(interp[0], "call");
        assert_eq!(interp[1], "literal");
        assert_eq!(interp[2], "true");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            interp,
            "compiled backend must run these check-accepted core expressions",
        );
    }

    /// (BUG-562) Anonymous-record synthetic type names are keyed by field shape,
    /// not by each module's local first-seen ordinal. Different shapes from
    /// different modules must not collapse into the same bare compiler-private
    /// type after linking, while equal shapes may still share one definition.
    #[test]
    fn anonymous_record_shapes_do_not_collide_across_modules() {
        let left = "pub fn make():\n    .{a: 1}\n";
        let right = "pub fn make():\n    .{a: 3}\n";
        let main = "import left\nimport right\n\nfn main(console: Console):\n    let local = .{b: 2}\n    let l = left.make()\n    let r = right.make()\n    console.print(\"${l.a}\")\n    console.print(\"${r.a}\")\n    console.print(\"${local.b}\")\n";
        let sources = [("left", left), ("right", right), ("main", main)];
        let expected = ["1", "3", "2"];
        assert_eq!(interpreter::run_program(&sources, "main").expect("interp"), expected);
        assert_eq!(
            run_linked_on_wasm(&sources, "main"),
            expected,
            "compiled anonymous-record shape names must agree",
        );
    }

    /// (RFC-0078) Anonymous records are a structural type, not only expression
    /// sugar: the same shape can appear in aliases, params, returns, fields, and
    /// generic arguments. Field order in the type spelling is canonicalized by
    /// shape, and both backends see the same synthetic record instantiation.
    #[test]
    fn anonymous_record_type_positions_work_on_both_backends() {
        let src = "type Point = .{x: Int, y: Int}\ntype Wrapper:\n    point: .{y: Int, x: Int}\n\nfn make(x: Int, y: Int) -> .{y: Int, x: Int}:\n    .{x: x, y: y}\n\nfn label(p: .{y: Int, x: Int}) -> String:\n    \"${p.x},${p.y}\"\n\nfn lift(p: Point) -> Wrapper:\n    Wrapper(p)\n\nfn main(console: Console):\n    let p: Point = make(3, 4)\n    let w = lift(p)\n    let rows: List(.{y: Int, x: Int}) = [w.point, .{x: 5, y: 6}]\n    console.print(label(list.at(rows, 0)))\n    console.print(label(list.at(rows, 1)))\n";
        let expected = ["3,4", "5,6"];
        assert_eq!(link_run(src), expected, "interp structural record type positions");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled structural record type positions must agree",
        );
    }

    /// (RFC-0098 slice 1) Type-position record spread normalizes through aliases
    /// and generic arguments to the same exact structural identity as a directly
    /// spelled shape before either backend sees it.
    #[test]
    fn structural_record_type_composition_runs_on_both_backends() {
        let src = r#"type Value(a) = .{value: a}
type Located(a) = .{..Value(a), line: Int, label: String}

fn describe(row: .{label: String, value: String, line: Int}) -> String:
    "${row.line}:${row.label}:${row.value}"

fn main(console: Console):
    let row: Located(String) = .{label: "ready", value: "payload", line: 7}
    console.print(describe(row))
    console.print("${row}")
"#;
        let expected = ["7:ready:payload", ".{label: ready, line: 7, value: payload}"];
        assert_eq!(link_run(src), expected, "interp record type composition");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled record type composition must agree",
        );
    }

    #[test]
    fn structural_record_type_composition_rejects_invalid_shapes_before_backends() {
        let conflict = "type Base = .{a: Int}\ntype Bad = .{..Base, a: String}\nfn main():\n    ()\n";
        let conflict_error = try_link_std(conflict).expect_err("conflicting field types fail");
        assert!(
            conflict_error.contains("field `a` has conflicting types"),
            "{conflict_error}"
        );

        let nominal = "type User:\n    a: Int\ntype Bad = .{..User, b: String}\nfn main():\n    ()\n";
        let nominal_error = try_link_std(nominal).expect_err("nominal base fails");
        assert!(
            nominal_error.contains("type spread requires an anonymous record shape"),
            "{nominal_error}"
        );
    }

    /// (RFC-0098 slice 2) Directed expected-type sites authenticate one shared
    /// exact projection before the interpreter/Wasm split. Rendering proves the
    /// target loses extra fields while a borrowed call leaves the richer source
    /// unchanged.
    #[test]
    fn structural_record_width_projection_runs_on_both_backends() {
        let src = r#"type Summary = .{id: Int, label: String}
type Detailed = .{id: Int, label: String, note: String}

fn summarize(row: Summary) -> String:
    "${row}"

fn inspect(let row: Summary) -> String:
    "${row.label}"

fn consume(own row: Summary) -> String:
    "${row}"

fn mark_int(console: Console, marker: String, value: Int) -> Int:
    console.print(marker)
    value

fn mark_string(console: Console, marker: String, value: String) -> String:
    console.print(marker)
    value

fn make(console: Console) -> Detailed:
    .{
        id: mark_int(console, "source-id", 8),
        label: mark_string(console, "source-label", "made"),
        note: mark_string(console, "source-note", "discarded")
    }

fn main(console: Console):
    let detailed: Detailed = .{id: 7, label: "ready", note: "kept"}
    console.print(summarize(detailed))
    console.print(inspect(detailed))
    console.print("${detailed}")
    let assigned: Summary = detailed
    console.print("${assigned}")
    let cast = detailed as Summary
    console.print("${cast}")
    let owned: Detailed = .{id: 9, label: "owned", note: "gone"}
    console.print(consume(move owned))
    console.print(summarize(make(console)))
"#;
        let expected = [
            ".{id: 7, label: ready}",
            "ready",
            ".{id: 7, label: ready, note: kept}",
            ".{id: 7, label: ready}",
            ".{id: 7, label: ready}",
            ".{id: 9, label: owned}",
            "source-id",
            "source-label",
            "source-note",
            ".{id: 8, label: made}",
        ];
        assert_eq!(link_run(src), expected, "interpreter record width projection");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled record width projection must agree",
        );
    }

    #[test]
    fn structural_record_width_rejections_are_shared_before_backends() {
        let missing = r#"type Need = .{a: Int, b: String}
fn take(row: Need):
    ()
fn main():
    take(.{a: 1})
"#;
        let linked = resolve_std_src(missing);
        let error = typeck::check(&linked)
            .expect_err("missing field must fail")
            .to_string();
        assert!(error.contains("missing required field `b`"), "{error}");

        let mismatch = r#"type Need = .{a: Int, b: String}
fn take(row: Need):
    ()
fn bad(row: .{a: Int, b: Int, c: Int}):
    take(row)
fn main():
    bad(.{a: 1, b: 2, c: 3})
"#;
        let linked = resolve_std_src(mismatch);
        let error = typeck::check(&linked)
            .expect_err("mismatched field must fail")
            .to_string();
        assert!(error.contains("field `b` has incompatible type"), "{error}");
    }

    #[test]
    fn structural_record_width_expected_site_matrix_agrees_on_both_backends() {
        let src = r#"type Small = .{a: Int}
type Large = .{a: Int, b: String}

type Slot:
    row: Small

fn returned(row: Large) -> Small:
    return row

fn tailed(row: Large) -> Small:
    row

fn defaulted(row: Small = .{a: 6, b: "default"}) -> Small:
    row

fn main(console: Console):
    let large: Large = .{a: 1, b: "kept"}
    var assigned: Small = .{a: 0}
    assigned = large
    let rows: List(Small) = [large]
    let pair: (Small, Int) = (large, 2)
    let slot = Slot(large)
    let conditional: Small = if true:
        large
    else:
        .{a: 9}
    console.print("${assigned}")
    console.print("${list.at(rows, 0)}")
    console.print("${pair.0}")
    console.print("${slot.row}")
    console.print("${conditional}")
    console.print("${returned(large)}")
    console.print("${tailed(large)}")
    console.print("${defaulted()}")
"#;
        let expected = [
            ".{a: 1}",
            ".{a: 1}",
            ".{a: 1}",
            ".{a: 1}",
            ".{a: 1}",
            ".{a: 1}",
            ".{a: 1}",
            ".{a: 6}",
        ];
        assert_eq!(link_run(src), expected, "interpreter expected-site matrix");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled expected-site matrix must agree",
        );
    }

    /// RFC-0098 AC7/12: once projected, every reflective and exact-shape
    /// consumer sees only the authenticated target record. A String field also
    /// exercises the compiled reference-bearing slot path; dictionary lookup
    /// proves exact target equality/key comparison rather than source-layout
    /// relabeling.
    #[test]
    fn structural_record_projection_observability_agrees_on_both_backends() {
        let src = r#"import dict
import json
import reflect

type Summary = .{id: Int, label: String}
type Detailed = .{..Summary, revision: Int}

fn main(console: Console):
    let detailed: Detailed = .{id: 7, label: "ready", revision: 3}
    let summary: Summary = detailed
    let expected: Summary = .{id: 7, label: "ready"}
    console.print("${summary}")
    console.print(json.stringify(summary))
    console.print(reflect.debug(summary))
    console.print("${summary == expected}")
    var keyed = dict.new()
    keyed.insert(summary, "hit")
    console.print("${keyed.contains_key(expected)}")
    console.print("${detailed}")
"#;
        let expected = [
            ".{id: 7, label: ready}",
            "{\"id\":7,\"label\":\"ready\"}",
            "true",
            "true",
            ".{id: 7, label: ready, revision: 3}",
        ];
        let interpreter = link_run(src);
        let compiled = run_linked_on_wasm(&[("main", src)], "main");
        assert_eq!(compiled, interpreter, "compiled projected observability must agree");
        assert_eq!(interpreter[0], expected[0]);
        assert_eq!(interpreter[1], expected[1]);
        assert!(interpreter[2].contains("id: 7"), "{}", interpreter[2]);
        assert!(interpreter[2].contains("label: \"ready\""), "{}", interpreter[2]);
        assert!(!interpreter[2].contains("revision"), "{}", interpreter[2]);
        assert_eq!(&interpreter[3..], &expected[2..]);
    }

    /// (RFC-0078) Anonymous records support the same spread/update spelling as
    /// named records. The spread preserves the base shape exactly; it does not
    /// introduce width subtyping or new fields.
    #[test]
    fn anonymous_record_spread_updates_on_both_backends() {
        let src = "fn make(x: Int, y: Int) -> .{x: Int, y: Int}:\n    .{x: x, y: y}\n\nfn bump(p: .{y: Int, x: Int}) -> .{x: Int, y: Int}:\n    .{y: p.y + 5, ..p}\n\nfn main(console: Console):\n    let p = make(3, 4)\n    let q = .{y: 9, ..p}\n    let r = .{x: q.x + 1, ..q}\n    let s = bump(r)\n    console.print(\"${q}\")\n    console.print(\"${r.x},${r.y}\")\n    console.print(\"${s.x},${s.y}\")\n";
        let expected = [".{x: 3, y: 9}", "4,9", "4,14"];
        assert_eq!(link_run(src), expected, "interp anonymous record spread");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled anonymous record spread must agree",
        );
    }

    /// (RFC-0078) Anonymous union injections are typed by their expected union
    /// shape and render with source-level `.Tag` names, including inside Result.
    #[test]
    fn anonymous_union_injections_work_on_both_backends() {
        let src = "type LoadErr = .[BadPort(Int) | NotFound]\n\nfn bad(port: Int) -> LoadErr:\n    .BadPort(port)\n\nfn missing() -> LoadErr:\n    .NotFound\n\nfn parse(ok: Bool) -> Result(Int, LoadErr):\n    if ok:\n        Ok(1)\n    else:\n        Err(.NotFound)\n\nfn main(console: Console):\n    console.print(\"${bad(70000)}\")\n    console.print(\"${missing()}\")\n    console.print(\"${parse(false)}\")\n";
        let expected = [".BadPort(70000)", ".NotFound", "Err(.NotFound)"];
        assert_eq!(link_run(src), expected, "interp anonymous union injection");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled anonymous union injection must agree",
        );
    }

    /// (RFC-0078) Anonymous union patterns are checked against the scrutinee's
    /// closed tag set and lower to the same tag-word dispatch as declared enums.
    #[test]
    fn anonymous_union_patterns_work_on_both_backends() {
        let src = "type LoadErr = .[BadPort(Int) | Missing(String) | NotFound]\n\nfn describe(e: LoadErr) -> String:\n    match e:\n        .BadPort(p) -> \"bad:\" + \"${p}\"\n        .Missing(k) -> \"missing:\" + k\n        .NotFound -> \"not-found\"\n\nfn parse(kind: Int) -> Result(Int, LoadErr):\n    if kind == 0:\n        Ok(7)\n    else:\n        if kind == 1:\n            Err(.Missing(\"host\"))\n        else:\n            Err(.BadPort(70000))\n\nfn describe_result(r: Result(Int, LoadErr)) -> String:\n    match r:\n        Ok(n) -> \"ok:\" + \"${n}\"\n        Err(.BadPort(p)) -> \"bad-result:\" + \"${p}\"\n        Err(.Missing(k)) -> \"missing-result:\" + k\n        Err(.NotFound) -> \"not-found-result\"\n\nfn main(console: Console):\n    console.print(describe(.BadPort(9)))\n    console.print(describe(.Missing(\"port\")))\n    console.print(describe(.NotFound))\n    console.print(describe_result(parse(0)))\n    console.print(describe_result(parse(1)))\n    console.print(describe_result(parse(2)))\n";
        let expected = [
            "bad:9",
            "missing:port",
            "not-found",
            "ok:7",
            "missing-result:host",
            "bad-result:70000",
        ];
        assert_eq!(link_run(src), expected, "interp anonymous union patterns");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled anonymous union patterns must agree",
        );
    }

    /// (RFC-0078) Anonymous union widening is a no-op at runtime because tag
    /// identity is global, not the variant's position inside one closed set.
    #[test]
    fn anonymous_union_widening_uses_global_tags_on_both_backends() {
        let src = "type Small = .[B(Int) | C]\ntype Big = .[A | B(Int) | C]\n\nfn small_b(n: Int) -> Small:\n    .B(n)\n\nfn small_c() -> Small:\n    .C\n\nfn describe(e: Big) -> String:\n    match e:\n        .A -> \"A\"\n        .B(n) -> \"B:\" + \"${n}\"\n        .C -> \"C\"\n\nfn tail_widen(n: Int) -> Big:\n    small_b(n)\n\nfn return_widen(n: Int) -> Big:\n    return small_b(n)\n\nfn small_result() -> Result(Int, Small):\n    Err(small_c())\n\nfn try_widen() -> Result(Int, Big):\n    Ok(small_result()?)\n\nfn main(console: Console):\n    console.print(describe(small_b(4)))\n    console.print(describe(tail_widen(5)))\n    console.print(describe(return_widen(6)))\n    match try_widen():\n        Ok(n) -> console.print(\"ok:\" + \"${n}\")\n        Err(.A) -> console.print(\"err:A\")\n        Err(.B(n)) -> console.print(\"err:B:\" + \"${n}\")\n        Err(.C) -> console.print(\"err:C\")\n";
        let expected = ["B:4", "B:5", "B:6", "err:C"];
        assert_eq!(link_run(src), expected, "interp anonymous union widening");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled anonymous union widening must agree",
        );
    }

    /// (RFC-0078) Anonymous unions participate in the structural protocol tier:
    /// `Show`, `Reflect`, and `PartialEq` are synthesized from their closed tag
    /// set and payload protocols. The rendered/debug spelling keeps the leading
    /// dot to mark the anonymous tier.
    #[test]
    fn anonymous_union_protocols_work_on_both_backends() {
        let src = "import show\nimport reflect\nimport cmp\n\nfn same(x: a, y: a) -> Bool where a: PartialEq:\n    x == y\n\nfn main(console: Console):\n    let a: .[Bad(Int) | Missing(String)] = .Bad(7)\n    let b: .[Bad(Int) | Missing(String)] = .Missing(\"port\")\n    let c: .[Bad(Int) | Missing(String)] = .Bad(8)\n    console.print(show.render(a))\n    console.print(show.render(b))\n    console.print(reflect.debug(a))\n    console.print(reflect.debug(b))\n    console.print(\"${same(a, a)}\")\n    console.print(\"${same(a, b)}\")\n    console.print(\"${same(a, c)}\")\n";
        let expected = [".Bad(7)", ".Missing(port)", ".Bad(7)", ".Missing(\"port\")", "true", "false", "false"];
        assert_eq!(link_run(src), expected, "interp anonymous union protocols");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled anonymous union protocols must agree",
        );
    }

    /// (RFC-0065, parity) Set/DateTime/Rng/Url are `sealed type`s: external code must
    /// build them through the module's smart constructors (dedup / validated /
    /// parsed), but may still READ and MATCH them. The public API + inspection run
    /// identically on both backends, and a raw out-of-module data-constructor call is
    /// a link-time rejection (the same check on both backends — parity by
    /// construction). Guards BUG-238/252/256/460.
    #[test]
    fn rfc0065_sealed_std_types_seal_construction_keep_inspection_on_both_backends() {
        // Public API (dedup set, validated time, parsed url) + external field-read all
        // run, and agree on both backends.
        let ok = "import set\nimport time\nimport url\n\nfn main(console: Console):\n    let s = set.from_list([1, 1, 2, 3])\n    console.print(\"${set.length(s)}\")\n    let d = time.from_millis(0)\n    console.print(\"${time.year(d)}\")\n    match url.parse(\"http://h/p\"):\n        Ok(u) -> console.print(url.host(u))\n        Err(e) -> console.print(url.url_error_message(e))\n";
        let expected = ["3", "1970", "h"];
        assert_eq!(link_run(ok), expected, "interp: sealed-type public API + inspection");
        assert_eq!(
            run_linked_on_wasm(&[("main", ok)], "main"),
            expected,
            "compiled: sealed-type public API + inspection must agree",
        );

        // Raw out-of-module construction of the data constructor is a link error,
        // naming the sealed type — so the impossible `DateTime(2026, 13, 40, …)` the
        // validating `time.civil`/`time.from_millis` would never produce is
        // unrepresentable outside `time` (BUG-252).
        let forge = "import time\n\nfn main(console: Console):\n    let d = time.DateTime(2026, 13, 40, 0, 0, 0)\n    console.print(\"${time.year(d)}\")\n";
        let err = try_link_std(forge).expect_err("raw sealed construction must be a link error");
        assert!(
            err.contains("sealed type") && err.contains("DateTime") && err.contains("construct"),
            "diagnostic must name the sealed type and construction: {err}"
        );

        // `semver.Version` is also sealed (BUG-191) — and, unlike the four above, it
        // is a DERIVED record used inside `List`/`Option`/`Result` across a module
        // that itself imports (cmp/string/iter), so this pins that the seal holds for
        // a derived, container-carried, transitively-imported type too.
        let sv_forge = "import semver\n\nfn main(console: Console):\n    let v = semver.Version(-1, 2, 3)\n    console.print(\"${semver.format(v)}\")\n";
        let err = try_link_std(sv_forge).expect_err("raw Version construction must be a link error");
        assert!(
            err.contains("sealed type") && err.contains("Version") && err.contains("construct"),
            "diagnostic must name the sealed Version and construction: {err}"
        );
    }

    /// (BUG-064) Channel endpoints are sealed brands around executor channel ids:
    /// user code may name/pass `Sender(m)` and `Receiver(m)`, but it cannot rebuild
    /// one endpoint at a different message type and make `__unerase` lie.
    #[test]
    fn chan_endpoints_seal_raw_channel_id_construction() {
        let sender_forge = "import chan\nfrom chan import Sender\n\nasync fn main(console: Console):\n    let (tx, _rx) = chan.channel(0).await\n    match tx:\n        Sender(id) ->\n            let forged: Sender(String) = Sender(id)\n            let _ = forged\n            console.print(\"forged\")\n";
        let err = try_link_std(sender_forge).expect_err("Sender raw-id construction must be sealed");
        assert!(
            err.contains("sealed type") && err.contains("Sender") && err.contains("construct"),
            "diagnostic must name sealed Sender construction: {err}"
        );

        let receiver_forge = "import chan\nfrom chan import Receiver\n\nasync fn main(console: Console):\n    let (_tx, rx) = chan.channel(0).await\n    match rx:\n        Receiver(id) ->\n            let forged: Receiver(String) = Receiver(id)\n            let _ = forged\n            console.print(\"forged\")\n";
        let err = try_link_std(receiver_forge).expect_err("Receiver raw-id construction must be sealed");
        assert!(
            err.contains("sealed type") && err.contains("Receiver") && err.contains("construct"),
            "diagnostic must name sealed Receiver construction: {err}"
        );
    }
