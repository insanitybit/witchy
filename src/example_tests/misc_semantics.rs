use super::*;
use crate::{codegen, interpreter, parser, typeck};

    #[test]
    fn public_sources_do_not_call_legacy_render_intrinsic() {
        fn collect(root: &std::path::Path, suffix: &str, out: &mut Vec<std::path::PathBuf>) {
            if root.is_file() {
                if root.to_string_lossy().ends_with(suffix) {
                    out.push(root.to_path_buf());
                }
                return;
            }
            for entry in std::fs::read_dir(root).unwrap_or_else(|_| panic!("read {}", root.display())) {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect(&path, suffix, out);
                } else if path.to_string_lossy().ends_with(suffix) {
                    out.push(path);
                }
            }
        }

        let mut paths = Vec::new();
        for root in ["std", "examples"] {
            collect(std::path::Path::new(root), ".witchy", &mut paths);
        }
        for root in ["README.md", "book", "spec", "rfcs/performance-modes.md"] {
            collect(std::path::Path::new(root), ".md", &mut paths);
        }
        paths.sort();

        let mut offenders = Vec::new();
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("read {}", path.display()));
            if text.contains("__render(") {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "public source/docs must use interpolation or show.render, not __render:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn generic_type_aliases_resolve_on_linked_path() {
        // BUG-563: parameterized aliases were accepted at the declaration site
        // but every `Pair(Int)` use reached type resolution as an unknown type.
        let src = "type Pair(a) = (a, a)\ntype Rows(a) = List(Pair(a))\n\nfn first(p: Pair(Int)) -> Int:\n    p.0\n\nfn main(console: Console):\n    let rows: Rows(String) = [(\"a\", \"b\")]\n    console.print(\"${first((1, 2))}:${list.length(rows)}\")\n";
        assert_eq!(link_run(src), vec!["1:1"]);
    }

    /// (BUG-546) Sealed domain values display through their public canonical
    /// formatter, not their private constructor-shaped representation.
    #[test]
    fn sealed_domain_values_use_canonical_show_on_both_backends() {
        let src = "import show\nimport semver\nimport url\nimport time\n\nfn main(console: Console):\n    let v = semver.version(1, 2, 3)\n    let d = time.from_unix(0)\n    match url.parse(\"https://example.com/p\"):\n        Ok(u) ->\n            show.say(console, v)\n            show.say(console, u)\n            show.say(console, d)\n            console.print(\"${v}\")\n            console.print(\"${u}\")\n            console.print(\"${d}\")\n            console.print(show.render([v, semver.version(2, 0, 0)]))\n            console.print(show.render(Some(u)))\n        Err(e) -> console.print(url.url_error_message(e))\n";
        let expected = [
            "1.2.3",
            "https://example.com/p",
            "1970-01-01T00:00:00Z",
            "1.2.3",
            "https://example.com/p",
            "1970-01-01T00:00:00Z",
            "[1.2.3, 2.0.0]",
            "Some(https://example.com/p)",
        ];
        assert_eq!(link_run(src), expected, "interp: sealed domain Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: sealed domain Show",
        );
    }

    /// Type heads are not runtime values. The resolver may keep ambient type
    /// names bare (`Int`, `Set`, `Tuple2`, a local sum type name), but type
    /// checking must reject them before codegen instead of letting them look like
    /// unknown constructors with fresh result types.
    #[test]
    fn type_names_are_rejected_as_values_after_linking() {
        let cases = [
            (
                "builtin constructor-looking call",
                "fn main(console: Console):\n    Int(1)\n    console.print(\"bad\")\n",
                "type `Int` is not a value",
            ),
            (
                "prelude type",
                "fn main(console: Console):\n    Result\n    console.print(\"bad\")\n",
                "type `Result` is not a value",
            ),
            (
                "synthetic tuple type",
                "fn main(console: Console):\n    Tuple2(1, 2)\n    console.print(\"bad\")\n",
                "type `Tuple2` is not a value",
            ),
            (
                "local sum type name",
                "type Color:\n    Red\n    Blue\n\nfn main(console: Console):\n    Color\n    console.print(\"bad\")\n",
                "type `Color` is not a value",
            ),
        ];

        for (label, src, want) in cases {
            let module = parser::parse_module(src).expect(label);
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
                .unwrap_or_else(|e| panic!("{label}: link failed: {}", e.message));
            let err = typeck::check(&linked).expect_err(label);
            assert!(
                err.message.contains(want),
                "{label}: expected `{want}`, got `{}`",
                err.message
            );
        }
    }

    /// (BUG-216) A local binding with the same name as a prelude/imported module
    /// owns dotted calls consistently. `string.to_upper("x")` below must dispatch
    /// to the local `S` method, not silently escape to std String.to_upper.
    #[test]
    fn shadowing_module_name_keeps_dotted_calls_on_local() {
        let src = "type S:\n    x: String\n\nimpl S:\n    fn to_upper(self: S, suffix: String) -> String:\n        self.x + suffix\n\nfn module_upper(s: String) -> String:\n    s.to_upper()\n\nfn main(console: Console):\n    let string = S(\"s\")\n    console.print(string.to_upper(\"x\"))\n    console.print(module_upper(\"y\"))\n";
        let expected = ["sx", "Y"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
    }

    /// (SEC-045) An overflowing `Content-Length` must NOT crash the server. The old
    /// `content_length` guarded with `ascii.all_digits` (which passes an arbitrarily
    /// long digit string) then called `string.to_int`, which TRAPS on i64 overflow —
    /// an unauthenticated remote crash. The fix parses totally with `string.parse_int`
    /// (returns None on overflow) and treats a rejected value as no body (0). This
    /// mirrors `server.content_length` and must agree + not trap on both backends.
    #[test]
    fn overflowing_content_length_does_not_trap_on_either_backend() {
        // `ascii.all_digits` accepts the overflowing string (the old trap trigger),
        // but the total parse yields 0 (no body) rather than aborting the VM.
        let src = "import ascii\nimport option\n\n\
                   fn content_length_val(v: String) -> Int:\n\
                   \x20   match v.parse_int():\n\
                   \x20       Some(n) -> if n > 0: n else: 0\n\
                   \x20       None -> 0\n\n\
                   fn main(console: Console):\n\
                   \x20   let big = \"99999999999999999999999999\"\n\
                   \x20   console.print(\"${ascii.all_digits(big)}\")\n\
                   \x20   console.print(\"${content_length_val(big)}\")\n\
                   \x20   console.print(\"${content_length_val(\"42\")}\")\n\
                   \x20   console.print(\"${content_length_val(\"abc\")}\")\n";
        let want = ["true", "0", "42", "0"];
        assert_eq!(link_run(src), want, "interp: overflow -> 0, valid -> value, junk -> 0");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "wasm must agree and not trap");
    }

    /// (BUG-335, spec §16) `main` may return only Nil/Int/Float; a String/Bool/List
    /// return is a CHECK-TIME error (the interpreter echoes it but the compiled run
    /// wrapper drops it — a silent divergence, now rejected loud by construction).
    #[test]
    fn off_spec_main_return_is_check_error() {
        assert!(typeck::check_str("fn main(console: Console) -> String:\n    \"oops\"\n").is_err(), "String main rejected");
        assert!(typeck::check_str("fn main(console: Console) -> Bool:\n    true\n").is_err(), "Bool main rejected");
        assert!(typeck::check_str("fn main(console: Console) -> List(Int):\n    [1, 2]\n").is_err(), "List main rejected");
        typeck::check_str("fn main(console: Console) -> Int:\n    0\n").expect("Int main ok");
        typeck::check_str("fn main(console: Console) -> Float:\n    2.5\n").expect("Float main ok");
        typeck::check_str("fn main(console: Console):\n    console.print(\"hi\")\n").expect("Nil main ok");
    }

    /// RFC-0006 regression: an IMPORTED tag used inside a NON-`main` function
    /// expands and runs identically on both backends. This locks in the
    /// infinite-recursion fix in `tagged::expand`: to RUN a tag the compiler links
    /// a synthetic comptime program, and `linker::link` re-runs `tagged::expand`
    /// per module — so if the comptime program still carried the CONSUMER's
    /// tag-bearing function (`render`, with its unexpanded `box"…"`), expansion
    /// would loop forever (rebuild the program → expand the tag again → …) and
    /// overflow the stack. The fix prunes the comptime program to only the items
    /// REACHABLE FROM THE TAG (its callees + the types they name), which excludes
    /// `render`/`main`, so the program holds no tagged literals and terminates.
    /// Shape mirrors the glamour `html"…"`-in-`view` case that triggered the bug.
    #[test]
    fn imported_tag_in_non_main_fn_agrees_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        // The tag-defining module: a tiny `box"…"` tag that emits source wrapping
        // each hole in `widget.unwrap(widget.wrap(…))`. The `Wrapped` type + `wrap`/`unwrap`
        // helpers exercise the reachable-TYPES half of the prune (the tag's
        // signature/body reach `Wrapped`, so it must be kept for the comptime
        // program to type-check), and prove a tag works when defined in an
        // IMPORTED rune, not just locally.
        let widget = "type Wrapped:\n    Wrap(String)\n\npub fn unwrap(w: Wrapped) -> String:\n    match w:\n        Wrap(s) -> s\n\npub fn wrap(s: String) -> Wrapped:\n    Wrap(s)\n\npub fn box(parts: List(String), holes: List(String)) -> String:\n    var out = \"widget.unwrap(widget.wrap(\\\"\"\n    var i = 0\n    let n = list.length(parts)\n    for p in parts:\n        out = out + p\n        if i < n - 1:\n            out = out + \"\\\" + \" + list.at(holes, i) + \" + \\\"\"\n        i = i + 1\n    out + \"\\\"))\"\n";
        // The CONSUMER: the tag appears in `render`, a NON-`main` function. This is
        // the exact shape that recursed before the fix (cf. glamour's `view`).
        let app = "import widget\n\nfn render(x: String) -> String:\n    box\"[${x}]\"\n\nfn main(console: Console):\n    console.print(render(\"hi\"))\n";

        let want = vec!["[hi]".to_string()];
        let link = || {
            let app_m = parser::parse_module(app).expect("parse app");
            let widget_m = parser::parse_module(widget).expect("parse widget");
            crate::pipeline::link(
                vec![("main".into(), app_m), ("widget".into(), widget_m)],
                "main",
            )
            .expect("link (must not overflow the stack)")
        };

        let linked = link();
        typeck::check(&linked).expect("typecheck");
        let interp_out = interpreter::run_module(linked, ".", Vec::new()).expect("interp run");
        assert_eq!(interp_out, want, "interpreter");

        let linked = link();
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, ..Default::default() }, 4)
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
    }

    /// An early `return` inside an `var` function must agree on both backends.
    /// An var function yields multiple results (the declared return plus one per
    /// var param), so an early return reproduces that epilogue: it pushes each
    /// var param's current value before returning. (Regression for the
    /// interpreter-only return-in-var gap.)
    #[test]
    fn return_in_var_fn_agrees_on_both_backends() {
        let src = "fn clamp(var n: Int):\n    if (n > 10):\n        n = 10\n        return\n    n = n + 1\n\nfn main(console: Console):\n    var a = 5\n    clamp(a)\n    console.print(\"${a}\")\n    var b = 50\n    clamp(b)\n    console.print(\"${b}\")\n";
        let want = vec!["6".to_string(), "10".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `float_to_int` on infinities or out-of-range finite values must saturate
    /// the same way on both backends. NaN is deliberately excluded here: BUG-466
    /// makes NaN a loud contract error, covered by `math_to_int_nan_aborts_on_both_backends`.
    #[test]
    fn wasm_float_to_int_saturates_like_the_interpreter() {
        let src = "fn main(console: Console):\n    console.print(\"${math.to_int(1.0 / 0.0)}\")\n    console.print(\"${math.to_int(0.0 - 1.0 / 0.0)}\")\n    console.print(\"${math.to_int(0.0 - 3.9)}\")\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
            "-3".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `clock.now_monotonic()` yields monotonic elapsed nanoseconds — a steady
    /// clock for measuring durations (used by the benchmark harness to time the
    /// compute kernel, excluding process startup). The absolute value is
    /// nondeterministic, so parity is asserted on a *derived* property (elapsed is
    /// non-negative and the kernel result is identical) that both backends agree on.
    #[test]
    fn now_monotonic_measures_elapsed_on_both_backends() {
        let src = "fn spin(n: Int) -> Int:\n    var a = 0\n    var i = 0\n    while i < n:\n        a = a + i\n        i = i + 1\n    a\n\nfn main(console: Console, clock: Clock):\n    let t0 = clock.now_monotonic()\n    let r = spin(1000)\n    let t1 = clock.now_monotonic()\n    console.print(\"${r}\")\n    console.print(\"${t1 - t0 >= 0}\")\n";
        let expected = vec!["499500".to_string(), "true".to_string()];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
        // Like `now`, it needs a Clock — another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let t = now_monotonic(c)\n").is_err());
        // The Clock requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module(
                "fn main(console: Console, clock: Clock):\n    let t = clock.now_monotonic()\n",
            )
            .expect("parse"),
        );
        assert!(fp.total.contains_key("Clock"), "Clock should appear in the footprint");
    }

    /// The reference interpreter and the compiled WASM backend must produce the
    /// same output for the same program — the core promise of witchy's two-tier
    /// design. This differential test exercises a spread of features and asserts
    /// agreement directly (no hardcoded expectations), so a future codegen change
    /// that silently diverges from the interpreter is caught. Programs stay
    /// within the compiled backend's supported semantics (notably 32-bit Int).
    #[test]
    fn interpreter_and_compiled_backends_agree() {
        let programs: &[(&str, &str)] = &[
            (
                "arithmetic + control flow",
                r#"
fn main(console: Console):
    var acc = 0
    var i = 0
    while (i < 12):
        if ((i % 2) == 0):
            acc = (acc + i)
        else:
            acc = (acc - i)
        i = (i + 1)
    console.print("${acc}")
"#,
            ),
            (
                "records + update + field access",
                r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(3, 4)
    let q = Point(x: ((p).x + 10), ..p)
    console.print("${((q).x * (q).y)}")
"#,
            ),
            (
                "lists + recursion + head/tail match",
                r#"
fn sum(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [h, ..t] -> (h + sum(t))

fn main(console: Console):
    console.print("${sum([1, 2, 3, 4, 5])}")
"#,
            ),
            (
                "ADTs + match",
                r#"
type Shape:
    Circle(Int)
    Rect(Int, Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> ((3 * r) * r)
        Rect(w, h) -> (w * h)

fn main(console: Console):
    console.print("${(area(Circle(5)) + area(Rect(3, 4)))}")
"#,
            ),
            (
                "capturing closures + higher-order",
                r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let k = 100
    console.print("${apply(fn(n: Int): (n + k), 5)}")
"#,
            ),
            (
                "dicts",
                r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    dict.insert(d, "a", 9)
    console.print("${(dict.get_or(d, "a", 0) + dict.length(d))}")
"#,
            ),
            (
                "strings",
                r#"
fn main(console: Console):
    console.print("a,b,c".replace(",", "-"))
    console.print("${"hello".contains("l")}")
    console.print("hello".substring(1, 4))
    for w in "the cat sat".split(" "):
        console.print(w)
"#,
            ),
            (
                "string equality across a List(String) parameter",
                r#"
fn count_matches(words: List(String), target: String) -> Int:
    var n = 0
    for w in words:
        if (w == target):
            n = (n + 1)
    n

fn main(console: Console):
    let words = "apple banana apple cherry apple".split(" ")
    console.print("${count_matches(words, "apple")}")
"#,
            ),
            (
                "string equality + ordering",
                r#"
fn main(console: Console):
    let a = "xapple".substring(1, 6)
    console.print("${(a == "apple")}")
    console.print("${(a == "apricot")}")
    console.print("${(a != "apricot")}")
    console.print("${("apple" < "banana")}")
    console.print("${("banana" < "apple")}")
    console.print("${("app" < "apple")}")
    console.print("${("apple" <= "apple")}")
"#,
            ),
            (
                "tuples + polymorphic to_string",
                r#"
fn main(console: Console):
    let (a, b) = (7, 8)
    console.print("${(a + b)}")
    console.print("${(a < b)}")
    console.print("${"done"}")
"#,
            ),
            (
                // Regression (M7): an inline `else:` ending in a bare identifier,
                // immediately followed by a `"${...}"` interpolation, must parse as
                // two statements (not `count(...)`). (Builtins/prelude only — this
                // harness doesn't link std modules.)
                "inline else bare-ident before an interpolation",
                r#"
fn describe(n: Int) -> String:
    let label = if n < 0: "neg" else: "pos"
    let mag = if n < 0: 0 - n else: n
    "${label}:${mag}"

fn main(console: Console):
    console.print(describe(0 - 4250))
    console.print(describe(150000))
"#,
            ),
        ];
        for (name, src) in programs {
            let interpreted = interp(src);
            let compiled = run_on_wasm(src);
            assert_eq!(
                interpreted, compiled,
                "interpreter and compiled backends diverged for `{name}`"
            );
        }
    }

    // A guard on a constructor pattern must bind the field first, then test it
    // (`Yep(n) if n > 10`), and fall through to the next arm when the guard
    // fails. Mutual recursion exercises forward references between compiled
    // functions. Both must agree across backends.
    #[test]
    fn adt_guards_and_mutual_recursion_backends_agree() {
        let src = r#"
type Opt:
    Nope
    Yep(Int)

fn describe(o: Opt) -> String:
    match o:
        Yep(n) if (n > 10) -> "big"
        Yep(n) -> "small"
        Nope -> "none"

fn is_even(n: Int) -> Bool:
    if (n == 0):
        true
    else:
        is_odd((n - 1))

fn is_odd(n: Int) -> Bool:
    if (n == 0):
        false
    else:
        is_even((n - 1))

fn main(console: Console):
    console.print(describe(Yep(50)))
    console.print(describe(Yep(3)))
    console.print(describe(Nope))
    console.print("${if is_even(10): 1 else: 0}")
    console.print("${if is_odd(7): 1 else: 0}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "adt guards / mutual recursion diverged");
        assert_eq!(run_on_wasm(src), vec!["big", "small", "none", "1", "1"]);
    }

    // A recursive ADT (binary tree) with nested constructor patterns, exercised
    // by two recursive traversals (sum and depth). Recursion through a
    // heap-allocated ADT and destructuring `Node(l, v, r)` must agree across
    // backends.
    #[test]
    fn recursive_tree_adt_backends_agree() {
        let src = r#"
type Tree:
    Leaf
    Node(Tree, Int, Tree)

fn sum_tree(t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, v, r) -> ((sum_tree(l) + v) + sum_tree(r))

fn depth(t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, v, r) ->
            let dl = depth(l)
            let dr = depth(r)
            (1 + if (dl > dr): dl else: dr)

fn main(console: Console):
    let t = Node(Node(Leaf, 1, Node(Leaf, 5, Leaf)), 2, Node(Leaf, 3, Leaf))
    console.print("${sum_tree(t)}")
    console.print("${depth(t)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "recursive tree ADT diverged");
        assert_eq!(run_on_wasm(src), vec!["11", "3"]);
    }

    // Regression: a local variable that shares its name with a same-module
    // function must stay a local, not be rewritten into a first-class reference
    // to that function by the linker. (The function-as-value feature qualifies
    // bare function-name Vars; it must skip names shadowed by a local.)
    #[test]
    fn local_shadowing_function_name_backends_agree() {
        let client = r#"
fn size(n: Int) -> Int:
    (n * 100)

fn main(console: Console):
    var size = 3
    size = (size + 4)
    console.print("${size}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "local shadowing a function name diverged");
        assert_eq!(compiled, vec!["7"]);
    }
