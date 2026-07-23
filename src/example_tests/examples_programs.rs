use super::*;
use crate::{interpreter, parser, typeck};

    /// `examples/calc/src/calc.witchy` — a recursive-descent arithmetic evaluator — honors
    /// operator precedence and left-associativity, and reports division-by-zero
    /// and parse errors through `Result`. A pure (Console-only) tour of recursive
    /// enums + pattern matching.
    #[test]
    fn calc_example_evaluates_with_precedence_and_errors() {
        assert_eq!(
            crate::execute_file("examples/calc/src/calc.witchy", Vec::new()).unwrap(),
            vec![
                "2 + 3 * 4       => 14",
                "(2 + 3) * 4     => 20",
                "100 - 2 - 3     => 95",
                "2 * (10 - 1)    => 18",
                "8 / (4 - 4)     => error: division by zero",
                "2 * (3 +        => error: unexpected end of input",
            ]
        );
        let src = std::fs::read_to_string("examples/calc/src/calc.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/wrap/src/wrap.witchy` — greedy word wrapping — packs space-separated
    /// words onto lines within a column width, breaking before overflow, and
    /// frames each padded line. Pure string handling; agrees on both backends.
    #[test]
    fn wrap_example_greedily_wraps_to_width() {
        assert_eq!(
            crate::execute_file("examples/wrap/src/wrap.witchy", Vec::new()).unwrap(),
            vec![
                "wrapped to 20 columns:",
                "| The quick brown fox  |",
                "| jumps over the lazy  |",
                "| dog and then keeps   |",
                "| on running far away  |",
            ]
        );
    }

    /// `examples/dijkstra/src/dijkstra.witchy` — single-source shortest paths in a weighted
    /// directed graph — settles the nearest node, relaxes edges, then prints
    /// every distance and one reconstructed path. Returns a tuple of parallel
    /// arrays, so it also covers tuple-return + `let (a, b) =` on both backends.
    #[test]
    fn dijkstra_example_finds_shortest_paths() {
        assert_eq!(
            crate::execute_file("examples/dijkstra/src/dijkstra.witchy", Vec::new()).unwrap(),
            vec![
                "shortest distances from A:",
                "  A = 0",
                "  B = 3",
                "  C = 1",
                "  D = 4",
                "  E = 7",
                "path to E: A -> C -> B -> D -> E",
            ]
        );
    }

    /// `examples/queens/src/queens.witchy` — N-queens by backtracking — counts all 92
    /// solutions for the 8x8 board and renders the first (column-order DFS). Deep
    /// recursion with an early-exit search; agrees on both backends.
    #[test]
    fn queens_example_counts_and_renders_first_board() {
        assert_eq!(
            crate::execute_file("examples/queens/src/queens.witchy", Vec::new()).unwrap(),
            vec![
                "8-queens solutions: 92",
                "first solution:",
                "Q.......",
                "....Q...",
                ".......Q",
                ".....Q..",
                "..Q.....",
                "......Q.",
                ".Q......",
                "...Q....",
            ]
        );
    }

    /// `examples/regex/src/regex_demo.witchy` — a tiny K&P-style regex matcher (literals, `.`,
    /// `*`, `^`, `$`) — matches a battery of pattern/text pairs. Every step is a
    /// two-`list.at(..)` character comparison, so it stresses content comparison on
    /// both backends.
    #[test]
    fn regex_example_matches_literals_dot_star_anchors() {
        assert_eq!(
            crate::execute_file("examples/regex/src/regex_demo.witchy", Vec::new()).unwrap(),
            vec![
                "/abc/     \"abc\"           match",
                "/a.c/     \"axc\"           match",
                "/a.c/     \"ac\"            no match",
                "/a*b/     \"aaab\"          match",
                "/a*b/     \"b\"             match",
                "/^hello/  \"hello world\"   match",
                "/world$/  \"hello world\"   match",
                "/^a.*z$/  \"abcz\"          match",
                "/^a.*z$/  \"abc\"           no match",
            ]
        );
    }

    /// `examples/brainfuck/src/brainfuck.witchy` — a full brainfuck interpreter — runs the
    /// canonical "Hello World!" program and a second that prints 'A', building
    /// output by indexing a printable-ASCII literal (no chr/ord builtin). The
    /// instruction dispatch compares `list.at(code, pc)` against operator literals,
    /// so it's another both-backends guard for content comparison.
    #[test]
    fn brainfuck_example_runs_hello_world() {
        assert_eq!(
            crate::execute_file("examples/brainfuck/src/brainfuck.witchy", Vec::new()).unwrap(),
            vec!["Hello World!", "A"]
        );
    }

    /// `examples/diff/src/diff.witchy` — an LCS line diff — fills the longest-common-
    /// subsequence table and backtracks into unchanged/removed/added lines. The
    /// backtrack compares `list.at(old, i) == list.at(new, j)` (two `List(String)` element
    /// reads), so it also guards content comparison on both backends.
    #[test]
    fn diff_example_emits_lcs_line_diff() {
        assert_eq!(
            crate::execute_file("examples/diff/src/diff.witchy", Vec::new()).unwrap(),
            vec![
                "  apple",
                "- banana",
                "  cherry",
                "  date",
                "+ elderberry",
                "  fig",
            ]
        );
    }

    /// `examples/rpn/src/rpn.witchy` — a stack-machine reverse-Polish calculator — folds
    /// tokens through an operand stack and reports underflow / division-by-zero
    /// through `Result`. Pure (Console), both backends.
    #[test]
    fn rpn_example_evaluates_postfix_with_a_stack() {
        assert_eq!(
            crate::execute_file("examples/rpn/src/rpn.witchy", Vec::new()).unwrap(),
            vec![
                "3 4 +               => 7",
                "5 1 2 + 4 * + 3 -   => 14",
                "10 2 /              => 5",
                "1 0 /               => error: division by zero",
                "1 +                 => error: stack underflow at `+`",
            ]
        );
    }

    /// `examples/maze/src/maze.witchy` — BFS shortest path through a grid maze, with a
    /// `prev` Dict for path reconstruction. Pure (Console); interpreter-hosted.
    #[test]
    fn maze_example_finds_shortest_path_by_bfs() {
        let out = crate::execute_file("examples/maze/src/maze.witchy", Vec::new())
            .unwrap()
            .join("\n");
        assert!(out.contains("shortest path: 14 steps"), "distance: {out}");
        assert!(
            out.contains("#S#***# #") && out.contains("### ###*#"),
            "route marked: {out}"
        );
    }

    /// `examples/sudoku/src/sudoku.witchy` — a backtracking solver over immutable boards —
    /// solves the canonical puzzle to its unique solution. Pure (Console),
    /// recursion + Option-backtracking heavy.
    #[test]
    fn sudoku_example_solves_by_backtracking() {
        let out = crate::execute_file("examples/sudoku/src/sudoku.witchy", Vec::new())
            .unwrap()
            .join("\n");
        assert!(
            out.contains("solved:\n534678912\n672195348\n198342567\n859761423"),
            "unique solution: {out}"
        );
        let src = std::fs::read_to_string("examples/sudoku/src/sudoku.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/life/src/life.witchy` — Conway's Game of Life over a `List(List(Bool))` —
    /// evolves a glider through its phases by the B3/S23 rule. Pure (Console),
    /// nested-list heavy, and identical on both backends.
    #[test]
    fn life_example_evolves_a_glider() {
        let out = crate::execute_file("examples/life/src/life.witchy", Vec::new())
            .unwrap()
            .join("\n");
        // Generation 0 is the seeded glider.
        assert!(
            out.contains("generation 0:\n.#......\n..#.....\n###....."),
            "seed glider: {out}"
        );
        // After 3 steps it has drifted down-and-right into its next phase.
        assert!(
            out.contains("generation 3:\n........\n.#......\n..##....\n.##....."),
            "evolved glider: {out}"
        );
        let src = std::fs::read_to_string("examples/life/src/life.witchy").unwrap();
        let fp = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(crate::capabilities::show_caps(&fp.total), "Console");
    }

    /// `examples/rle/src/rle.witchy` — run-length encoding and its inverse — collapses
    /// runs to "<count><char>" and expands them back, verifying decode∘encode is
    /// the identity. Pure string processing; identical on both backends. (Its
    /// run-counting loop is what exposed the two-`at`-results comparison gap.)
    #[test]
    fn rle_example_round_trips_runs() {
        assert_eq!(
            crate::execute_file("examples/rle/src/rle.witchy", Vec::new()).unwrap(),
            vec![
                "\"aaabbbbc\" -> \"3a4b1c\"  roundtrip ok",
                "\"wwwwww\" -> \"6w\"  roundtrip ok",
                "\"abcdef\" -> \"1a1b1c1d1e1f\"  roundtrip ok",
                "\"mississippi\" -> \"1m1i2s1i2s1i2p1i\"  roundtrip ok",
                "\"\" -> \"\"  roundtrip ok",
            ]
        );
    }

    /// `main` may declare a `List(String)` parameter to receive command-line
    /// arguments — argv is input data, not authority, so it's an ordinary value
    /// parameter passed by the host (here `run_module_args`), not a capability.
    #[test]
    fn main_receives_command_line_args() {
        let run = |args: Vec<String>| -> Vec<String> {
            let src = "fn main(console: Console, args: List(String)):\n    console.print(list.join(args, \",\"))\n";
            let module = parser::parse_module(src).expect("parse");
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
            typeck::check(&linked).expect("typecheck");
            interpreter::run_module_args(linked, ".", Vec::new(), args).expect("run")
        };
        assert_eq!(run(vec!["a".into(), "b".into(), "c".into()]), vec!["a,b,c"]);
        assert_eq!(run(Vec::new()), vec![""]); // empty argv -> empty list -> ""
    }

    #[test]
    fn config_merge_example_runs_on_wasm() {
        // The layered-config example (json.merge shallow override + encode_pretty)
        // prints identically on both backends: base.debug survives, production
        // overrides host/port and adds workers.
        let sources = [
            ("json", crate::bundled_module("json").unwrap()),
            ("main", include_str!("../../examples/config_merge/src/config_merge.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "config_merge diverged");
        assert_eq!(
            compiled,
            vec![
                "{\n  \"debug\": true,\n  \"host\": \"example.com\",\n  \"port\": 443,\n  \"workers\": 8\n}",
                "has workers",
            ]
        );
    }

    #[test]
    fn durations_example_runs_on_wasm() {
        // The durations example (literals + Duration*Int + comparison + the
        // duration module) prints identically on both backends.
        let sources = [
            ("duration", crate::bundled_module("duration").unwrap()),
            ("main", include_str!("../../examples/durations/src/durations.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "durations example diverged");
        assert_eq!(
            compiled,
            vec!["1s", "2s", "4s", "5s", "5s", "1:30:00", "true", "2m30s"]
        );
    }

    #[test]
    fn dice_example_runs_on_wasm() {
        // The dice example (seeded prng.next_below, threaded Rng) prints the
        // same deterministic rolls on both backends.
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("prng", crate::bundled_module("prng").unwrap()),
            ("main", include_str!("../../examples/dice/src/dice.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "dice example diverged");
        assert_eq!(compiled, vec!["2 2 1 6 2 2 1 5 2 2", "total: 25"]);
    }

    /// The Fahrenheit-to-Celsius table (K&R / Go tour), reproduced in witchy. It
    /// needs real float output — `math.format_float` makes it compile and agree
    /// on both backends, which the float-formatting-less `to_string` could not.
    #[test]
    fn temperature_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/temperature/src/temperature.witchy").unwrap();
        let sources = [("math", crate::bundled_module("math").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "temperature diverged");
        assert_eq!(compiled[0], "0F = -17.8C");
        assert_eq!(compiled[1], "60F = 15.6C");
    }

    #[test]
    fn plugin_host_example_runs_on_wasm() {
        // The capability-thesis demo: a list of function-value plugins applied as
        // a pipeline, plus a console-capturing logger closure — identical on both
        // backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../../examples/plugin_host/src/plugin_host.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "plugin_host diverged");
        assert_eq!(
            compiled,
            vec!["1 -> 12", "5 -> 20", "10 -> 30", "[log] ran the pipeline"]
        );
    }

    #[test]
    fn bst_example_runs_on_wasm() {
        // The binary search tree (recursive ADT + pattern matching + tree sort)
        // produces identical output on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("main", include_str!("../../examples/bst/src/bst.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "bst diverged");
        assert_eq!(
            compiled,
            vec!["1 2 3 4 5 6 7 8 9", "contains 7: true", "contains 10: false"]
        );
    }

    #[test]
    fn generic_stack_example_runs_on_wasm() {
        // A recursive generic ADT `Stack(a)` used at two instantiations (Int and
        // String) with a generic `Option(a)` peek produces identical output on
        // both backends — parametric polymorphism end to end.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../../examples/generic_stack/src/generic_stack.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "generic_stack diverged");
        assert_eq!(
            compiled,
            vec![
                "nums size:  3",
                "words size: 2",
                "nums top:   1",
                "words top:  first",
                "rev nums top:  3",
                "rev words top: second",
            ]
        );
    }

    #[test]
    fn ranges_example_runs_on_wasm() {
        // Integer range patterns (`lo..hi`, `lo..=hi`) are real `Pattern::IntRange`
        // nodes (RFC-0052), so the HTTP-status and grade classifiers match
        // identically on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../../examples/ranges/src/ranges.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "ranges diverged");
        assert_eq!(
            compiled,
            vec![
                "200 -> success",
                "204 -> success",
                "301 -> redirect",
                "404 -> client error",
                "503 -> server error",
                "600 -> unknown",
                "95 -> A",
                "83 -> B",
                "71 -> C",
                "42 -> F",
            ]
        );
    }

    #[test]
    fn roman_example_runs_on_wasm() {
        // Greedy table walk by subscript (to_roman) and a char scan with the
        // subtractive rule (from_roman) round-trip identically on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../../examples/roman/src/roman.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "roman diverged");
        assert_eq!(
            compiled,
            vec![
                "4 = IV -> 4",
                "9 = IX -> 9",
                "49 = XLIX -> 49",
                "90 = XC -> 90",
                "1994 = MCMXCIV -> 1994",
                "2024 = MMXXIV -> 2024",
            ]
        );
    }

    #[test]
    fn constants_example_runs_on_wasm() {
        // Top-level constants (including ones built from earlier constants) are
        // inlined before both backends, producing identical output.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../../examples/constants/src/constants.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "constants diverged");
        assert_eq!(
            compiled,
            vec![
                "1 hour      = 3600s",
                "1 day       = 86400s",
                "1d 2h 3m 4s = 93784s",
            ]
        );
    }

    #[test]
    fn aliases_example_runs_on_wasm() {
        // Type aliases are expanded before both backends everywhere a type is
        // written — signatures/fields AND body-level positions: the `let`
        // ascription (`hottest: Celsius`), the lambda's alias-typed parameter and
        // return (`Converter`), the `as` narrow through a capability alias
        // (`console as Out`), and the impl head (`impl Describe for Celsius`). So the
        // conversions, averaging, and `.describe()` all agree (RFC H1).
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../../examples/aliases/src/aliases.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "aliases diverged");
        assert_eq!(compiled, vec!["avg C = 21", "25C = 77F", "0C  = 32F", "hottest = 25C = 77F"]);
    }

    #[test]
    fn regex_example_runs_on_wasm() {
        // The std/regex backtracking matcher (. * + ? ^ $) produces identical
        // results on both backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("regex", crate::bundled_module("regex").unwrap()),
            ("main", include_str!("../../examples/patterns/src/patterns.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "regex diverged");
        assert_eq!(
            compiled,
            vec![
                "match  ^h.*o$  ~  hello",
                "no     ^h.*o$  ~  hi there",
                "match  colou?r  ~  color",
                "match  colou?r  ~  colour",
                "match  ab+a  ~  abbba",
                "no     ab+a  ~  aa",
                "match  cat  ~  the cat sat",
                "no     ^cat  ~  the cat sat",
            ]
        );
    }

    #[test]
    fn calculator_example_runs_on_wasm() {
        // The recursive-descent calculator (mutual recursion + tuple cursors +
        // string scanning) parses and evaluates expressions identically on both
        // backends.
        let sources = [
            ("string", crate::bundled_module("string").unwrap()),
            ("option", crate::bundled_module("option").unwrap()),
            ("main", include_str!("../../examples/calculator/src/calculator.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "calculator diverged");
        assert_eq!(
            compiled,
            vec![
                "2 + 3 * 4        = 14",
                "(2 + 3) * 4      = 20",
                "100 - 2 * (3 + 4) = 86",
                "7 + 6 / 2 - 1    = 9",
            ]
        );
    }

    #[test]
    fn pipeline_example_runs_on_wasm() {
        // The method-chained data pipeline (filter/map/sum over list.range)
        // prints identically on both backends.
        let sources = [
            ("option", crate::bundled_module("option").unwrap()),
            ("list", crate::bundled_module("list").unwrap()),
            ("string", crate::bundled_module("string").unwrap()),
            ("main", include_str!("../../examples/pipeline/src/pipeline.witchy")),
        ];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "pipeline diverged");
        assert_eq!(compiled, vec!["120", "0,2,4,6,8"]);
    }

    #[test]
    fn parse_kv_example_runs_on_wasm() {
        // The `key=value` parser example compiles end-to-end: index_of +
        // substring + string_length + ends_with + Bool interpolation, matching the
        // interpreter. `.index_of` resolves to the std `string.index_of`
        // (Option-returning, RFC-0044), so it needs the std-linking `wasm_run`.
        assert_eq!(
            wasm_run(include_str!("../../examples/parse_kv/src/parse_kv.witchy")),
            vec!["timeout", "30", "true"]
        );
    }

    #[test]
    fn compute_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../../examples/compute/src/compute.witchy")),
            vec!["217"]
        );
    }

    #[test]
    fn early_return_runs_on_wasm() {
        // Guard-clause early returns compile to valid WASM and run.
        // classify(-5) = -1, classify(0) = 0, classify(9) = 1; sum = 0.
        let src = r#"
fn classify(n: Int) -> Int:
    if (n < 0):
        return (0 - 1)
    if (n == 0):
        return 0
    1

fn main() -> Int:
    ((classify((0 - 5)) + classify(0)) + classify(9))
"#;
        assert_eq!(run_on_wasm(src), vec!["0"]);
    }

    #[test]
    fn shapes_runs_on_wasm() {
        assert_eq!(
            run_on_wasm(include_str!("../../examples/shapes/src/shapes.witchy")),
            vec!["325"]
        );
    }

    /// Real examples (not toy snippets) compile and run on the WASM backend,
    /// matching the interpreter — a concrete check of codegen breadth.
    #[test]
    fn eval_example_runs_on_wasm() {
        assert_eq!(run_on_wasm(include_str!("../../examples/eval/src/eval.witchy")), vec!["20"]);
    }

    #[test]
    fn bank_example_runs_on_wasm() {
        // Records + lists + for-in + Result + `?` together, compiled to WASM.
        assert_eq!(
            run_on_wasm(include_str!("../../examples/bank/src/bank.witchy")),
            vec!["total = 150", "remaining: 90", "error: insufficient funds for bob"]
        );
    }

    // A Net capability is an allow-list, and attenuation only ever narrows it.
    // These rejections fire on the allow-list check, before any socket is
    // opened, so the test needs no network. (`run_with` grants the root Net.)
    /// A library imported into a program brings its functions into scope but no
    /// authority: `lib` has no capability parameters, so it can only compute.
    #[test]
    fn imported_library_is_pure_and_confined() {
        let lib = r#"
pub fn label(n: Int) -> String:
    if (n < 0):
        "neg"
    else:
        "nonneg"
"#;
        let main = r#"
import lib

fn main(console: Console):
    console.print(lib.label((-2)))
    console.print(lib.label(7))
"#;
        let out = interpreter::run_program(&[("lib", lib), ("main", main)], "main")
            .expect("multi-module program runs");
        assert_eq!(out, vec!["neg", "nonneg"]);
    }

    #[test]
    fn compute_example_returns_value() {
        assert_eq!(
            crate::execute_file("examples/compute/src/compute.witchy", Vec::new()).unwrap(),
            vec!["217"]
        );
    }

    #[test]
    fn shapes_example_returns_value() {
        assert_eq!(
            crate::execute_file("examples/shapes/src/shapes.witchy", Vec::new()).unwrap(),
            vec!["325"]
        );
    }

    /// `largest` reproduces the generic function from The Rust Programming
    /// Language ch. 10: a `where a: Ord` bound finds the biggest element of a
    /// list, for `Int` and for a user `Version` type with an `Ord` impl (the
    /// trait's derived `greater` dispatches correctly through monomorphization).
    #[test]
    fn largest_example_agrees_on_both_backends() {
        let client = std::fs::read_to_string("examples/largest/src/largest.witchy").unwrap();
        let sources = [("cmp", crate::bundled_module("cmp").unwrap()), ("main", client.as_str())];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "largest diverged");
        assert_eq!(
            compiled,
            vec!["largest number: 100".to_string(), "latest version: 2.0".to_string()]
        );
    }

    /// `minigrep` is the CLI search tool from The Rust Programming Language ch. 12,
    /// reproduced in witchy: it takes a query and a file path as args, reads the
    /// file with a `Dir[Read]` capability, and prints the matching lines. Missing
    /// args print usage and exit 1 (the conventional process exit code).
    #[test]
    fn minigrep_example_searches_a_file_like_the_rust_book() {
        let (out, code) = crate::execute_file_exit(
            "examples/minigrep/src/minigrep.witchy",
            Vec::new(),
            Vec::new(),
            vec!["nobody".into(), "examples/data/poem.txt".into()],
            None,
            Vec::new(),
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            out,
            vec!["I'm nobody! Who are you?".to_string(), "Are you nobody, too?".to_string()]
        );
        // No args: usage message and a non-zero exit code.
        let (out, code) =
            crate::execute_file_exit("examples/minigrep/src/minigrep.witchy", Vec::new(), Vec::new(), Vec::new(), None, Vec::new())
                .unwrap();
        assert_eq!(code, 1);
        assert_eq!(out, vec!["usage: minigrep <query> <file>".to_string()]);
    }

    #[test]
    fn hello_example() {
        assert_eq!(
            interp(include_str!("../../examples/hello/src/hello.witchy")),
            vec!["hello, witchy", "8 doubled is 16", "negative"]
        );
    }

    #[test]
    fn mutate_example() {
        assert_eq!(
            interp(include_str!("../../examples/mutate/src/mutate.witchy")),
            vec!["bumped to 3"]
        );
    }

    #[test]
    fn ownership_example() {
        assert_eq!(
            interp(include_str!("../../examples/ownership/src/ownership.witchy")),
            vec!["[witchy]"]
        );
    }

    #[test]
    fn guard_example_runs_on_wasm() {
        // Early `return` from a function and from inside a `for` loop.
        let src = include_str!("../../examples/guard/src/guard.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["negative", "zero", "positive", "8", "-1"]);
    }

    #[test]
    fn generics_example_runs_on_wasm() {
        // A generic `swap((a, b)) -> (b, a)` on a mixed (Int, String) tuple:
        // tuple pattern match + construction through a generic function.
        let src = include_str!("../../examples/generics/src/generics.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["answer", "42"]);
    }

    #[test]
    fn signs_example_runs_on_wasm() {
        // Negative-literal match patterns (`-1 -> ...`).
        let src = include_str!("../../examples/signs/src/signs.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
        assert_eq!(run_on_wasm(src), vec!["left", "right", "stay", "?"]);
    }

    #[test]
    fn mutate_example_runs_on_wasm() {
        // `var` (move-in / move-out) compiles: the example agrees with the
        // interpreter through the WASM backend.
        let src = include_str!("../../examples/mutate/src/mutate.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
    }

    #[test]
    fn ownership_example_runs_on_wasm() {
        // `own` (consume / move ownership) compiles and agrees across backends.
        let src = include_str!("../../examples/ownership/src/ownership.witchy");
        assert_eq!(interp(src), run_on_wasm(src));
    }

    #[test]
    fn commands_example_runs_and_compiles() {
        let src = include_str!("../../examples/commands/src/commands.witchy");
        assert_eq!(interp(src), vec!["total is 1"]);
        assert_fn_compiles(src);
    }

    #[test]
    fn runs_a_file_with_file_based_imports() {
        let dir = std::env::temp_dir().join(format!("witchy_cli_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("strutil.witchy"),
            r#"
pub fn shout(s: String) -> String:
    ("HI " + s)
"#,
        )
        .unwrap();
        let app = dir.join("app.witchy");
        std::fs::write(
            &app,
            "import strutil\nfn main(console: Console):\n    console.print(strutil.shout(\"x\"))\n",
        )
        .unwrap();

        let out = crate::execute_file(app.to_str().unwrap(), Vec::new()).unwrap();
        assert_eq!(out, vec!["HI x"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generics_example() {
        assert_eq!(
            interp(include_str!("../../examples/generics/src/generics.witchy")),
            vec!["answer", "42"]
        );
    }

    #[test]
    fn result_example() {
        assert_eq!(
            interp(include_str!("../../examples/result/src/result_demo.witchy")),
            vec!["ok 5", "err divide by zero"]
        );
    }

    #[test]
    fn try_example() {
        assert_eq!(
            interp(include_str!("../../examples/try/src/try.witchy")),
            vec!["= 11", "error: divide by zero", "error: divide by zero"]
        );
    }

    #[test]
    fn eval_example() {
        assert_eq!(interp(include_str!("../../examples/eval/src/eval.witchy")), vec!["20"]);
    }

    #[test]
    fn bank_example() {
        assert_eq!(
            interp(include_str!("../../examples/bank/src/bank.witchy")),
            vec![
                "total = 150",
                "remaining: 90",
                "error: insufficient funds for bob"
            ]
        );
    }

    #[test]
    fn guard_example() {
        assert_eq!(
            interp(include_str!("../../examples/guard/src/guard.witchy")),
            vec!["negative", "zero", "positive", "8", "-1"]
        );
    }

    #[test]
    fn signs_example() {
        assert_eq!(
            interp(include_str!("../../examples/signs/src/signs.witchy")),
            vec!["left", "right", "stay", "?"]
        );
    }

    #[test]
    fn parse_kv_example() {
        // Uses `setting.index_of("=")` → std `string.index_of` (now Option-returning,
        // RFC-0044), so it must link std — `link_run` pulls in the `string` prelude,
        // where the plain `interp` (builtins only) cannot resolve the std function.
        assert_eq!(
            link_run(include_str!("../../examples/parse_kv/src/parse_kv.witchy")),
            vec!["timeout", "30", "true"]
        );
    }

    #[test]
    fn fizzbuzz_example() {
        assert_eq!(
            interp(include_str!("../../examples/fizzbuzz/src/fizzbuzz.witchy")),
            vec![
                "1", "2", "Fizz", "4", "Buzz", "Fizz", "7", "8", "Fizz", "Buzz", "11", "Fizz",
                "13", "14", "FizzBuzz"
            ]
        );
    }

    #[test]
    fn compute_example_compiles() {
        assert_fn_compiles(include_str!("../../examples/compute/src/compute.witchy"));
    }

    #[test]
    fn shapes_example_compiles() {
        assert_fn_compiles(include_str!("../../examples/shapes/src/shapes.witchy"));
    }
