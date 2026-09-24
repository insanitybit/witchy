use super::*;
use crate::{interpreter, typeck};

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
