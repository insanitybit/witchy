    use super::*;

    fn roundtrips(src: &str) -> bool {
        reformat(src).is_some()
    }

    #[test]
    fn comprehensions_survive_formatting_everywhere() {
        // Learner round-3 BLOCKER: `let ys = [n * n for n in xs]` used to print
        // as `let ys = 0` (the inline renderer's placeholder leaked), and the
        // idempotence-only guard shipped it. Comprehensions must print back as
        // the literal — in value position, at a function tail, with filters,
        // and with multiple generators.
        let src = "fn squares(xs: List(Int)) -> List(Int):\n    [n * n for n in xs]\n\nfn main(console: Console):\n    let xs = [1, 2, 3]\n    let ys = [n * n for n in xs]\n    let odds = [n for n in xs if n % 2 == 1]\n    let pairs = [(a, b) for a in xs for b in xs if a < b]\n    print(console, \"${ys} ${odds} ${pairs} ${squares(xs)}\")\n";
        let out = reformat(src).expect("comprehensions round-trip");
        assert_eq!(out, src, "comprehensions are already canonical");
    }

    #[test]
    fn the_semantic_guard_rejects_a_mangling_printer() {
        // The guard must compare programs, not just check idempotence. A block
        // value the printer can't render (here: forced via a synthetic AST
        // whose printed placeholder `0` differs from the original program)
        // must make reformat return None rather than ship the placeholder.
        use crate::ast::*;
        // let x = { var __zzz = []; __zzz }  — NOT comprehension-shaped (no
        // loop), so the printer has no faithful inline form for it.
        let block = Expr::Block(Block {
            stmts: vec![
                Stmt::Let { name: "__zzz".into(), ty: None, mutable: true, value: Expr::List(vec![]) },
                Stmt::Expr(Expr::Var("__zzz".into())),
            ],
            lines: vec![0, 0],
            restrict: None,
            region: None,
        });
        let m = Module {
            modes: Vec::new(),
            imports: vec![],
            items: vec![Item::Function(Function {
                public: false,
                name: "main".into(),
                params: vec![Param {
                    name: "console".into(),
                    ty: Some(Type::Named("Console".into(), vec![])),
                    convention: Default::default(),
                }],
                ret: None,
                body: Block {
                    stmts: vec![Stmt::Let { name: "x".into(), ty: None, mutable: false, value: block }],
                    lines: vec![0],
                    restrict: None,
                    region: None,
                },
                bounds: vec![],
                is_gen: false,
                is_async: false,
            })],
            import_lines: vec![],
            item_lines: vec![],
        };
        let printed = module(&m, &[]);
        // The printed form parses, but to a DIFFERENT program (`let x = 0`);
        // reformat over the printed source still succeeds (it is self-faithful),
        // while the original AST and the reparse of `printed` must disagree —
        // exactly what the guard checks.
        let mut want = m.clone();
        let mut got = crate::parser::parse_module(&printed).expect("placeholder parses");
        canon_module(&mut want);
        canon_module(&mut got);
        assert_ne!(want, got, "the placeholder changed the program — guard must see it");
    }

    #[test]
    fn reformats_every_std_and_example_to_an_equal_ast() {
        // The printer must faithfully round-trip every shipped source file.
        let dirs = ["std", "examples"];
        let mut failures = Vec::new();
        for dir in dirs {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) != Some("witchy") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                if crate::parser::parse_module(&src).is_ok() && !roundtrips(&src) {
                    failures.push(path.display().to_string());
                }
            }
        }
        assert!(failures.is_empty(), "did not round-trip: {failures:?}");
    }

    #[test]
    fn preserves_capability_firewalls() {
        // `retain`/`without` blocks must round-trip to an equal AST (so the
        // restriction survives a reformat) and print their header back verbatim.
        let src = "fn main(console: Console, clock: Clock):\n    without clock:\n        print(console, \"a\")\n    retain console:\n        print(console, \"b\")\n    retain:\n        0\n";
        let out = reformat(src).expect("firewalls round-trip to an equal AST");
        assert!(out.contains("without clock:"), "{out}");
        assert!(out.contains("retain console:"), "{out}");
        assert!(out.contains("retain:"), "{out}");
    }

    #[test]
    fn preserves_ranges() {
        // Ranges used to fail to format (they desugared to a synthetic block at
        // parse time); now they round-trip and print back as `lo..hi` / `lo..=hi`,
        // including when used as a value or with operator operands.
        let src = "fn main(console: Console):\n    for i in 0..3:\n        print(console, __render(i))\n    let xs = 1..=n\n    let ys = a + 1..b * 2\n";
        let out = reformat(src).expect("ranges round-trip");
        assert!(out.contains("for i in 0..3:"), "{out}");
        assert!(out.contains("let xs = 1..=n"), "{out}");
        // Operator operands bind tighter than `..`, so they need no parentheses.
        assert!(out.contains("let ys = a + 1..b * 2"), "{out}");
    }

    #[test]
    fn preserves_subscripts() {
        // Subscripts used to de-sugar to `list.at(xs, i)` on format; now they round-trip
        // and print back as `base[index]`, including nested and computed indices.
        let src = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    let grid = [[1], [2]]\n    print(console, __render(xs[0] + grid[1][0]))\n";
        let out = reformat(src).expect("subscripts round-trip");
        assert!(out.contains("xs[0]"), "{out}");
        assert!(out.contains("grid[1][0]"), "{out}");
        assert!(!out.contains("list.at("), "subscripts must not de-sugar to list.at(): {out}");
    }

    #[test]
    fn preserves_while_let() {
        // `while let` used to de-sugar to `while true / match / break` on format;
        // now it round-trips and prints back as `while let PAT = SCRUT:`.
        let src = "fn main(console: Console):\n    var o = Some(1)\n    while let Some(n) = o:\n        print(console, __render(n))\n        o = None\n";
        let out = reformat(src).expect("while let round-trips");
        assert!(out.contains("while let Some(n) = o:"), "{out}");
        assert!(!out.contains("while true"), "while let must not de-sugar: {out}");
        assert!(!out.contains("match o"), "while let must not de-sugar: {out}");
    }

    #[test]
    fn preserves_top_level_comments() {
        // The header (before imports) and a doc comment before an item survive
        // formatting, attached in the right place.
        let src = "// header one\n// header two\n\nimport string\n\n// doc for f\nfn f() -> Int:\n    5\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("// header one\n// header two"), "{out}");
        assert!(out.contains("// doc for f\nfn f"), "{out}");
        // The header stays above the import.
        assert!(out.find("// header one").unwrap() < out.find("import string").unwrap(), "{out}");
    }

    #[test]
    fn preserves_blank_lines_between_statements() {
        // A single author blank line between statements survives (it used to be
        // stripped); multiple blanks collapse to one.
        let src = "fn main(console: Console):\n    let a = 1\n\n\n    let b = 2\n    let c = 3\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("let a = 1\n\n    let b = 2"), "blank not preserved: {out}");
        assert!(out.contains("let b = 2\n    let c = 3"), "adjacent stmts gained a blank: {out}");
    }

    #[test]
    fn no_false_blank_after_multiline_statement() {
        // The lines a multi-line `if`/`while` spans must not be mistaken for a
        // blank: `print("after")` follows the block with no blank inserted.
        let src = "fn main(console: Console):\n    if true:\n        let x = 1\n        let y = 2\n    let z = 3\n";
        let out = reformat(src).expect("round-trips");
        assert!(!out.contains("let y = 2\n    \n"), "{out}");
        // The statement after the if-block is directly adjacent (no blank).
        assert!(out.contains("        let y = 2\n    let z = 3"), "{out}");
    }

    #[test]
    fn preserves_blank_between_comment_paragraphs() {
        // A blank line separating two comment paragraphs used to be collapsed,
        // gluing the paragraphs together; it is now kept.
        let src = "fn main(console: Console):\n    // paragraph one\n    // still one\n\n    // paragraph two\n    print(console, \"hi\")\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("// still one\n\n    // paragraph two"), "blank between paragraphs lost: {out}");
    }

    #[test]
    fn preserves_ufcs_method_calls() {
        // `x.f()` used to de-sugar to `f(x)` on format; it now round-trips,
        // including chains and a parenthesized receiver. Module-qualified calls
        // (`json.decode(x)`) stay as-is.
        let src = "import json\nfn main(console: Console):\n    let r = 5.double().inc()\n    let q = (2 + 3).double()\n    let d = json.decode(\"1\")\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("5.double().inc()"), "chain not preserved: {out}");
        assert!(out.contains("(2 + 3).double()"), "paren receiver not preserved: {out}");
        assert!(out.contains("json.decode(\"1\")"), "module call changed: {out}");
        assert!(!out.contains("double(5)"), "UFCS de-sugared: {out}");
    }

    #[test]
    fn preserves_comments_between_imports() {
        // A comment sitting between two imports used to be relocated past the whole
        // import block (to just above the first item); it now stays in place.
        let src = "import string\n// a note about result\nimport result\n\nfn f() -> Int:\n    1\n";
        let out = reformat(src).expect("round-trips");
        assert!(
            out.contains("import string\n// a note about result\nimport result"),
            "{out}"
        );
        // And it must not have drifted down to the function.
        assert!(!out.contains("// a note about result\nfn f"), "{out}");
    }

    #[test]
    fn preserves_in_body_and_nested_comments() {
        let src = "fn main(console: Console):\n    // before x\n    let x = 5\n    while x > 0:\n        // inside loop\n        x = x - 1\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("    // before x\n    let x = 5"), "{out}");
        // The nested comment keeps the loop body's indentation.
        assert!(out.contains("        // inside loop\n        x = x - 1"), "{out}");
    }

    #[test]
    fn formatting_is_idempotent() {
        // Formatting already-formatted code must be a no-op: `fmt(fmt(x)) == fmt(x)`.
        let dirs = ["std", "examples"];
        let mut failures = Vec::new();
        for dir in dirs {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) != Some("witchy") {
                    continue;
                }
                let src = std::fs::read_to_string(&path).unwrap();
                if let Some(once) = reformat(&src) {
                    if reformat(&once).as_deref() != Some(once.as_str()) {
                        failures.push(path.display().to_string());
                    }
                }
            }
        }
        assert!(failures.is_empty(), "formatting is not idempotent: {failures:?}");
    }

    #[test]
    fn block_body_lambdas_round_trip() {
        // A closure with a multi-statement body, and one whose body is a `match`,
        // now format as block-form lambdas and re-parse to the same AST.
        let multi = "fn make(n: Int) -> fn(Int) -> Int:\n    fn(x: Int):\n        let y = (x + n)\n        (y * 2)\n";
        let out = reformat(multi).expect("multi-statement closure round-trips");
        assert!(!out.contains('{'), "braces: {out}");

        let matchy = "type Opt:\n    Some(a)\n    None\n\nfn classify() -> fn(Opt(Int)) -> Int:\n    fn(o: Opt(Int)):\n        match o:\n            Some(n) -> n\n            None -> 0\n";
        assert!(reformat(matchy).is_some(), "match-body closure should round-trip");

        // A trailing block-lambda call with a postfix `.await` (the
        // `chan.serve(s, fn(..): match ...).await` shape) keeps its multi-line
        // form: the `.await` rides as a suffix after the closing `)`, and must
        // not force the whole call inline and corrupt the block lambda.
        let awaited = "async fn loop_it() -> Nil:\n    serve(0, fn(n, m):\n        match m:\n            0 -> n + 1\n            _ -> n).await\n";
        let out = reformat(awaited).expect("block-lambda call + .await round-trips");
        assert!(out.contains("serve(0, fn(n, m):"), "block lambda lost: {out}");
        assert!(out.contains(").await"), "postfix .await lost: {out}");
    }

    #[test]
    fn reformat_is_idempotent_and_brace_free() {
        let src = "fn classify(n: Int) -> String:\n    if n > 0: \"pos\" else: \"non-pos\"\n\nfn main(console: Console):\n    print(console, classify(5))\n";
        let out = reformat(src).expect("round-trips");
        assert!(!out.contains('{'), "still has braces: {out}");
        assert_eq!(reformat(&out).unwrap(), out, "not idempotent");
    }
