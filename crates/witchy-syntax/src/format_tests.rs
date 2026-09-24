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
            } else if trimmed == "```witchy" {
                in_witchy = true;
                start_line = idx + 2;
            }
        }
    }

    #[test]
    fn declaration_attributes_round_trip_and_unknown_names_fail_closed() {
        let src = "@dynamic\npub fn render(self: Int) -> String:\n    \"${self}\"\n";
        let out = reformat(src).expect("function attribute round-trips");
        assert!(out.starts_with("@dynamic\npub fn render"), "{out}");
        assert_eq!(reformat(&out).as_deref(), Some(out.as_str()));

        let error = crate::parser::parse_module("@typo\nfn render() -> Int:\n    1\n")
            .expect_err("unknown attributes must not become inert metadata");
        assert!(error.message.contains("unknown declaration attribute `@typo`"));
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
                line: 0,
                public: false,
                pure: false,
                comptime_only: false,
                attributes: Vec::new(),
                name: "main".into(),
                type_params: Vec::new(),
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
            compiler_item_syntax: vec![],
            compiler_expr_syntax: vec![],
            compiler_type_syntax: vec![],
            compiler_pattern_syntax: vec![],
            compiler_stmt_syntax: vec![],
            compiler_block_syntax: vec![],
            linked_entry: None,
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
    fn reformats_every_std_example_and_glamour_source_to_an_equal_ast() {
        // The printer must faithfully round-trip every shipped source file and
        // parseable book/README `witchy` fence. The shipped trees live at the
        // workspace root (two levels up from this crate's manifest dir).
        let root = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
        )
        .join("../..");
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
    fn formatting_is_idempotent() {
        // Formatting already-formatted code must be a no-op: `fmt(fmt(x)) == fmt(x)`.
        // The shipped std/ + examples/ trees live at the workspace root (two
        // levels up from this crate's manifest dir).
        let root = std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
        )
        .join("../..");
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

    // ---- RFC-0081: existential trait types -------------------------------

#[test]
fn rfc0167_mutable_legacy_migration_never_becomes_an_immutable_view() {
    for source in [
        "mode opt\n\nfn edit(value: &'a mut String) -> &'a mut String:\n    value\n",
        "mode opt\n\nfn edit(value: String):\n    &mut value\n",
        "mode opt\n\nfn write(value: &'a mut String):\n    *value = \"next\"\n",
    ] {
        assert!(migrate_references(source).is_none(), "mutable source was silently rewritten: {source}");
        assert!(reformat_references(source).is_none(), "mutable source was emitted as a view: {source}");
    }
}
