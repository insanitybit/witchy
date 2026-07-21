use super::*;

    /// (BUG-321) Dict keys are any concrete `Eq` value, not just scalar slots.
    /// The compiled backend dispatches key equality through the same structural
    /// helpers as `==`, including String fields and enum payloads.
    #[test]
    fn compound_dict_keys_work_on_both_backends() {
        let src = "import cmp\nimport dict\n\ntype Pair derive(PartialEq, Eq):\n    id: Int\n    label: String\n\ntype Tag derive(PartialEq, Eq):\n    ById(Int)\n    ByName(String)\n\nfn main(console: Console):\n    var records = dict.new()\n    dict.insert(records, Pair(100000, \"a\"), 10)\n    dict.insert(records, Pair(100000, \"a\"), 20)\n    console.print(\"${dict.get_or(records, Pair(100000, \"a\"), 0)}\")\n    console.print(\"${dict.contains_key(records, Pair(100000, \"a\"))}\")\n    console.print(\"${dict.length(records)}\")\n\n    var tags = dict.new()\n    dict.insert(tags, ById(7), \"id\")\n    dict.insert(tags, ByName(\"x\"), \"name\")\n    dict.insert(tags, ById(7), \"id2\")\n    console.print(dict.get_or(tags, ById(7), \"missing\"))\n    console.print(dict.get_or(tags, ByName(\"x\"), \"missing\"))\n    console.print(\"${dict.length(tags)}\")\n";
        let expected = ["20", "true", "1", "id2", "name", "2"];
        assert_eq!(link_run(src), expected, "interp: compound dict keys");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: compound dict keys",
        );
    }

    /// (BUG-317/BUG-336) Dict subscripts are real read places, not a list-only
    /// parser desugar. That makes direct reads, compound assignment, and nested
    /// place assignment through a Dict agree on both backends.
    #[test]
    fn dict_subscript_read_and_nested_place_assignment_work_on_both_backends() {
        let src = "import dict\n\nfn main(console: Console):\n    var counts: Dict(String, Int) = dict.new()\n    counts[\"a\"] = 1\n    counts[\"a\"] += 2\n    console.print(\"${counts[\"a\"]}\")\n\n    var nested: Dict(String, Dict(String, Int)) = dict.new()\n    var inner: Dict(String, Int) = dict.new()\n    dict.insert(inner, \"inner\", 1)\n    nested[\"outer\"] = inner\n    nested[\"outer\"][\"inner\"] = 7\n    console.print(\"${nested[\"outer\"][\"inner\"]}\")\n\n    var rows: Dict(String, List(Int)) = dict.new()\n    rows[\"r\"] = [1, 2, 3]\n    rows[\"r\"][1] = 9\n    console.print(\"${rows[\"r\"]}\")\n";
        let expected = ["3", "7", "[1, 9, 3]"];
        assert_eq!(link_run(src), expected, "interp: dict subscript read");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: dict subscript read",
        );
    }

    /// Field-contained containers mutated through a `var` record parameter keep
    /// their headers intact across write-back, including the interpolation reads
    /// that previously exposed stale lengths. This pins both the free-function
    /// shape and the `var self` method shape needed by the stdlib method sweep.
    #[test]
    fn var_record_container_field_updates_keep_headers_on_both_backends() {
        let list_free = "type Buf:\n    items: List(Int)\n\nfn add(var b: Buf, x: Int):\n    list.push(b.items, x)\n\nfn main(console: Console):\n    var b = Buf([])\n    var i = 0\n    while i < 16:\n        add(b, i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n";
        let list_method = "type Buf:\n    items: List(Int)\n\nimpl Buf:\n    fn add(var self, x: Int) -> Nil:\n        list.push(self.items, x)\n        return\n\nfn main(console: Console):\n    var b = Buf([])\n    var i = 0\n    while i < 16:\n        b.add(i)\n        i = i + 1\n    console.print(\"${list.at(b.items, 15)}\")\n    console.print(\"${list.length(b.items)}\")\n";
        let dict_field = "import dict\n\ntype Tally:\n    counts: Dict(Int, Int)\n\nfn bump(var t: Tally, k: Int):\n    dict.insert(t.counts, k, k * 2)\n\nfn main(console: Console):\n    var t = Tally(dict.new())\n    var i = 0\n    while i < 50:\n        bump(t, i)\n        i = i + 1\n    console.print(\"${dict.get_or(t.counts, 49, 0)}\")\n    console.print(\"${dict.length(t.counts)}\")\n";

        for (label, src, expected) in [
            ("list free function", list_free, vec!["15", "16"]),
            ("list var self method", list_method, vec!["15", "16"]),
            ("dict free function", dict_field, vec!["98", "50"]),
        ] {
            assert_eq!(link_run(src), expected, "interp: {label}");
            assert_eq!(
                run_linked_on_wasm(&[("main", src)], "main"),
                expected,
                "compiled: {label}",
            );
        }
    }

    /// (RFC-0074) Lists have the same remove-by-value affordance as set/dict,
    /// with first-occurrence semantics; all-occurrences removal remains `filter`.
    #[test]
    fn list_remove_removes_first_occurrence_on_both_backends() {
        let src = "import list\n\nfn main(console: Console):\n    var xs = [1, 2, 3, 2]\n    xs.remove(2)\n    console.print(\"${xs}\")\n    xs.remove(9)\n    console.print(\"${xs}\")\n    var words = [\"a\", \"b\", \"a\"]\n    let _removed = list.remove(words, \"a\")\n    console.print(\"${words}\")\n";
        let expected = ["[1, 3, 2]", "[1, 3, 2]", "[b, a]"];
        assert_eq!(link_run(src), expected, "interp: list.remove");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: list.remove",
        );
    }

    /// (RFC-0050/RFC-0074) The list operations that act like value-mutators have
    /// real inherent methods, so statement form writes back consistently instead
    /// of depending on the older free-function UFCS path.
    #[test]
    fn list_mutator_methods_write_back_on_both_backends() {
        let src = "import list\n\nfn main(console: Console):\n    var xs = [3, 1, 2]\n    xs.sort()\n    console.print(\"${xs}\")\n    xs.reverse()\n    console.print(\"${xs}\")\n    xs.set_at(1, 9)\n    console.print(\"${xs}\")\n    xs.update_at(2, fn(n: Int): n + 10)\n    console.print(\"${xs}\")\n    xs.remove(9)\n    console.print(\"${xs}\")\n    var ys = [3, 1, 2]\n    ys.sort_by(fn(x: Int, y: Int): x > y)\n    console.print(\"${ys}\")\n";
        let expected = ["[1, 2, 3]", "[3, 2, 1]", "[3, 9, 1]", "[3, 9, 11]", "[3, 11]", "[3, 2, 1]"];
        assert_eq!(link_run(src), expected, "interp: list mutator methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: list mutator methods",
        );
    }

    /// Dict and Set carry the same mutator-method rule as List: module
    /// functions remain the function-value surface, while statement-form methods
    /// on `var self` write back to the receiver.
    #[test]
    fn dict_and_set_mutator_methods_write_back_on_both_backends() {
        let src = "import dict\nimport set\n\nfn main(console: Console):\n    var d: Dict(String, Int) = dict.new()\n    d.insert(\"a\", 1)\n    d.insert(\"b\", 2)\n    d.update(\"b\", 0, fn(n: Int): n + 5)\n    d.remove(\"a\")\n    console.print(\"${dict.get_or(d, \"a\", 0)}\")\n    console.print(\"${dict.get_or(d, \"b\", 0)}\")\n    console.print(\"${dict.length(d)}\")\n\n    var s: Set(Int) = set.new()\n    s.insert(1)\n    s.insert(2)\n    s.insert(2)\n    s.remove(1)\n    console.print(\"${set.contains(s, 1)}\")\n    console.print(\"${set.contains(s, 2)}\")\n    console.print(\"${set.length(s)}\")\n";
        let expected = ["0", "7", "1", "false", "true", "1"];
        assert_eq!(link_run(src), expected, "interp: dict/set mutator methods");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: dict/set mutator methods",
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

    #[test]
    fn compound_assignment_backends_agree() {
        // `x op= e` desugars to `x = x op e`; verify all five ops in both
        // backends, in a loop and a sequence.
        let src = r#"
fn main(console: Console):
    var sum = 0
    var i = 0
    while (i < 5):
        sum = (sum + i)
        i = (i + 1)
    console.print("${sum}")
    var x = 100
    x = (x - 30)
    x = (x * 2)
    x = (x / 7)
    x = (x % 5)
    console.print("${x}")
"#;
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["10", "0"]);
    }
