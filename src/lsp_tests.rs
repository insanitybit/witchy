    use super::*;

    fn diags(text: &str) -> Vec<Value> {
        compute_diagnostics("file:///tmp/main.witchy", text, &HashMap::new())
    }

    fn docs(uri: &str, text: &str) -> HashMap<String, String> {
        HashMap::from([(uri.to_string(), text.to_string())])
    }

    fn hover_at(
        docs: &HashMap<String, String>,
        uri: &str,
        text: &str,
        line: usize,
        name: &str,
    ) -> Value {
        let character = text.lines().nth(line).unwrap().find(name).unwrap() as u64;
        hover_response(
            docs,
            &json!({
                "textDocument": { "uri": uri },
                "position": { "line": line, "character": character },
            }),
        )
    }

    const GENERATED_ITEM_SOURCE: &str = r#"import meta

comptime fn build() -> ItemSyntax:
    quote item:
        pub fn generated() -> Int:
            7

comptime:
    emit_item(build())

fn main(console: Console):
    console.print("${generated()}")
"#;

    #[test]
    fn completion_includes_keywords_builtins_and_module_fns() {
        let src = "\nfn helper(n: Int) -> Int:\n    n\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let docs = docs("file:///t.witchy", src);
        let items = completion_response(
            &docs,
            &json!({ "textDocument": { "uri": "file:///t.witchy" } }),
        );
        let labels: Vec<String> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect();
        assert!(labels.contains(&"match".to_string()), "keywords offered");
        assert!(labels.contains(&"print".to_string()), "builtins offered");
        assert!(labels.contains(&"helper".to_string()), "document fns offered");
        assert!(labels.contains(&"show.render".to_string()), "prelude Show renderer offered");
        assert!(labels.contains(&"show.say".to_string()), "prelude Show printer offered");
        assert!(
            labels.iter().any(|l| l.starts_with("string.")),
            "imported module fns offered: {labels:?}"
        );
    }

    #[test]
    fn generated_definition_requires_an_admitted_tooling_session() {
        let uri = "file:///tmp/main.witchy";
        let docs = docs(uri, GENERATED_ITEM_SOURCE);
        let response = definition_response(
            &docs,
            &json!({
                "textDocument": { "uri": uri },
                "position": { "line": 7, "character": 0 },
            }),
        );
        assert!(response.is_null(), "{response}");
        let diagnostics = diags(GENERATED_ITEM_SOURCE);
        assert!(diagnostics.iter().any(|diagnostic|
            diagnostic["message"].as_str().is_some_and(|message|
                message.contains("requires authenticated prior expansion"))),
            "{diagnostics:?}");
    }

    #[test]
    fn hover_shows_signature_and_doc() {
        let src = "// Doubles a number.\n// Twice the input.\nfn double(n: Int) -> Int:\n    n * 2\n\nfn main(console: Console):\n    console.print(\"${double(3)}\")\n";
        let uri = "file:///t.witchy";
        let docs = docs(uri, src);
        let resp = hover_at(&docs, uri, src, 6, "double");
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(contents.contains("fn double(n: Int) -> Int"), "{contents}");
        assert!(contents.contains("Doubles a number."), "{contents}");
    }

    #[test]
    fn rfc0167_lsp_rejects_retired_syntax_instead_of_migrating_it() {
        for (source, spelling) in [
            ("fn old(value: View(String, 'a)):\n    Nil\n", "View"),
            ("fn old(value: let('a) String):\n    Nil\n", "let('a)"),
            ("fn old(let('a) value: String):\n    Nil\n", "let('a)"),
            ("fn old(var('a) value: String):\n    Nil\n", "var('a)"),
            ("fn old(value: local unique String):\n    Nil\n", "local unique"),
            ("comptime fn old() -> TypeSyntax:\n    quote type:\n        View(String, 'a)\n", "View"),
        ] {
            for prefix in ["", "mode opt\n\n"] {
                let diagnostics = diags(&format!("{prefix}{source}"));
                assert!(diagnostics.iter().any(|diagnostic| {
                    diagnostic["severity"] == json!(1)
                        && diagnostic["message"].as_str().unwrap().contains(spelling)
                }), "{source}: {diagnostics:?}");
            }
        }
    }

    #[test]
    fn word_at_is_utf8_boundary_safe() {
        // A line with a multibyte char before the identifier. LSP `character` is a
        // code-unit (char) offset; using it as a BYTE index slices off a UTF-8
        // boundary (panic) or returns the wrong word. Here `π` precedes `repeat`;
        // the char offset of `repeat` differs from its byte offset.
        let line = "    console.print(\"π\" + \"ab\".repeat(2))";
        let text = format!("fn main(console: Console):\n{line}\n");
        // char offset of the `r` in `repeat` (differs from its byte offset).
        let repeat_char_idx = "    console.print(\"π\" + \"ab\".".chars().count();
        let w = word_at(&text, 1, repeat_char_idx).expect("word found");
        assert_eq!(w, "repeat", "got {w:?}");
        // Hovering right on the multibyte char must not panic.
        let pi_idx = "    console.print(\"".chars().count();
        let _ = word_at(&text, 1, pi_idx); // must not panic
    }

    #[test]
    fn clean_program_has_no_diagnostics() {
        let src = r#"
fn main(console: Console):
    console.print("hi")
"#;
        assert_eq!(diags(src), Vec::<Value>::new());
    }

    #[test]
    fn sibling_cannot_replace_reserved_std_module() {
        let dir = std::env::temp_dir().join(format!("witchy-lsp-std-owner-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("show.witchy"), "pub fn fake() -> Int:\n    1\n").unwrap();
        let main = dir.join("main.witchy");
        let src = "import show\n\nfn main(console: Console):\n    console.print(\"${90000ms}\")\n";
        let uri = format!("file://{}", main.to_str().unwrap());
        let mut docs = HashMap::new();
        docs.insert(uri.clone(), src.to_string());

        let d = compute_diagnostics(&uri, src, &docs);
        assert_eq!(d.len(), 1, "{d:?}");
        let msg = d[0]["message"].as_str().unwrap();
        assert!(
            msg.contains("module `show` uses a reserved standard-library name"),
            "{msg}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_error_points_at_its_line() {
        // Missing closing brace / stray token on line 2 (1-based).
        let src = "fn main(console: Console) {\n  let x = \n}\n";
        let d = diags(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0]["severity"], json!(1));
        assert_eq!(d[0]["source"], json!("witchy"));
        // The error sits on a real line, reported 0-based.
        assert!(d[0]["range"]["start"]["line"].as_u64().is_some());
    }

    #[test]
    fn type_error_is_reported_on_the_offending_line() {
        // Adding a String to an Int is a type error; it should map to a single
        // diagnostic whose line was recovered from the checker's message.
        let src = r#"
fn main(console: Console):
    let x = (1 + "two")
    console.print("")
"#;
        let d = diags(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0]["severity"], json!(1));
        assert_eq!(d[0]["range"]["start"]["line"], json!(2));
        assert!(
            d[0]["message"].as_str().unwrap().contains("type error"),
            "{:?}",
            d[0]["message"]
        );
    }

    #[test]
    fn missing_on_disk_import_is_reported_gracefully() {
        // BUG-168: `import helper` with no helper.witchy anywhere must yield a
        // clear "cannot resolve import" at the import line, not a line-0
        // "link error: module main imports unknown module helper".
        let dir = std::env::temp_dir().join(format!("witchy-lsp-168-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.witchy");
        let src = "import helper\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let uri = format!("file://{}", main.to_str().unwrap());
        let mut docs = HashMap::new();
        docs.insert(uri.clone(), src.to_string());
        let d = compute_diagnostics(&uri, src, &docs);
        assert_eq!(d.len(), 1, "{d:?}");
        let msg = d[0]["message"].as_str().unwrap();
        assert!(msg.contains("cannot resolve import `helper`"), "{msg}");
        assert_eq!(d[0]["range"]["start"]["line"], json!(0), "{d:?}");
        assert!(!msg.contains("link error"), "{msg}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn link_error_maps_to_real_location() {
        // BUG-162: a link error carrying `line N` underlines that line; one that
        // carries a structured source line underlines it; an imported module
        // falls back to its import; otherwise line 0.
        let text = "import helper\nimport other\nfn main(console: Console):\n    console.print(\"x\")\n    console.print(helper.not_real())\n";
        let error = |message: &str, location: Option<(&str, u32)>| witchy_syntax::linker::LinkError {
            message: message.to_string(),
            location: location.map(|(module, line)| witchy_syntax::linker::LinkLocation {
                module: module.to_string(),
                line,
            }),
        };
        assert_eq!(
            link_error_line(&error("boom at line 3: bad thing", None), text, "main"),
            2
        );
        assert_eq!(
            link_error_line(
                &error(
                    "module `helper` has no function `not_real`",
                    Some(("main", 5)),
                ),
                text,
                "main",
            ),
            4
        );
        assert_eq!(
            link_error_line(
                &error("module `main` imports unknown module `other`", None),
                text,
                "main",
            ),
            1
        );
        assert_eq!(
            link_error_line(
                &error("module `helper` has no function `not_real`", Some(("helper", 8))),
                text,
                "main",
            ),
            0
        );
        assert_eq!(
            link_error_line(&error("something went wrong", None), text, "main"),
            0
        );
    }

    #[test]
    fn uri_to_path_survives_percent_before_multibyte_char() {
        // A `%` NOT followed by two hex digits — here by a multi-byte UTF-8 char
        // (`€` is 3 bytes) — used to panic: the decoder sliced `&s[i+1..i+3]`,
        // landing mid-codepoint. It must now pass the `%` through untouched.
        assert_eq!(
            uri_to_path("file:///Users/x/100%€/a.witchy"),
            Some(PathBuf::from("/Users/x/100%€/a.witchy"))
        );
        // A bare trailing `%` and a `%` before a non-hex char must also not panic.
        assert_eq!(
            uri_to_path("file:///x/%"),
            Some(PathBuf::from("/x/%"))
        );
        assert_eq!(
            uri_to_path("file:///x/%zz"),
            Some(PathBuf::from("/x/%zz"))
        );
    }

    #[test]
    fn dot_completion_on_capability_offers_capability_methods() {
        let src = "fn main(console: Console):\n    console.\n";
        let uri = "file:///test.witchy";
        let docs = docs(uri, src);
        let items = completion_response(
            &docs,
            &json!({
                "textDocument": { "uri": uri },
                "position": { "line": 1, "character": 12 },
            }),
        );
        let arr = items.as_array().expect("items array");
        let labels: Vec<&str> = arr.iter().filter_map(|i| i["label"].as_str()).collect();

        assert!(labels.contains(&"print"), "console.print offered: {labels:?}");
        assert!(labels.contains(&"input"), "console.input offered: {labels:?}");

        let print_item = arr.iter().find(|i| i["label"] == "print").expect("print item");
        assert_eq!(print_item["kind"], 2); // Method
        let detail = print_item["detail"].as_str().expect("print detail");
        assert!(detail.contains("String"), "{detail}");
    }

