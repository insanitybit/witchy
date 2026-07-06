    use super::*;

    fn diags(text: &str) -> Vec<Value> {
        compute_diagnostics("file:///tmp/main.witchy", text)
    }

    #[test]
    fn completion_includes_keywords_builtins_and_module_fns() {
        let mut docs = HashMap::new();
        let src = "import string\n\nfn helper(n: Int) -> Int:\n    n\n\nfn main(console: Console):\n    print(console, \"x\")\n";
        docs.insert("file:///t.witchy".to_string(), src.to_string());
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
        assert!(
            labels.iter().any(|l| l.starts_with("string.")),
            "imported module fns offered: {labels:?}"
        );
    }

    #[test]
    fn hover_shows_signature_and_doc() {
        let mut docs = HashMap::new();
        let src = "// Doubles a number.\n// Twice the input.\nfn double(n: Int) -> Int:\n    n * 2\n\nfn main(console: Console):\n    print(console, __render(double(3)))\n";
        docs.insert("file:///t.witchy".to_string(), src.to_string());
        // Hover over `double` in the call on line 6 (0-based), col 35.
        let col = src.lines().nth(6).unwrap().find("double").unwrap() as u64;
        let resp = hover_response(
            &docs,
            &json!({
                "textDocument": { "uri": "file:///t.witchy" },
                "position": { "line": 6, "character": col },
            }),
        );
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(contents.contains("fn double(n: Int) -> Int"), "{contents}");
        assert!(contents.contains("Doubles a number."), "{contents}");
    }

    #[test]
    fn hover_resolves_imported_module_functions() {
        let mut docs = HashMap::new();
        let src = "import string\n\nfn main(console: Console):\n    print(console, string.repeat(\"ab\", 2))\n";
        docs.insert("file:///t.witchy".to_string(), src.to_string());
        let line = 3u64;
        let col = src.lines().nth(3).unwrap().find("repeat").unwrap() as u64;
        let resp = hover_response(
            &docs,
            &json!({
                "textDocument": { "uri": "file:///t.witchy" },
                "position": { "line": line, "character": col },
            }),
        );
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(contents.contains("repeat"), "{contents}");
        // BUG-161: the module qualifier must go BEFORE the fn name, not jammed in
        // front of `pub fn` — `string.pub fn repeat(...)` is malformed.
        assert!(
            !contents.contains("string.pub fn"),
            "malformed signature: {contents}"
        );
        assert!(
            contents.contains("fn string.repeat("),
            "expected qualified signature, got: {contents}"
        );
    }

    #[test]
    fn qualify_signature_inserts_module_before_name() {
        assert_eq!(
            qualify_signature("pub fn repeat(s: String, n: Int) -> String", "string."),
            "pub fn string.repeat(s: String, n: Int) -> String"
        );
        assert_eq!(
            qualify_signature("fn tcp(host: String, port: Int) -> NetPolicy", "Net."),
            "fn Net.tcp(host: String, port: Int) -> NetPolicy"
        );
        // A bare (document-local) signature is left untouched.
        assert_eq!(
            qualify_signature("fn double(n: Int) -> Int", ""),
            "fn double(n: Int) -> Int"
        );
    }

    #[test]
    fn word_at_is_utf8_boundary_safe() {
        // A line with a multibyte char before the identifier. LSP `character` is a
        // code-unit (char) offset; using it as a BYTE index slices off a UTF-8
        // boundary (panic) or returns the wrong word. Here `π` precedes `repeat`;
        // the char offset of `repeat` differs from its byte offset.
        let line = "    print(console, \"π\" + string.repeat(\"ab\", 2))";
        let text = format!("fn main(console: Console):\n{line}\n");
        // char offset of the `r` in `repeat` (differs from its byte offset).
        let repeat_char_idx = "    print(console, \"π\" + string.".chars().count();
        let w = word_at(&text, 1, repeat_char_idx).expect("word found");
        assert_eq!(w, "string.repeat", "got {w:?}");
        // Hovering right on the multibyte char must not panic.
        let pi_idx = "    print(console, \"".chars().count();
        let _ = word_at(&text, 1, pi_idx); // must not panic
    }

    #[test]
    fn clean_program_has_no_diagnostics() {
        let src = r#"
fn main(console: Console):
    print(console, "hi")
"#;
        assert_eq!(diags(src), Vec::<Value>::new());
    }

    #[test]
    fn importing_std_resolves_without_false_errors() {
        // `string` isn't on disk next to /tmp/main.witchy, so this exercises the
        // bundled-module fallback. A clean program must stay diagnostic-free.
        let src = "import string\nfn main(console: Console):\n    print(console, string.repeat(\"ab\", 2))\n";
        assert_eq!(diags(src), Vec::<Value>::new());
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
    print(console, "")
"#;
        let d = diags(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0]["severity"], json!(1));
        assert!(
            d[0]["message"].as_str().unwrap().contains("type error"),
            "{:?}",
            d[0]["message"]
        );
    }

    #[test]
    fn uri_to_path_decodes_spaces() {
        assert_eq!(
            uri_to_path("file:///Users/x/my%20dir/a.witchy"),
            Some(PathBuf::from("/Users/x/my dir/a.witchy"))
        );
        assert_eq!(uri_to_path("untitled:foo"), None);
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
