use crate::{interpreter, parser, typeck};

/// Independent expected-value oracle for shipped data-oriented examples. Backend parity
/// belongs to `example_sweeps`, which runs every example under each optimization
/// configuration; repeating that sweep per row only hid this compact contract.
#[test]
fn data_oriented_examples_match_golden_output() {
    let cases: &[(&str, &[&str])] = &[
        (
            "examples/calc/src/calc.witchy",
            &[
                "2 + 3 * 4       => 14",
                "(2 + 3) * 4     => 20",
                "100 - 2 - 3     => 95",
                "2 * (10 - 1)    => 18",
                "8 / (4 - 4)     => error: division by zero",
                "2 * (3 +        => error: unexpected end of input",
            ],
        ),
        (
            "examples/wrap/src/wrap.witchy",
            &[
                "wrapped to 20 columns:",
                "| The quick brown fox  |",
                "| jumps over the lazy  |",
                "| dog and then keeps   |",
                "| on running far away  |",
            ],
        ),
        (
            "examples/dijkstra/src/dijkstra.witchy",
            &[
                "shortest distances from A:",
                "  A = 0",
                "  B = 3",
                "  C = 1",
                "  D = 4",
                "  E = 7",
                "path to E: A -> C -> B -> D -> E",
            ],
        ),
        (
            "examples/queens/src/queens.witchy",
            &[
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
            ],
        ),
        (
            "examples/regex/src/regex_demo.witchy",
            &[
                "/abc/     \"abc\"           match",
                "/a.c/     \"axc\"           match",
                "/a.c/     \"ac\"            no match",
                "/a*b/     \"aaab\"          match",
                "/a*b/     \"b\"             match",
                "/^hello/  \"hello world\"   match",
                "/world$/  \"hello world\"   match",
                "/^a.*z$/  \"abcz\"          match",
                "/^a.*z$/  \"abc\"           no match",
            ],
        ),
        ("examples/brainfuck/src/brainfuck.witchy", &["Hello World!", "A"]),
        (
            "examples/diff/src/diff.witchy",
            &[
                "  apple",
                "- banana",
                "  cherry",
                "  date",
                "+ elderberry",
                "  fig",
            ],
        ),
        (
            "examples/rpn/src/rpn.witchy",
            &[
                "3 4 +               => 7",
                "5 1 2 + 4 * + 3 -   => 14",
                "10 2 /              => 5",
                "1 0 /               => error: division by zero",
                "1 +                 => error: stack underflow at `+`",
            ],
        ),
        (
            "examples/rle/src/rle.witchy",
            &[
                "\"aaabbbbc\" -> \"3a4b1c\"  roundtrip ok",
                "\"wwwwww\" -> \"6w\"  roundtrip ok",
                "\"abcdef\" -> \"1a1b1c1d1e1f\"  roundtrip ok",
                "\"mississippi\" -> \"1m1i2s1i2s1i2p1i\"  roundtrip ok",
                "\"\" -> \"\"  roundtrip ok",
            ],
        ),
        (
            "examples/config_merge/src/config_merge.witchy",
            &[
                "{\n  \"debug\": true,\n  \"host\": \"example.com\",\n  \"port\": 443,\n  \"workers\": 8\n}",
                "has workers",
            ],
        ),
        (
            "examples/durations/src/durations.witchy",
            &["1s", "2s", "4s", "5s", "5s", "1:30:00", "true", "2m30s"],
        ),
        (
            "examples/dice/src/dice.witchy",
            &["2 2 1 6 2 2 1 5 2 2", "total: 25"],
        ),
        (
            "examples/plugin_host/src/plugin_host.witchy",
            &["1 -> 12", "5 -> 20", "10 -> 30", "[log] ran the pipeline"],
        ),
        (
            "examples/bst/src/bst.witchy",
            &["1 2 3 4 5 6 7 8 9", "contains 7: true", "contains 10: false"],
        ),
        (
            "examples/generic_stack/src/generic_stack.witchy",
            &[
                "nums size:  3",
                "words size: 2",
                "nums top:   1",
                "words top:  first",
                "rev nums top:  3",
                "rev words top: second",
            ],
        ),
        (
            "examples/ranges/src/ranges.witchy",
            &[
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
            ],
        ),
        (
            "examples/roman/src/roman.witchy",
            &[
                "4 = IV -> 4",
                "9 = IX -> 9",
                "49 = XLIX -> 49",
                "90 = XC -> 90",
                "1994 = MCMXCIV -> 1994",
                "2024 = MMXXIV -> 2024",
            ],
        ),
        (
            "examples/constants/src/constants.witchy",
            &["1 hour      = 3600s", "1 day       = 86400s", "1d 2h 3m 4s = 93784s"],
        ),
        (
            "examples/aliases/src/aliases.witchy",
            &["avg C = 21", "25C = 77F", "0C  = 32F", "hottest = 25C = 77F"],
        ),
        (
            "examples/patterns/src/patterns.witchy",
            &[
                "match  ^h.*o$  ~  hello",
                "no     ^h.*o$  ~  hi there",
                "match  colou?r  ~  color",
                "match  colou?r  ~  colour",
                "match  ab+a  ~  abbba",
                "no     ab+a  ~  aa",
                "match  cat  ~  the cat sat",
                "no     ^cat  ~  the cat sat",
            ],
        ),
        (
            "examples/calculator/src/calculator.witchy",
            &[
                "2 + 3 * 4        = 14",
                "(2 + 3) * 4      = 20",
                "100 - 2 * (3 + 4) = 86",
                "7 + 6 / 2 - 1    = 9",
            ],
        ),
        ("examples/pipeline/src/pipeline.witchy", &["120", "0,2,4,6,8"]),
        ("examples/parse_kv/src/parse_kv.witchy", &["timeout", "30", "true"]),
        ("examples/compute/src/compute.witchy", &["217"]),
        ("examples/shapes/src/shapes.witchy", &["325"]),
        ("examples/eval/src/eval.witchy", &["20"]),
        (
            "examples/bank/src/bank.witchy",
            &["total = 150", "remaining: 90", "error: insufficient funds for bob"],
        ),
        (
            "examples/hello/src/hello.witchy",
            &["hello, witchy", "8 doubled is 16", "negative"],
        ),
        (
            "examples/guard/src/guard.witchy",
            &["negative", "zero", "positive", "8", "-1"],
        ),
        ("examples/generics/src/generics.witchy", &["answer", "42"]),
        ("examples/signs/src/signs.witchy", &["left", "right", "stay", "?"]),
        ("examples/mutate/src/mutate.witchy", &["bumped to 3"]),
        ("examples/ownership/src/ownership.witchy", &["[witchy]"]),
        ("examples/commands/src/commands.witchy", &["total is 1"]),
        (
            "examples/result/src/result_demo.witchy",
            &["ok 5", "err divide by zero"],
        ),
        (
            "examples/try/src/try.witchy",
            &["= 11", "error: divide by zero", "error: divide by zero"],
        ),
        (
            "examples/fizzbuzz/src/fizzbuzz.witchy",
            &[
                "1", "2", "Fizz", "4", "Buzz", "Fizz", "7", "8", "Fizz", "Buzz", "11",
                "Fizz", "13", "14", "FizzBuzz",
            ],
        ),
        ("examples/std_demo/src/std_demo.witchy", &["30", "3"]),
        (
            "examples/sort/src/sort.witchy",
            &["1,1,3,4,5", "5,4,3,1,1"],
        ),
        ("examples/math_demo/src/math_demo.witchy", &["7", "5", "10", "1024", "12"]),
        ("examples/floats/src/floats.witchy", &["4.0", "3.5", "5.0", "1.0"]),
        ("examples/list_more/src/list_more.witchy", &["true", "3", "-1", "20", "30"]),
        ("examples/list_pipeline/src/list_pipeline.witchy", &["233", "2 8", "735"]),
        (
            "examples/zip/src/zip.witchy",
            &["0:alice 1:bob 2:carol", "alice=30 bob=25 carol=40"],
        ),
        ("examples/predicates/src/predicates.witchy", &["true", "true", "false", "false"]),
        ("examples/option_std/src/option_std.witchy", &["10", "-1"]),
        (
            "examples/text/src/text.witchy",
            &["ALICE | BOB | CAROL", "===", "alice,***,carol"],
        ),
        (
            "examples/jq/src/jq.witchy",
            &[
                "user.name       => \"Ada\"",
                "user.roles      => [\"admin\",\"dev\"]",
                "user.roles.0    => \"admin\"",
                "user.roles.1    => \"dev\"",
                "count           => 42",
                "active          => true",
                "user.missing    => (no such path)",
            ],
        ),
        (
            "examples/traits/src/traits.witchy",
            &[
                "square with area 25",
                "rectangle with area 12",
                "right triangle with area 12",
                "total of three squares: 29",
            ],
        ),
        (
            "examples/anagram/src/anagram.witchy",
            &["listen, silent, enlist", "cat, act, tac", "dog, god"],
        ),
        (
            "examples/stats/src/stats.witchy",
            &[
                "count    8",
                "mean     5.00",
                "variance 4.00",
                "stddev   2.00",
                "min      2.00",
                "max      9.00",
            ],
        ),
        (
            "examples/matrix/src/matrix.witchy",
            &[
                "A x B =",
                "[  58  64 ]",
                "[ 139 154 ]",
                "transpose(A) =",
                "[ 1 4 ]",
                "[ 2 5 ]",
                "[ 3 6 ]",
                "identity(3) =",
                "[ 1 0 0 ]",
                "[ 0 1 0 ]",
                "[ 0 0 1 ]",
            ],
        ),
        (
            "examples/toposort/src/toposort.witchy",
            &[
                "build order: boot -> config -> db -> cache -> api -> web",
                "cyclic:      error: cycle among egg, chicken",
            ],
        ),
        ("examples/list_ops/src/list_ops.witchy", &["55", "6", "0-2-4"]),
        ("examples/wordcount/src/wordcount.witchy", &["3", "1", "0", "4"]),
        ("examples/inventory/src/inventory.witchy", &["total = 9", "over 2: 2"]),
        (
            "examples/time_and_encoding/src/time_and_encoding.witchy",
            &[
                "date:    2026-05-28T20:26:40Z (Thursday)",
                "layout:  Thursday, May 28 2026 at 20:26",
                "parsed:  2026-06-08T20:30:00Z",
                "checked: day 30 is out of range for 2026-2",
                "base64:  d2l0Y2h5IPCfp5k=",
                "hex:     77697463687920f09fa799",
                "decoded: witchy 🧙",
            ],
        ),
        ("examples/closures/src/closures.witchy", &["81", "16", "105"]),
        (
            "examples/higher_order_sum/src/higher_order_sum.witchy",
            &["imperative: 5456", "functional: 5456"],
        ),
        ("examples/higher_order/src/higher_order.witchy", &["15", "81", "15", "120"]),
        (
            "examples/pascal/src/pascal.witchy",
            &["1", "1 1", "1 2 1", "1 3 3 1", "1 4 6 4 1", "1 5 10 10 5 1"],
        ),
        ("examples/dedup/src/dedup.witchy", &["1 2 3 2 4"]),
        (
            "examples/generators/src/generators.witchy",
            &[
                "fib[0..10): 0, 1, 1, 2, 3, 5, 8, 13, 21, 34",
                "collatz(6): 6, 3, 10, 5, 16, 8, 4, 2, 1",
                "collatz(27) length: 112",
            ],
        ),
        (
            "examples/lazy_fib/src/lazy_fib.witchy",
            &[
                "first 10: 0, 1, 1, 2, 3, 5, 8, 13, 21, 34",
                "even fib sum < 1000: 798",
                "first fib > 1000: 1597",
            ],
        ),
        (
            "examples/let_patterns/src/let_patterns.witchy",
            &["found: 42", "head is 7", "pop 1", "pop 2", "pop 3", "pop 4", "drained"],
        ),
        (
            "examples/tuples/src/tuples.witchy",
            &["3 remainder 2", "7 spells seven", "just the remainder: 2", "2 3"],
        ),
        ("examples/loops/src/loops.witchy", &["sum = 108", "witchy loops work"]),
        (
            "examples/listmatch/src/listmatch.witchy",
            &["sum = 21", "starts with 3", "one: 42", "empty"],
        ),
        (
            "examples/conventions/src/conventions.witchy",
            &[
                "count: 2",
                "sum: 10",
                "doubled first: 2",
                "nums still here, length: 4",
                "bag total: 60",
                "drained length: 3",
                "running sum: 300",
                "running sum: 306",
            ],
        ),
        (
            "examples/records/src/records.witchy",
            &["origin.x = 2", "moved = (12, 3)", "manhattan(moved) = 15"],
        ),
        ("examples/record_compiled/src/record_compiled.witchy", &["32"]),
        (
            "examples/record_update/src/record_update.witchy",
            &["alice 100", "alice 150", "alice smith 150"],
        ),
        (
            "examples/temperature/src/temperature.witchy",
            &[
                "0F = -17.8C",
                "60F = 15.6C",
                "120F = 48.9C",
                "180F = 82.2C",
                "240F = 115.6C",
                "300F = 148.9C",
            ],
        ),
        (
            "examples/largest/src/largest.witchy",
            &["largest number: 100", "latest version: 2.0"],
        ),
    ];

    for (path, expected) in cases {
        assert_eq!(
            crate::execute_file(path, Vec::new()).unwrap(),
            *expected,
            "golden output drifted for {path}"
        );
    }
}

