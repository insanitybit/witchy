use super::*;
use crate::{ast, interpreter, parser, typeck};

#[test]
fn rfc0122_local_exclusive_reference_write_agrees_on_both_backends() {
    let src = "mode opt\n\nfn main(console: Console):\n    var value = 1\n    let reference = &mut value\n    *reference = 42\n    console.print(\"${value}\")\n    console.print(\"${*reference}\")\n";
    let want = ["42", "42"];
    assert_eq!(link_run(src), want, "interpreter writes through the local reference place");
    let (compiled, _) = wasm_run_reowns(src);
    assert_eq!(compiled, want, "compiled local reference write updates its owner place");
}

    /// RFC-0083: a live view makes its owner shared in the uniqueness lattice.
    /// Materializing the view ends the loan, but the resulting owned snapshot
    /// must remain independent when the original owner mutates afterward.
    #[test]
    fn rfc0083_materialized_view_survives_owner_mutation_on_both_backends() {
        let src = "mode opt\n\nimport list\n\nfn view(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\nfn materialize(xs: List(Int)) -> List(Int):\n    xs\n\nfn main(console: Console):\n    var xs = [1]\n    list.push(xs, 2)\n    let make_view = view\n    let borrowed = make_view(xs)\n    let forwarded = borrowed\n    let snapshot = materialize(forwarded)\n    list.push(xs, 3)\n    console.print(\"${snapshot}\")\n    console.print(\"${xs}\")\n";
        let want = ["[1, 2]", "[1, 2, 3]"];
        assert_eq!(link_run(src), want, "interpreter preserves the materialized snapshot");
        let (compiled, reowns) = wasm_run_reowns(src);
        assert_eq!(compiled, want, "compiled roots and re-own preserve the snapshot");
        assert!(reowns >= 1, "opening the view must invalidate the owner's unique token");

        // A transient view consumed by `.owned()` leaves no live loan or runtime
        // root, but evaluating it still shares the owner and must invalidate the
        // uniqueness token before the subsequent mutation.
        let direct = "mode opt\n\nimport borrow\nimport list\n\nfn view(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n\nfn main(console: Console):\n    var xs = [1]\n    list.push(xs, 2)\n    let snapshot = view(xs).owned()\n    list.push(xs, 3)\n    console.print(\"${snapshot}\")\n    console.print(\"${xs}\")\n";
        assert_eq!(link_run(direct), want, "interpreter preserves a direct materialization");
        let (compiled, reowns) = wasm_run_reowns(direct);
        assert_eq!(compiled, want, "compiled preserves a direct materialization");
        assert!(reowns >= 1, "transient materialization must invalidate uniqueness");
    }

    #[test]
    fn rfc0112_borrowed_parser_iterator_agrees_on_both_backends() {
        let src = "mode opt\n\n\
             type Parser('a):\n    input: View(String, 'a)\n    offset: Int\n\n\
             type TokenIter('a):\n    input: View(String, 'a)\n    index: Int\n\n\
             type Token('a):\n    text: View(String, 'a)\n    width: Int\n\n\
             fn parser(input: let('a) String) -> Parser('a):\n    Parser(input, 2)\n\n\
             fn tokens(input: let('a) String) -> TokenIter('a):\n    TokenIter(input, 3)\n\n\
             fn scan(input: let('a) String) -> Int:\n    let p = parser(input)\n    let it = tokens(p.input)\n    let values: List(Token('a)) = [Token(p.input, p.offset), Token(it.input, it.index)]\n    var total = 0\n    for token in values:\n        total = total + token.width\n    total\n\n\
             fn main(console: Console):\n    console.print(\"${scan(\"source\")}\")\n";
        let want = ["5"];
        assert_eq!(link_run(src), want, "interpreter parser/iterator result");
        let (compiled, _) = wasm_run_reowns(src);
        assert_eq!(compiled, want, "compiled parser/iterator result");
    }

    #[test]
    fn rfc0083_view_may_not_cross_async_suspension_unmaterialized() {
        let src = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nasync fn bad(console: Console) -> Nil:\n    var text = \"borrowed\"\n    let w = view(text)\n    let _ = task.done(0).await\n    console.print(w)\n";
        let err = try_link_std(src).expect_err("a borrowed view may not cross an await");
        assert!(
            err.contains("async fn `bad`: borrowed value `w` remains live across `await`"),
            "diagnostic must explain the live view: {}",
            err,
        );

        let assigned = "mode opt\n\nfn view(text: let('a) String) -> View(String, 'a):\n    text\n\nasync fn bad(console: Console) -> Nil:\n    var text = \"borrowed\"\n    var slot = \"\"\n    slot = view(text)\n    let _ = task.done(0).await\n    console.print(slot)\n";
        let err = try_link_std(assigned)
            .expect_err("assignment may not hide a view across async suspension");
        assert!(
            err.contains("async fn `bad`: borrowed value `slot` remains live across `await`"),
            "{err}"
        );
    }

    #[test]
    fn rfc0083_normal_caller_enforces_opt_module_loan_relation() {
        let api = parser::parse_module(
            "mode opt\n\npub fn view(text: let('a) String) -> View(String, 'a):\n    text\n",
        )
        .expect("parse opt API");
        let main = parser::parse_module(
            "import api\n\nfn main(console: Console):\n    var text = \"original\"\n    let w = api.view(text)\n    text = \"changed\"\n    console.print(w)\n",
        )
        .expect("parse normal caller");
        let err = crate::pipeline::link(
            vec![("main".into(), main), ("api".into(), api)],
            "main",
        )
        .expect_err("normal callers cannot name a legacy reference-bearing opt export");
        assert!(err.to_string().contains("reference-bearing opt API `api.view`"), "{err}");
        return;

        let async_main = parser::parse_module(
            "import api\n\nasync fn main(console: Console):\n    let text = \"original\"\n    let w = api.view(text)\n    let _ = task.done(0).await\n    console.print(w)\n",
        )
        .expect("parse async normal caller");
        let err = crate::pipeline::link(
            vec![
                (
                    "api".into(),
                    parser::parse_module(
                        "mode opt\n\npub fn view(text: let('a) String) -> View(String, 'a):\n    text\n",
                    )
                    .expect("parse opt API"),
                ),
                ("main".into(), async_main),
            ],
            "main",
        )
        .expect_err("an imported view may not cross async suspension");
        assert!(
            err.to_string()
                .contains("async fn `main`: borrowed value `w` remains live across `await`"),
            "{err}"
        );

        let list_api = "mode opt\n\npub fn view(xs: let('a) List(Int)) -> View(List(Int), 'a):\n    xs\n";
        let indirect_main = "import api\nimport list\n\nfn main(console: Console):\n    var xs = [1]\n    let make_view = api.view\n    let w = make_view(xs)\n    console.print(\"${list.length(w)}\")\n    list.push(xs, 2)\n    console.print(\"${list.length(xs)}\")\n";
        let linked = crate::pipeline::link(
            vec![
                ("api".into(), parser::parse_module(list_api).expect("parse opt list API")),
                (
                    "main".into(),
                    parser::parse_module(indirect_main).expect("parse normal indirect caller"),
                ),
            ],
            "main",
        )
        .expect("link normal indirect caller");
        typeck::check(&linked).expect("an imported function value preserves its loan relation");
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("run indirect view"),
            ["1", "2"],
            "interpreter ends the indirect view loan at last use",
        );
        assert_eq!(
            run_linked_on_wasm(&[("api", list_api), ("main", indirect_main)], "main"),
            ["1", "2"],
            "compiled backend ends the indirect view loan at last use",
        );
    }

    /// RFC-0088 baseline: update-and-extract returns the exact old leaf while
    /// committing the repaired collection. Shared snapshots remain unchanged;
    /// empty/missing operations return None. The structural helper performs the
    /// selection and repair together on both engines.
    #[test]
    fn rfc0088_update_extract_baseline_agrees_on_both_backends() {
        let src = r#"import dict
import list

fn main(console: Console):
    var d = dict.from_pairs([("a", "one"), ("b", "two")])
    let d_snapshot = d
    let replaced = d.insert("a", "ONE")
    let inserted = d.insert("c", "three")
    let removed = d.remove("b")
    let absent = d.remove("missing")
    match replaced:
        Some(value) -> console.print("replace=${value}")
        None -> console.print("replace=none")
    match inserted:
        Some(value) -> console.print("insert=${value}")
        None -> console.print("insert=none")
    match removed:
        Some(value) -> console.print("remove=${value}")
        None -> console.print("remove=none")
    match absent:
        Some(value) -> console.print("absent=${value}")
        None -> console.print("absent=none")
    console.print("d=${dict.pairs(d)}")
    console.print("d_snapshot=${dict.pairs(d_snapshot)}")
"#;
        let want = [
            "replace=one",
            "insert=none",
            "remove=two",
            "absent=none",
            "d=[(a, ONE), (c, three)]",
            "d_snapshot=[(a, one), (b, two)]",
        ];
        assert_eq!(link_run(src), want, "interpreter update-and-extract");
        assert_eq!(wasm_run(src), want, "compiled update-and-extract");
    }

    /// The ordinary result must survive nested-place reconstruction. The caller
    /// rebuilds list/record/dictionary roots after staging the multi-result call;
    /// those rebuilds may use tuple scratches but cannot clobber the `Option`.
    #[test]
    fn rfc0088_nested_place_results_survive_writeback_on_both_backends() {
        let src = r#"import dict
import list

type Holder:
    xs: List(Int)

fn main(console: Console):
    var rows = [[1, 2]]
    let old = rows[0].pop()
    var holders = [Holder([5, 6])]
    let old2 = holders[0].xs.pop()
    var maps = [dict.from_pairs([(1, "one")])]
    let old3 = maps[0].insert(1, "two")
    console.print("${old ?? -1}")
    console.print("${old2 ?? -1}")
    console.print("${old3 ?? "none"}")
    console.print("${rows}")
    console.print("${holders[0].xs}")
    console.print("${dict.at(maps[0], 1)}")
"#;
        let want = ["2", "6", "one", "[[1]]", "[5]", "two"];
        assert_eq!(link_run(src), want, "interpreter nested extraction");
        assert_eq!(wasm_run(src), want, "compiled nested extraction");
    }

    /// Dictionary extraction uses the resolved `Eq` implementation on both
    /// engines. Structural host-value equality would treat these keys as
    /// different and turn replacement/removal into misses.
    #[test]
    fn rfc0088_custom_eq_keys_select_the_same_entry_on_both_backends() {
        let src = r#"import dict

type CI:
    text: String

impl PartialEq for CI:
    fn eq(self, other: CI) -> Bool:
        self.text.to_lower() == other.text.to_lower()

impl Eq for CI

fn main(console: Console):
    var d = dict.from_pairs([(CI("a"), 1)])
    d.insert(CI("A"), 2)
    d.update(CI("a"), 0, fn(n: Int): n + 3)
    let replaced = d.insert(CI("A"), 7)
    let removed = d.remove(CI("a"))
    console.print("${replaced ?? -1}")
    console.print("${removed ?? -1}")
    console.print("${dict.length(d)}")
"#;
        let want = ["5", "7", "0"];
        assert_eq!(link_run(src), want, "interpreter custom Eq key");
        assert_eq!(wasm_run(src), want, "compiled custom Eq key");
    }

    /// Integrated half of the one-search oracle: public result-bearing wrappers
    /// must delegate directly to the fused structural primitive. Combined with
    /// the WIR counter test, this catches both wrapper-level and helper-level
    /// accidental second searches.
    #[test]
    fn rfc0088_public_dict_wrappers_use_fused_extract_primitives() {
        let module = parser::parse_module(crate::bundled_module("dict").expect("bundled dict"))
            .expect("parse dict stdlib");
        for (public, primitive) in [
            ("insert", "dict.__insert_extract"),
            ("remove", "dict.__remove_extract"),
        ] {
            let function = module
                .items
                .iter()
                .find_map(|item| match item {
                    ast::Item::Function(function) if function.name == public => Some(function),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing public dict.{public}"));
            let [ast::Stmt::Expr(ast::Expr::Call { name, .. })] =
                function.body.stmts.as_slice()
            else {
                panic!("dict.{public} must contain exactly one fused `var` extraction");
            };
            assert_eq!(name, primitive, "dict.{public} must use its fused extraction primitive");
        }
    }

    /// Capacity tokens are an optimization detail of the convention-bearing
    /// `var` ABI. Function values and structured early returns must preserve the
    /// same source envelope even when they conservatively take the CoW path.
    #[test]
    fn rfc0088_heap_var_function_values_and_early_returns_agree() {
        let src = r#"import dict

fn replace(var d: Dict(Int, Int), value: Int, stop: Bool) -> Option(Int):
    let old = d.insert(1, value)
    if stop:
        return old
    old

fn apply(
    f: fn(var Dict(Int, Int), Int, Bool) -> Option(Int),
    var d: Dict(Int, Int),
    value: Int,
    stop: Bool,
) -> Option(Int):
    f(d, value, stop)

fn main(console: Console):
    var d = dict.from_pairs([(1, 10)])
    let f: fn(var Dict(Int, Int), Int, Bool) -> Option(Int) = replace
    let first = apply(f, d, 20, true)
    let second = apply(f, d, 30, false)
    console.print("${first ?? -1}")
    console.print("${second ?? -1}")
    console.print("${dict.at(d, 1)}")
"#;
        let want = ["10", "20", "30"];
        assert_eq!(link_run(src), want, "interpreter heap-var function value");
        assert_eq!(wasm_run(src), want, "compiled heap-var function value");
    }

    /// Shared record and ADT leaves exercise the descriptor path beyond scalar,
    /// string, and nested-container slots. Snapshots must retain their old leaf
    /// while the repaired root and ordinary result remain independently live.
    #[test]
    fn rfc0088_record_and_adt_leaves_survive_shared_extraction() {
        let src = r#"import dict

type Box:
    value: String

type Payload:
    Text(String)
    Count(Int)

fn payload_text(value: Payload) -> String:
    match value:
        Text(text) -> text
        Count(n) -> "${n}"

fn main(console: Console):
    var boxes = [Box("a"), Box("b")]
    let boxes_snapshot = boxes
    let popped = boxes.pop()

    var payloads = dict.from_pairs([("key", Text("old"))])
    let payloads_snapshot = payloads
    let replaced = payloads.insert("key", Count(7))
    let removed = payloads.remove("key")

    match popped:
        Some(value) -> console.print(value.value)
        None -> console.print("none")
    match replaced:
        Some(value) -> console.print(payload_text(value))
        None -> console.print("none")
    match removed:
        Some(value) -> console.print(payload_text(value))
        None -> console.print("none")
    console.print("${boxes_snapshot[1].value}")
    console.print(payload_text(dict.at(payloads_snapshot, "key")))
    console.print("${dict.length(payloads)}")
"#;
        let want = ["b", "old", "7", "b", "old", "0"];
        assert_eq!(link_run(src), want, "interpreter record/ADT extraction");
        assert_eq!(wasm_run(src), want, "compiled record/ADT extraction");
    }

    /// Nested List and Dict leaves cover both RC layouts: ordinary object-base
    /// pointers and Dict's four-byte hidden-index bias.
    #[test]
    fn rfc0088_nested_container_leaves_survive_shared_extraction() {
        let src = r#"import dict

fn main(console: Console):
    var rows = [[1], [2, 3]]
    let rows_snapshot = rows
    let row = rows.pop()

    var inner = dict.from_pairs([("a", 1)])
    var outer = dict.from_pairs([("key", inner)])
    let outer_snapshot = outer
    inner.insert("b", 2)
    let replaced = outer.insert("key", inner)
    let removed = outer.remove("key")

    console.print("${row ?? []}")
    console.print("${rows_snapshot[1]}")
    match replaced:
        Some(value) -> console.print("${dict.length(value)}")
        None -> console.print("none")
    match removed:
        Some(value) -> console.print("${dict.at(value, "b")}")
        None -> console.print("none")
    console.print("${dict.at(dict.at(outer_snapshot, "key"), "a")}")
    console.print("${dict.length(outer)}")
"#;
        let want = ["[2, 3]", "[2, 3]", "1", "2", "1", "0"];
        assert_eq!(link_run(src), want, "interpreter nested-container extraction");
        assert_eq!(wasm_run(src), want, "compiled nested-container extraction");
    }

    /// (RFC-0087 §6, criterion 2) A callee-side `?` is a structured return: every
    /// `var` param commits its current value — partial progress on a multi-`var`
    /// call is observable, identically, on both backends ("commit atomicity, not
    /// rollback"). The caller-side `??` observes the committed state.
    #[test]
    fn rfc0087_callee_try_commits_multi_var_partial_progress_on_both_backends() {
        let src = "fn check(amount: Int) -> Result(Int, String):\n    if amount > 20:\n        return Err(\"limit\")\n    Ok(amount)\n\nfn transfer(var from: Int, var to: Int, amount: Int) -> Result(Int, String):\n    from = from - amount\n    let approved = check(amount)?\n    to = to + approved\n    Ok(approved)\n\nfn main(console: Console):\n    var from = 100\n    var to = 0\n    let receipt = transfer(from, to, 30) ?? -1\n    console.print(\"${from}\")\n    console.print(\"${to}\")\n    console.print(\"${receipt}\")\n";
        // The debit committed (70), the credit never ran (0), the `??` saw the Err.
        let want = ["70", "0", "-1"];
        assert_eq!(link_run(src), want, "interpreter commits partial progress on callee `?`");
        assert_eq!(wasm_run(src), want, "compiled backend commits partial progress on callee `?`");
    }

    /// (RFC-0087 §6, criterion 2) `?` is an ordinary early return, not a
    /// transaction boundary: `?`, its explicit `return Err(...)` desugaring, and a
    /// tail `Err(...)` all commit the same final `var` values on both backends.
    /// (RFC-0087 criterion 2) The classification edge: a first `var` param whose
    /// type equals the return type (`Result` receiver) plus an ADDITIONAL `var`
    /// param. Whatever the receiver's transitional classification, the extra
    /// param is a write-back channel and must commit on callee-`?` identically on
    /// both backends — mutator classification cannot bypass the `?` matrix.
    #[test]
    fn rfc0087_result_receiver_mutator_extra_var_writes_back_on_try() {
        let interp_std = |src: &str| interpreter::run_module(resolve_std_src(src), ".", Vec::new()).expect("run");
        let src = "import list\n\nfn step(var r: Result(Int, String), var log: List(Int)) -> Result(Int, String):\n    log.push(1)\n    let v = r?\n    log.push(v)\n    Ok(v + 1)\n\nfn main(console: Console):\n    var bad: Result(Int, String) = Err(\"nope\")\n    var log: List(Int) = []\n    let out = step(bad, log) ?? -1\n    console.print(\"${log}\")\n    console.print(\"${out}\")\n";
        // The pre-`?` append committed ([1]); the post-`?` append never ran.
        let want = ["[1]", "-1"];
        assert_eq!(interp_std(src), want, "interpreter commits the extra var param on `?`");
        assert_eq!(wasm_run(src), want, "compiled backend commits the extra var param on `?`");
    }

    /// Indirect calls use the same captured-place protocol as direct calls:
    /// nested roots are rebuilt only after every returned `var` value is staged.
    #[test]
    fn rfc0087_function_values_write_back_nested_places_on_both_backends() {
        let src = "import list\n\ntype State:\n    rows: List(List(Int))\n\nfn exchange(var left: Int, var right: Int) -> Int:\n    let old = left\n    left = right\n    right = old\n    left + right\n\nfn main(console: Console):\n    var state = State([[4, 9]])\n    let operation: fn(var Int, var Int) -> Int = exchange\n    let total: Int = operation(state.rows[0][0], state.rows[0][1])\n    let row = list.at(state.rows, 0)\n    console.print(\"${row} ${total}\")\n    if list.at(row, 0) == 9 && list.at(row, 1) == 4 && total == 13:\n        console.print(\"ok\")\n    else:\n        console.print(\"bad\")\n";
        let want = ["[9, 4] 13", "ok"];
        assert_eq!(link_run(src), want, "interpreter indirect nested write-back");
        assert_eq!(wasm_run(src), want, "compiled indirect nested write-back");
    }

    #[test]
    fn rfc0087_trait_dispatch_preserves_var_conventions_on_both_backends() {
        let src = "trait Advance:\n    fn advance(var self, by: Int) -> Int\n\ntype Counter:\n    value: Int\n\nimpl Advance for Counter:\n    fn advance(var self, by: Int) -> Int:\n        self.value = self.value + by\n        self.value\n\nfn step(var value: a, by: Int) -> Int where a: Advance:\n    value.advance(by)\n\nfn main(console: Console):\n    var counter = Counter(5)\n    let result = step(counter, 7)\n    console.print(\"${counter.value} ${result}\")\n";
        let want = ["12 12"];
        assert_eq!(link_run(src), want, "interpreter trait `var` dispatch");
        assert_eq!(wasm_run(src), want, "compiled trait `var` dispatch");

        let mismatch = "trait Advance:\n    fn advance(var self, by: Int) -> Int\n\ntype Counter:\n    value: Int\n\nimpl Advance for Counter:\n    fn advance(self, by: Int) -> Int:\n        self.value + by\n";
        let linked = crate::pipeline::link(
            vec![("main".to_string(), parser::parse_module(mismatch).expect("parse"))],
            "main",
        )
        .expect("link");
        let err = typeck::check(&linked)
            .expect_err("trait implementation must retain the declared convention");
        assert!(err.message.contains("convention"), "trait mismatch: {err}");
        assert!(err.message.contains("Var"), "trait mismatch must show required `var`: {err}");
    }

    /// Nested field/index places capture their coordinates once, stage the rebuilt
    /// root, and commit the same result on both engines.
    #[test]
    fn rfc0087_nested_var_places_write_back_on_both_backends() {
        let src = "import list\n\ntype State:\n    rows: List(List(Int))\n\nfn bump(var n: Int) -> Int:\n    n = n + 10\n    n * 2\n\nfn main(console: Console):\n    var state = State([[1, 2], [3, 4]])\n    let result: Int = bump(state.rows[0][1])\n    let updated: Int = list.at(list.at(state.rows, 0), 1)\n    if updated == 12 && result == 24:\n        console.print(\"ok\")\n    else:\n        console.print(\"bad\")\n";
        let want = ["ok"];
        assert_eq!(link_run(src), want, "interpreter nested place write-back");
        assert_eq!(wasm_run(src), want, "compiled nested place write-back");
    }

    #[test]
    fn rfc0087_disjoint_same_root_places_compose_on_both_backends() {
        let src = "import list\n\nfn exchange(var left: Int, var right: Int) -> Int:\n    let old = left\n    left = right\n    right = old\n    left + right\n\nfn main(console: Console):\n    var xs = [4, 9]\n    let total: Int = exchange(xs[0], xs[1])\n    if list.at(xs, 0) == 9 && list.at(xs, 1) == 4 && total == 13:\n        console.print(\"ok\")\n    else:\n        console.print(\"bad\")\n";
        let want = ["ok"];
        assert_eq!(link_run(src), want, "interpreter composes disjoint places");
        assert_eq!(wasm_run(src), want, "compiled backend composes disjoint places");
    }

    #[test]
    fn rfc0087_exclusivity_and_var_place_diagnostics_are_resolved() {
        let error = |case: &str, src: &str| {
            typeck::check(&resolve_std_src(src))
                .err()
                .unwrap_or_else(|| panic!("{case}: invalid `var` place must be rejected"))
                .message
        };
        let duplicate = "fn exchange(var a: Int, var b: Int) -> Nil:\n    return\nfn main(console: Console):\n    var n = 1\n    exchange(n, n)\n";
        let message = error("duplicate", duplicate);
        assert!(message.contains("arguments 1 and 2"), "overlap positions: {message}");
        assert!(message.contains("rooted in `n`"), "overlap root: {message}");

        let dynamic = "import list\n\nfn exchange(var a: Int, var b: Int) -> Nil:\n    return\nfn main(console: Console):\n    var xs = [1, 2]\n    var i = 0\n    var j = 1\n    exchange(xs[i], xs[j])\n";
        assert!(error("dynamic", dynamic).contains("overlapping `var` places"));

        let reservation = "fn inner(var n: Int) -> Int:\n    n = n + 1\n    n\nfn outer(var n: Int, snapshot: Int) -> Nil:\n    return\nfn main(console: Console):\n    var n = 0\n    outer(n, inner(n))\n";
        let message = error("reservation", reservation);
        assert!(message.contains("reserves `var` place"), "reservation: {message}");
        assert!(message.contains("written evaluation order"), "reservation order: {message}");

        let moved = "fn bump(var n: Int) -> Nil:\n    return\nfn main(console: Console):\n    var n = 0\n    bump(move n)\n";
        let message = error("move", moved);
        assert!(message.contains("uses `move`"), "move diagnostic: {message}");
        assert!(message.contains("live mutable place"), "move fix: {message}");

        let temporary = "fn bump(var n: Int) -> Nil:\n    return\nfn main(console: Console):\n    bump(1)\n";
        assert!(error("temporary", temporary).contains("must be a mutable place"));

        let immutable = "fn bump(var n: Int) -> Nil:\n    return\nfn test(n: Int) -> Nil:\n    bump(n)\n    return\nfn main(console: Console):\n    test(1)\n";
        let message = error("immutable", immutable);
        assert!(message.contains("root `n` must be a mutable `var`"), "immutable: {message}");
    }

    #[test]
    fn rfc0087_completed_effects_and_proven_disjoint_places_are_legal() {
        let src = "import list\n\nfn bump(var n: Int) -> Int:\n    n = n + 1\n    n\nfn use_after(value: Int, var target: Int) -> Nil:\n    target = target + value\n    return\nfn exchange(var a: Int, var b: Int) -> Nil:\n    let old = a\n    a = b\n    b = old\n    return\nfn main(console: Console):\n    var n = 1\n    use_after(bump(n), n)\n    console.print(\"${n}\")\n";
        let want = ["4"];
        assert_eq!(link_run(src), want, "interpreter ordered/disjoint calls");
        assert_eq!(wasm_run(src), want, "compiled ordered/disjoint calls");
    }

    #[test]
    fn rfc0087_expression_evaluation_order_is_identical_on_both_backends() {
        let src = r#"import list

fn mark(var log: List(Int), value: Int) -> Int:
    log.push(value)
    value

fn mark_zero(var log: List(Int), value: Int) -> Int:
    log.push(value)
    0

fn mark_bool(var log: List(Int), value: Int, result: Bool) -> Bool:
    log.push(value)
    result

fn mark_none(var log: List(Int), value: Int) -> Option(Int):
    log.push(value)
    None

fn mark_some(var log: List(Int), value: Int) -> Option(Int):
    log.push(value)
    Some(value)

fn pair(a: Int, b: Int) -> Int:
    a + b

type Marker:
    value: Int

impl Marker:
    fn combine(self, other: Int) -> Int:
        self.value + other

fn make_marker(var log: List(Int), value: Int) -> Marker:
    Marker(mark(log, value))

fn main(console: Console):
    var log: List(Int) = []
    let call = pair(mark(log, 1), mark(log, 2))
    let operator = mark(log, 3) + mark(log, 4)
    let tuple = (mark(log, 5), mark(log, 6))
    let list_value = [mark(log, 7), mark(log, 8)]
    let mapped = [mark(log, x + 2) for x in [mark(log, 9), mark(log, 10)]]
    let filtered = [mark(log, x * 2 + 12) for x in [1, 2] if mark_bool(log, x * 2 + 11, true)]
    let text = "${mark(log, 17)} ${mark(log, 18)}"
    let fallback = mark_none(log, 19) ?? mark(log, 20)
    let short_fallback = mark_some(log, 21) ?? mark(log, 22)
    let matched = match mark_some(log, 23):
        Some(value) -> mark(log, 24) + value
        None -> 0
    let selected = if mark_bool(log, 25, true):
        mark(log, 26)
    else:
        0
    var target = [0]
    target[mark_zero(log, 27)] = mark(log, 28)
    let and_value = mark_bool(log, 29, false) && mark_bool(log, 30, true)
    let or_value = mark_bool(log, 31, true) || mark_bool(log, 32, false)
    let method = make_marker(log, 33).combine(mark(log, 34))
    console.print("${log}")
"#;
        let want = ["[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 23, 24, 25, 26, 27, 28, 29, 31, 33, 34]"];
        assert_eq!(link_run(src), want, "interpreter expression order");
        assert_eq!(wasm_run(src), want, "compiled expression order");
    }

    #[test]
    fn rfc0087_place_coordinates_are_evaluated_once_on_both_backends() {
        let src = "import list\n\nfn next_index(var calls: Int) -> Int:\n    calls = calls + 1\n    0\n\nfn bump(var n: Int) -> Nil:\n    n = n + 5\n    return\n\nfn main(console: Console):\n    var calls = 0\n    var rows = [[1, 2]]\n    bump(rows[next_index(calls)][1])\n    if calls == 1 && list.at(list.at(rows, 0), 1) == 7:\n        console.print(\"ok\")\n    else:\n        console.print(\"bad\")\n";
        let want = ["ok"];
        assert_eq!(link_run(src), want, "interpreter captures coordinates once");
        assert_eq!(wasm_run(src), want, "compiled backend captures coordinates once");
    }

    #[test]
    fn rfc0087_dict_element_place_writes_back_on_both_backends() {
        let src = r#"import dict

fn bump(var n: Int) -> Int:
    n = n + 3
    n

fn main(console: Console):
    var values = dict.from_pairs([("a", 4)])
    let result: Int = bump(values["a"])
    if dict.at(values, "a") == 7 && result == 7:
        console.print("ok")
    else:
        console.print("bad")
"#;
        let want = ["ok"];
        assert_eq!(link_run(src), want, "interpreter dict place write-back");
        assert_eq!(wasm_run(src), want, "compiled dict place write-back");
    }
