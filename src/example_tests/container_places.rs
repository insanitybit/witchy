use super::*;

    /// (BUG-317/BUG-336) Dict subscripts are real read places, not a list-only
    /// parser desugar. That makes direct reads, compound assignment, and nested
    /// place assignment through a Dict agree on both backends.
    #[test]
    fn dict_subscript_read_and_nested_place_assignment_work_on_both_backends() {
        let src = "import dict\n\nfn main(console: Console):\n    var counts: Dict(String, Int) = dict.new()\n    counts[\"a\"] = 1\n    counts[\"a\"] += 2\n    console.print(\"${counts[\"a\"]}\")\n\n    var nested: Dict(String, Dict(String, Int)) = dict.new()\n    var inner: Dict(String, Int) = dict.new()\n    let _ = dict.insert(inner, \"inner\", 1)\n    nested[\"outer\"] = inner\n    nested[\"outer\"][\"inner\"] = 7\n    console.print(\"${nested[\"outer\"][\"inner\"]}\")\n\n    var rows: Dict(String, List(Int)) = dict.new()\n    rows[\"r\"] = [1, 2, 3]\n    rows[\"r\"][1] = 9\n    console.print(\"${rows[\"r\"]}\")\n";
        let expected = ["3", "7", "[1, 9, 3]"];
        assert_eq!(link_run(src), expected, "interp: dict subscript read");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: dict subscript read",
        );
    }

    /// (RFC-0050/RFC-0074) The list operations that act like value-mutators have
    /// real inherent methods, so statement form writes back consistently instead
    /// of depending on the older free-function UFCS path.
    #[test]
    fn list_mutator_methods_write_back_on_both_backends() {
        let src = "import list\n\nfn main(console: Console):\n    var xs = [3, 1, 2]\n    xs.sort()\n    console.print(\"${xs}\")\n    xs.reverse()\n    console.print(\"${xs}\")\n    xs.set_at(1, 9)\n    console.print(\"${xs}\")\n    xs.update_at(2, fn(n: Int): n + 10)\n    console.print(\"${xs}\")\n    let _ = xs.remove(9)\n    console.print(\"${xs}\")\n    var ys = [3, 1, 2]\n    ys.sort_by(fn(x: Int, y: Int): x > y)\n    console.print(\"${ys}\")\n";
        let expected = ["[1, 2, 3]", "[3, 2, 1]", "[3, 9, 1]", "[3, 9, 11]", "[3, 11]", "[3, 2, 1]"];
        assert_eq!(link_run(src), expected, "interp: list mutator methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: list mutator methods",
        );
    }

    #[test]
    fn place_assignment_sugar_backends_agree() {
        // RFC-0022: `xs[i] = v`, `d[k] = v`, and `u.field = v` (plus their compound
        // forms) desugar to value-reassignment — list/dict `set_at` and a record
        // update — and run identically on both backends.
        let src = r#"
type P:
    x: Int
    y: Int

fn main(console: Console):
    var xs = [10, 20, 30]
    xs[0] = 9
    xs[2] += 5
    console.print("${xs}")
    var d = dict.new()
    d["a"] = 1
    d["b"] = 2
    console.print("${dict.get_or(d, "a", 0)} ${dict.get_or(d, "b", 0)}")
    var p = P(1, 2)
    p.x = 10
    p.y += 5
    console.print("${p.x} ${p.y}")
"#;
        assert_eq!(
            link_run(src),
            run_linked_on_wasm(&[("main", src)], "main"),
            "place-assignment must agree across backends"
        );
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            vec!["[9, 20, 35]", "1 2", "10 7"]
        );
    }