#[test]
fn pure_examples_require_only_console() {
    for path in [
        "examples/calc/src/calc.witchy",
        "examples/sudoku/src/sudoku.witchy",
        "examples/life/src/life.witchy",
        "examples/time_and_encoding/src/time_and_encoding.witchy",
    ] {
        let src = std::fs::read_to_string(path).unwrap();
        let footprint = crate::capabilities::analyze(&parser::parse_module(&src).expect("parse"));
        assert_eq!(
            crate::capabilities::show_caps(&footprint.total),
            "Console",
            "capability footprint drifted for {path}"
        );
    }
}

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

#[test]
fn sudoku_example_solves_by_backtracking() {
    let out = crate::execute_file("examples/sudoku/src/sudoku.witchy", Vec::new())
        .unwrap()
        .join("\n");
    assert!(
        out.contains("solved:\n534678912\n672195348\n198342567\n859761423"),
        "unique solution: {out}"
    );
}

#[test]
fn life_example_evolves_a_glider() {
    let out = crate::execute_file("examples/life/src/life.witchy", Vec::new())
        .unwrap()
        .join("\n");
    assert!(
        out.contains("generation 0:\n.#......\n..#.....\n###....."),
        "seed glider: {out}"
    );
    assert!(
        out.contains("generation 3:\n........\n.#......\n..##....\n.##....."),
        "evolved glider: {out}"
    );
}

/// `main` may receive argv because input data is not authority.
#[test]
fn main_receives_command_line_args() {
    let run = |args: Vec<String>| -> Vec<String> {
        let src = "fn main(console: Console, args: List(String)):\n    console.print(list.join(args, \",\"))\n";
        let module = parser::parse_module(src).expect("parse");
        let linked =
            crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        interpreter::run_module_args(linked, ".", Vec::new(), args).expect("run")
    };
    assert_eq!(run(vec!["a".into(), "b".into(), "c".into()]), vec!["a,b,c"]);
    assert_eq!(run(Vec::new()), vec![""]);
}

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
        vec!["I'm nobody! Who are you?", "Are you nobody, too?"]
    );

    let (out, code) = crate::execute_file_exit(
        "examples/minigrep/src/minigrep.witchy",
        Vec::new(),
        Vec::new(),
        Vec::new(),
        None,
        Vec::new(),
    )
    .unwrap();
    assert_eq!(code, 1);
    assert_eq!(out, vec!["usage: minigrep <query> <file>"]);
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
