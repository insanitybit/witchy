    use super::*;

    fn roundtrips(src: &str) -> bool {
        let Some(out) = reformat(src) else {
            return false;
        };
        crate::lexer::own_line_comments(src)
            .into_iter()
            .all(|(_, _, text)| out.contains(text.trim()))
    }

    fn collect_witchy_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_witchy_files(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("witchy") {
                out.push(path);
            }
        }
    }

    fn collect_witchy_fences(path: &std::path::Path, out: &mut Vec<(String, String)>) {
        let text = std::fs::read_to_string(path).unwrap();
        let mut in_witchy = false;
        let mut start_line = 0usize;
        let mut body = String::new();
        for (idx, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if in_witchy {
                if trimmed.starts_with("```") {
                    out.push((format!("{}:{}", path.display(), start_line), std::mem::take(&mut body)));
                    in_witchy = false;
                } else {
                    body.push_str(line);
                    body.push('\n');
                }
            } else if trimmed.starts_with("```witchy") {
                in_witchy = true;
                start_line = idx + 2;
            }
        }
    }

    #[test]
    fn packed_qualifier_survives_formatting() {
        // (RFC-0027) the `packed` modifier must round-trip through `witchy fmt`, not
        // be silently dropped (which would un-pack the type).
        let src = "type Point packed:\n    x: Int\n    y: Int\n\nfn main(console: Console):\n    print(console, \"${Point(1, 2).x}\")\n";
        let out = reformat(src).expect("packed type round-trips");
        assert!(out.contains("type Point packed:"), "packed must survive formatting: {out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn grantable_capability_survives_formatting() {
        // (RFC-0038) the `grantable` marker must round-trip through `witchy fmt`,
        // not be silently dropped (which would un-grant the capability).
        let src = "grantable capability UiRoot:\n    policy: String\n";
        let out = reformat(src).expect("grantable capability round-trips");
        assert!(out.contains("grantable capability UiRoot:"), "grantable must survive: {out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn capability_rights_use_brackets_when_formatting() {
        let src = "fn main(console: Console, dir: Dir[Read], file: File[Read], net: Net[Connect]):\n    print(console, read(file))\n";
        let out = reformat(src).expect("capability rights round-trip");
        assert!(out.contains("dir: Dir[Read]"), "{out}");
        assert!(out.contains("file: File[Read]"), "{out}");
        assert!(out.contains("net: Net[Connect]"), "{out}");
        assert!(!out.contains("File(Read)"), "{out}");
    }

    #[test]
    fn comptime_functions_round_trip_through_formatting() {
        let src = "comptime fn make() -> ItemSyntax:\n    item(\"fn generated() -> Int:\\n    1\")\n\ncomptime:\n    emit_item(make())\n";
        let out = reformat(src).expect("comptime fn round-trips");
        assert!(out.contains("comptime fn make() -> ItemSyntax:"), "{out}");
        assert!(out.contains("emit_item(make())"), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn anonymous_record_types_round_trip_through_formatting() {
        // RFC-0078 makes the structural record tier denotable in type position.
        // Formatting must print the source-level shape, not the synthetic
        // compiler-private `__anon...` record name.
        let src = "type Point = .{x: Int, y: Int}\n\nfn label(p: .{y: Int, x: Int}) -> String:\n    \"${p.x},${p.y}\"\n";
        let out = reformat(src).expect("anonymous record type round-trips");
        assert!(out.contains("type Point = .{x: Int, y: Int}"), "{out}");
        assert!(out.contains("p: .{x: Int, y: Int}"), "{out}");
        assert!(!out.contains("__anon"), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn anonymous_record_spread_round_trips_through_formatting() {
        // RFC-0078 gives anonymous records the same update/spread surface as named
        // records. Formatting should keep the structural spelling, not leak the
        // parser's placeholder synthetic name.
        let src = "fn bump(p: .{x: Int, y: Int}) -> .{x: Int, y: Int}:\n    .{y: p.y + 1, ..p}\n";
        let out = reformat(src).expect("anonymous record spread round-trips");
        assert!(out.contains(".{y: p.y + 1, ..p}"), "{out}");
        assert!(!out.contains("__anon"), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn anonymous_union_types_round_trip_through_formatting() {
        // RFC-0078 anonymous union types are canonical sets: formatting sorts the
        // tag spelling and keeps payload types attached to their tag.
        let src = "type LoadErr = .[Missing(String) | NotFound | BadPort(Int)]\n\nfn load() -> Result(Int, .[NotFound | BadPort(Int)]):\n    Ok(1)\n";
        let out = reformat(src).expect("anonymous union type round-trips");
        assert!(out.contains("type LoadErr = .[BadPort(Int) | Missing(String) | NotFound]"), "{out}");
        assert!(out.contains("Result(Int, .[BadPort(Int) | NotFound])"), "{out}");
        assert!(!out.contains("__union"), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn anonymous_union_injections_round_trip_through_formatting() {
        let src = "type LoadErr = .[BadPort(Int) | NotFound]\n\nfn bad() -> LoadErr:\n    .BadPort(70000)\n\nfn missing() -> LoadErr:\n    .NotFound\n";
        let out = reformat(src).expect("anonymous union injections round-trip");
        assert!(out.contains(".BadPort(70000)"), "{out}");
        assert!(out.contains(".NotFound"), "{out}");
        assert!(!out.contains("__union"), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn anonymous_union_patterns_round_trip_through_formatting() {
        let src = "type LoadErr = .[BadPort(Int) | Missing(String) | NotFound]\n\nfn describe(e: LoadErr) -> String:\n    match e:\n        .BadPort(p) -> \"bad:${p}\"\n        .Missing(k) -> k\n        .NotFound -> \"missing\"\n";
        let out = reformat(src).expect("anonymous union patterns round-trip");
        assert!(out.contains(".BadPort(p) ->"), "{out}");
        assert!(out.contains(".Missing(k) ->"), "{out}");
        assert!(out.contains(".NotFound ->"), "{out}");
        assert!(!out.contains("__union"), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn tagged_literals_survive_layout_changes() {
        // Tagged literals carry source positions for diagnostics. Formatting
        // can move the literal without changing its tag, parts, or hole source,
        // so the semantic guard must ignore those positions like other layout
        // metadata while still comparing the semantic fields.
        let src = "fn view(x: String) -> String:\n\n\n    html\"<p>${x}</p>\"\n";
        let out = reformat(src).expect("tagged literal round-trips after moving lines");
        assert!(out.contains("    html\"<p>${x}</p>\""), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
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
            region: None,
        });
        let m = Module {
            modes: Vec::new(),
            imports: vec![],
            from_imports: vec![],
            items: vec![Item::Function(Function {
                public: false,
                comptime_only: false,
                name: "main".into(),
                params: vec![Param {
                    name: "console".into(),
                    ty: Some(Type::Named("Console".into(), vec![])),
                    convention: Default::default(),
                    default: None,
                }],
                ret: None,
                body: Block {
                    stmts: vec![Stmt::Let { name: "x".into(), ty: None, mutable: false, value: block }],
                    lines: vec![0],
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
    fn single_expression_block_renders_its_value() {
        // Tagged-literal expansion stamps hole source locations by wrapping the
        // original expression in a one-expression block. That wrapper has no
        // semantic surface and must not leak as the inline placeholder.
        use crate::ast::*;
        let block = Expr::Block(Block {
            stmts: vec![Stmt::Expr(Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Int(41)),
                rhs: Box::new(Expr::Int(1)),
            })],
            lines: vec![7],
            region: None,
        });
        assert_eq!(expr_str(&block), "41 + 1");
    }

    #[test]
    fn reformats_every_std_example_and_glamour_source_to_an_equal_ast() {
        // The printer must faithfully round-trip every shipped source file and
        // parseable book/README `witchy` fence. The shipped trees live at the
        // workspace root (two levels up from this crate's manifest dir).
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dirs = ["std", "examples", "projects/glamour"];
        let mut failures = Vec::new();
        for dir in dirs {
            let mut files = Vec::new();
            collect_witchy_files(&root.join(dir), &mut files);
            for path in files {
                let src = std::fs::read_to_string(&path).unwrap();
                if crate::parser::parse_module(&src).is_ok() && !roundtrips(&src) {
                    failures.push(path.display().to_string());
                }
            }
        }
        let mut fences = Vec::new();
        collect_witchy_fences(&root.join("README.md"), &mut fences);
        let book = root.join("book/src");
        for entry in std::fs::read_dir(book).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                collect_witchy_fences(&path, &mut fences);
            }
        }
        for (label, src) in fences {
            if crate::parser::parse_module(&src).is_ok() && !roundtrips(&src) {
                failures.push(label);
            }
        }
        assert!(failures.is_empty(), "did not round-trip: {failures:?}");
    }

    #[test]
    fn preserves_ranges() {
        // Ranges used to fail to format (they desugared to a synthetic block at
        // parse time); now they round-trip and print back as `lo..hi` / `lo..=hi`,
        // including when used as a value or with operator operands.
        let src = "fn main(console: Console):\n    for i in 0..3:\n        print(console, \"${i}\")\n    let xs = 1..=n\n    let ys = a + 1..b * 2\n";
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
        let src = "fn main(console: Console):\n    let xs = [1, 2, 3]\n    let grid = [[1], [2]]\n    print(console, \"${xs[0] + grid[1][0]}\")\n";
        let out = reformat(src).expect("subscripts round-trip");
        assert!(out.contains("xs[0]"), "{out}");
        assert!(out.contains("grid[1][0]"), "{out}");
        assert!(!out.contains("list.at("), "subscripts must not de-sugar to list.at(): {out}");
    }

    #[test]
    fn preserves_while_let() {
        // `while let` used to de-sugar to `while true / match / break` on format;
        // now it round-trips and prints back as `while let PAT = SCRUT:`.
        let src = "fn main(console: Console):\n    var o = Some(1)\n    while let Some(n) = o:\n        print(console, \"${n}\")\n        o = None\n";
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
    fn preserves_impl_method_doc_comments() {
        // Method doc comments sit at the method header's indentation. They must
        // not be pulled into the previous or current method body.
        let src = "impl List(a):\n    // Maps elements.\n    pub fn map(self, f: fn(a) -> b) -> List(b):\n        var out = []\n        out\n\n    // Counts elements.\n    pub fn count(self) -> Int:\n        0\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("    // Maps elements.\n    pub fn map"), "{out}");
        assert!(out.contains("    // Counts elements.\n    pub fn count"), "{out}");
        assert!(!out.contains("pub fn map(self, f: fn(a) -> b) -> List(b):\n        // Maps elements."), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "formatting is idempotent");
    }

    #[test]
    fn preserves_trailing_and_inline_comments() {
        // BUG-331: trailing line comments and inline block comments used to be
        // silently deleted because the formatter only re-emits own-line comments.
        let trailing = "fn main(console: Console):\n    let x = 1 // keep me\n    print(console, \"${x}\")\n";
        let out = reformat(trailing).expect("trailing comments round-trip");
        assert!(out.contains("let x = 1 // keep me"), "{out}");

        let inline = "fn main(console: Console):\n    let x = 1 /* keep me */ + 2\n    print(console, \"${x}\")\n";
        let out = reformat(inline).expect("inline block comments round-trip");
        assert!(out.contains("let x = 1 + 2 /* keep me */"), "{out}");

        let own_line = "fn main(console: Console):\n    let x = 1\n    /* keep me */\n    print(console, \"${x}\")\n";
        let out = reformat(own_line).expect("own-line block comments still round-trip");
        assert!(out.contains("/* keep me */"), "{out}");
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
    fn preserves_comments_inside_type_and_match_bodies() {
        // BUG-332: body comments used to flush all at the top of the type/match,
        // so comments above later variants, fields, or arms attached to the first.
        let src = "type Shape:\n    Square\n    // circle docs\n    Circle\n\ntype Point:\n    x: Int\n    // y docs\n    y: Int\n\nfn pick(s: Shape) -> Int:\n    match s:\n        Square -> 1\n        // circle arm\n        Circle -> 2\n";
        let out = reformat(src).expect("round-trips");
        assert!(out.contains("Square\n    // circle docs\n    Circle"), "{out}");
        assert!(out.contains("x: Int\n    // y docs\n    y: Int"), "{out}");
        assert!(out.contains("Square -> 1\n        // circle arm\n        Circle -> 2"), "{out}");
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
        // The shipped std/ + examples/ trees live at the workspace root (two
        // levels up from this crate's manifest dir).
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dirs = ["std", "examples"];
        let mut failures = Vec::new();
        for dir in dirs {
            for entry in std::fs::read_dir(root.join(dir)).unwrap() {
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

    #[test]
    fn call_to_shadowing_param_keeps_its_target() {
        // BUG-014 corruption guard: `update` here is a PARAMETER (a callback),
        // not the retired `dict.update` builtin. `witchy fmt` must print the
        // call target verbatim — rewriting `update(x)` to `dict.update(x)`
        // would silently change which function the call resolves to (and, as it
        // happens, type-error, since `dict.update` is 4-arg). The name-blind
        // guard inside `reformat` cannot catch this on its own (it applied the
        // same rewrite to both sides), so assert the OUTPUT stays unqualified.
        let src = "fn f(update: fn(Int) -> Int, x: Int) -> Int:\n    update(x)\n";
        let out = reformat(src).expect("shadowing param round-trips");
        assert!(out.contains("update(x)"), "call target must stay `update`: {out}");
        assert!(!out.contains("dict.update"), "a shadowing param must not be qualified: {out}");
        assert_eq!(out, src, "already canonical — fmt must be a no-op here");
    }

    #[test]
    fn cap_method_migration_rewrites_bare_cap_ops() {
        let src = "fn main(console: Console, dir: Dir):\n    print(console, read(dir.read_file(\"note.txt\")))\n";
        let out = reformat_cap_methods(src).expect("migration formatter should converge");
        assert!(out.contains("console.print(dir.read_file(\"note.txt\").read())"), "{out}");
        assert_eq!(
            reformat_cap_methods(&out).as_deref(),
            Some(out.as_str()),
            "migration formatter must be idempotent"
        );
    }

    #[test]
    fn cap_method_migration_respects_local_shadowing_function() {
        let src = "fn read(x: Int) -> Int:\n    x + 1\n\nfn main(console: Console):\n    print(console, \"${read(1)}\")\n";
        let out = reformat_cap_methods(src).expect("migration formatter should converge");
        assert!(out.contains("console.print(\"${read(1)}\")"), "{out}");
        assert!(out.contains("read(1)"), "local function call must stay bare: {out}");
        assert!(!out.contains("1.read()"), "local function call was rewritten: {out}");
    }

    #[test]
    fn preserves_default_parameter() {
        // BUG-206: `fmt` used to drop `= <const>` on a defaulted parameter, so
        // the round-trip guard rejected any file with one. It now renders back.
        let src = "fn add(a: Int, b: Int = 2) -> Int:\n    a + b\n";
        let out = reformat(src).expect("default parameter round-trips");
        assert!(out.contains("b: Int = 2"), "default dropped: {out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "not idempotent: {out}");
    }

    #[test]
    fn preserves_index_place_assignment() {
        // BUG-333: `xs[i] = v` used to print as the desugared `xs = xs.set_at(...)`.
        // It now round-trips to the RFC-0022 canonical form, compound included.
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    xs[0] = 9\n    xs[1] += 5\n    var d = dict.new()\n    d[\"k\"] = 1\n    print(console, \"${xs.at(0)}\")\n";
        let out = reformat(src).expect("index place-assign round-trips");
        assert!(out.contains("    xs[0] = 9\n"), "index assign de-sugared: {out}");
        assert!(out.contains("    xs[1] += 5\n"), "compound index assign de-sugared: {out}");
        assert!(out.contains("    d[\"k\"] = 1\n"), "dict place-assign de-sugared: {out}");
        assert!(!out.contains("set_at"), "set_at leaked into output: {out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "not idempotent: {out}");
    }

    #[test]
    fn preserves_field_place_assignment() {
        // BUG-330: `p.f = v` desugars to a `RecordUpdate`, which `fmt` used to
        // render as the non-parseable `update p: f = v`, rejecting the whole file.
        let src = "type P:\n    x: Int\n\nfn main(console: Console):\n    var p = P(x: 1)\n    p.x = 5\n    p.x += 4\n    print(console, \"${p.x}\")\n";
        let out = reformat(src).expect("field place-assign round-trips");
        assert!(out.contains("    p.x = 5\n"), "field assign de-sugared: {out}");
        assert!(out.contains("    p.x += 4\n"), "compound field assign de-sugared: {out}");
        assert!(!out.contains("update p"), "RecordUpdate leaked into output: {out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()), "not idempotent: {out}");
    }

    #[test]
    fn preserves_for_var_loop() {
        // BUG-334: `for var x in xs:` used to de-sugar into a `for __fvN in ...`
        // indexed loop, leaking the internal counter into formatted source.
        let src = "fn main(console: Console):\n    var xs = [1, 2, 3]\n    for var x in xs:\n        x = x * 10\n    print(console, \"${xs.at(1)}\")\n";
        let out = reformat(src).expect("for var round-trips");
        assert!(out.contains("    for var x in xs:\n"), "for var de-sugared: {out}");
        assert!(!out.contains("__fv"), "synthetic counter leaked into output: {out}");
        assert_eq!(out, src, "for var must format to itself: {out}");
    }
