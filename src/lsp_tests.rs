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
    fn document_symbols_include_generated_items_with_typed_origins() {
        let uri = "file:///tmp/main.witchy";
        let docs = docs(uri, GENERATED_ITEM_SOURCE);
        let response = document_symbol_response(
            &docs,
            &json!({ "textDocument": { "uri": uri } }),
        );
        let symbols = response.as_array().expect("document symbols");
        assert!(symbols.iter().any(|symbol| symbol["name"] == json!("main")));
        let generated = symbols.iter().find(|symbol| {
            symbol["name"].as_str().is_some_and(|name| name.ends_with("generated"))
                && symbol["data"]["generated"] == json!(true)
        }).expect("generated symbol");

        assert_eq!(generated["range"]["start"]["line"], json!(7));
        assert_eq!(generated["data"]["origin"]["definition"]["start"]["line"], json!(4));
        assert_eq!(generated["data"]["origin"]["invocation"]["start"]["line"], json!(8));
        assert_eq!(generated["data"]["origin"]["hole_ancestry"], json!([]));
    }

    #[test]
    fn generated_definition_returns_invocation_and_macro_definition() {
        let uri = "file:///tmp/main.witchy";
        let docs = docs(uri, GENERATED_ITEM_SOURCE);
        let response = definition_response(
            &docs,
            &json!({
                "textDocument": { "uri": uri },
                "position": { "line": 7, "character": 0 },
            }),
        );
        let locations = response.as_array().expect("generated definition locations");
        let lines = locations
            .iter()
            .map(|location| location["range"]["start"]["line"].as_u64().unwrap())
            .collect::<HashSet<_>>();
        assert_eq!(lines, HashSet::from([3, 7]));
        assert!(locations.iter().all(|location| location["uri"] == json!(uri)));
    }

    #[test]
    fn async_and_generator_hover_preserve_function_kind() {
        let mut docs = HashMap::new();
        let generators = include_str!("../examples/generators/src/generators.witchy");
        let generator_uri = "file:///generators.witchy";
        docs.insert(generator_uri.to_string(), generators.to_string());

        let generator_items = completion_response(
            &docs,
            &json!({ "textDocument": { "uri": generator_uri } }),
        );
        let generator_labels: Vec<&str> = generator_items
            .as_array()
            .expect("generator completions")
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        assert!(generator_labels.contains(&"fibs"), "{generator_labels:?}");
        assert!(generator_labels.contains(&"collatz"), "{generator_labels:?}");

        let (fibs_line, fibs_source) = generators
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("fibs().take("))
            .expect("fibs call");
        let fibs_col = fibs_source.find("fibs").unwrap() as u64;
        let fibs = hover_response(
            &docs,
            &json!({
                "textDocument": { "uri": generator_uri },
                "position": { "line": fibs_line, "character": fibs_col },
            }),
        );
        let fibs_contents = fibs["contents"]["value"].as_str().expect("fibs hover");
        assert!(
            fibs_contents.contains("gen fn fibs() -> Iter(Int)"),
            "{fibs_contents}"
        );
        assert!(fibs_contents.contains("Fibonacci"), "{fibs_contents}");

        let async_tasks = include_str!("../examples/async_tasks/src/async_tasks.witchy");
        let async_uri = "file:///async_tasks.witchy";
        docs.insert(async_uri.to_string(), async_tasks.to_string());
        let async_items = completion_response(
            &docs,
            &json!({ "textDocument": { "uri": async_uri } }),
        );
        let async_labels: Vec<&str> = async_items
            .as_array()
            .expect("async completions")
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();
        assert!(async_labels.contains(&"ticker"), "{async_labels:?}");
        assert!(async_labels.contains(&"main"), "{async_labels:?}");

        let (ticker_line, ticker_source) = async_tasks
            .lines()
            .enumerate()
            .find(|(_, line)| line.contains("ticker(console, name, n - 1)"))
            .expect("ticker call");
        let ticker_col = ticker_source.find("ticker").unwrap() as u64;
        let ticker = hover_response(
            &docs,
            &json!({
                "textDocument": { "uri": async_uri },
                "position": { "line": ticker_line, "character": ticker_col },
            }),
        );
        let ticker_contents = ticker["contents"]["value"].as_str().expect("ticker hover");
        assert!(
            ticker_contents
                .contains("async fn ticker(console: Console, name: String, n: Int)"),
            "{ticker_contents}"
        );

        assert_eq!(fn_decl_line(generators, "fibs"), Some(12));
        assert_eq!(fn_decl_line(async_tasks, "ticker"), Some(14));
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
    fn rfc0081_lsp_accepts_and_displays_existential_signatures() {
        let src = "trait Render:\n    fn render(let self) -> String\n\nfn render_one(value: dyn Render) -> String:\n    value.render()\n\nfn main(console: Console):\n    console.print(\"ready\")\n";
        let uri = "file:///rfc0081-hover.witchy";
        let docs = docs(uri, src);
        assert!(
            compute_diagnostics(uri, src, &HashMap::new()).is_empty(),
            "valid existential signatures must not retain the old feature-stage diagnostic"
        );

        let response = hover_at(&docs, uri, src, 3, "render_one");
        let contents = response["contents"]["value"].as_str().expect("hover text");
        assert!(
            contents.contains("fn render_one(value: dyn Render) -> String"),
            "{contents}"
        );
    }

    #[test]
    fn rfc0082_dynamic_dispatch_is_visible_in_hover_and_symbols() {
        let src = r#"type Counter derive(Reflect):
    value: Int

@dynamic
pub fn bump(self: Counter, amount: Int) -> Counter:
    Counter(self.value + amount)

fn main():
    bump(Counter(1), 2)
"#;
        let uri = "file:///rfc0082-dynamic-lsp.witchy";
        let docs = docs(uri, src);
        let hover = hover_at(&docs, uri, src, 8, "bump");
        let hover_text = hover["contents"]["value"].as_str().expect("hover text");
        assert!(hover_text.contains("@dynamic"), "{hover_text}");
        assert!(hover_text.contains("Dynamic dispatch"), "{hover_text}");

        let response = document_symbol_response(
            &docs,
            &json!({ "textDocument": { "uri": uri } }),
        );
        let bump = response
            .as_array()
            .expect("document symbols")
            .iter()
            .find(|symbol| {
                symbol["name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with("bump"))
            })
            .unwrap_or_else(|| panic!("bump symbol in {response}"));
        assert_eq!(bump["detail"], json!("dynamic dispatch (@dynamic)"));
        assert_eq!(bump["data"]["dynamicDispatch"], json!(true));
    }

    #[test]
    fn rfc0082_invalid_dynamic_declaration_is_a_loud_lsp_diagnostic() {
        let src = "@dynamic\nfn hidden(self: Int) -> Int:\n    self\n\nfn main():\n    hidden(1)\n";
        let diagnostics = diags(src);
        let diagnostic = diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("must be public"))
            })
            .expect("public dynamic declaration diagnostic");
        assert_eq!(diagnostic["range"]["start"]["line"], json!(0));
    }

    #[test]
    fn rfc0098_hover_displays_structural_record_composition() {
        let src = "type Summary = .{id: Int, label: String}\n// Stored rows add a revision.\ntype Detailed = .{..Summary, revision: Int}\n\nfn keep(row: Detailed) -> Detailed:\n    row\n";
        let uri = "file:///rfc0098-hover.witchy";
        let docs = docs(uri, src);
        let response = hover_at(&docs, uri, src, 4, "Detailed");
        let contents = response["contents"]["value"].as_str().expect("alias hover text");
        assert!(
            contents.contains("type Detailed = .{..Summary, revision: Int}"),
            "{contents}"
        );
        assert!(contents.contains("Stored rows add a revision."), "{contents}");
    }

    #[test]
    fn rfc0098_lsp_width_error_points_at_the_projection_site() {
        let src = "type Need = .{a: Int, b: String}\n\nfn take(row: Need):\n    ()\n\nfn main():\n    take(.{a: 1})\n";
        let diagnostics = diags(src);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert_eq!(diagnostics[0]["range"]["start"]["line"], json!(6));
        let message = diagnostics[0]["message"].as_str().expect("diagnostic message");
        assert!(message.contains("missing required field `b`"), "{message}");
    }

    #[test]
    fn hover_resolves_imported_module_functions() {
        let src = "\nfn main(console: Console):\n    console.print(\"ab\".repeat(2))\n";
        let uri = "file:///t.witchy";
        let docs = docs(uri, src);
        let resp = hover_at(&docs, uri, src, 2, "repeat");
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(contents.contains("repeat"), "{contents}");
        // Impl methods are reported under their Type qualifier.
        assert!(
            contents.contains("String.repeat("),
            "expected Type-qualified signature, got: {contents}"
        );
    }

    /// The methods-first hover preference applies ONLY to the `receiver.name`
    /// shape. A bare name that the document itself defines must keep resolving
    /// to the document's own function, even when a std impl method shares the
    /// name; an explicit module qualifier must keep meaning the module function.
    #[test]
    fn hover_bare_and_module_qualified_names_are_not_shadowed_by_methods() {
        // `count` collides with String.count; the bare call must hover the
        // local free function.
        let src = "// How many widgets.\nfn count(n: Int) -> Int:\n    n\n\nfn main(console: Console):\n    console.print(\"${count(3)}\")\n";
        let uri = "file:///t.witchy";
        let mut docs = docs(uri, src);
        let resp = hover_at(&docs, uri, src, 5, "count");
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(
            contents.contains("fn count(n: Int) -> Int"),
            "bare name must prefer the document's own function, got: {contents}"
        );
        assert!(!contents.contains("String.count"), "{contents}");

        // `list.repeat` is module-qualified: the module free function, not
        // String.repeat.
        let src2 = "\nfn main(console: Console):\n    console.print(\"${list.repeat(0, 2)}\")\n";
        let uri2 = "file:///t2.witchy";
        docs.insert(uri2.to_string(), src2.to_string());
        let resp2 = hover_at(&docs, uri2, src2, 2, "repeat");
        let contents2 = resp2["contents"]["value"].as_str().expect("hover text");
        assert!(
            contents2.contains("fn list.repeat("),
            "module-qualified name must stay the module function, got: {contents2}"
        );
        assert!(!contents2.contains("String.repeat"), "{contents2}");
    }

    #[test]
    fn associated_std_function_hover_uses_type_owner() {
        let src = "\nfn main(console: Console):\n    let net_policy = Net.tcp(\"127.0.0.1\", 8080)\n    let dir_policy = Dir.ext(\".log\")\n    console.print(\"x\".repeat(2))\n";
        let uri = "file:///policy-hover.witchy";
        let docs = docs(uri, src);

        let net = hover_at(&docs, uri, src, 2, "tcp");
        let net_contents = net["contents"]["value"].as_str().expect("Net.tcp hover");
        assert!(
            net_contents.contains("Net.tcp(host: String, port: Int) -> NetPolicy"),
            "{net_contents}"
        );
        assert!(!net_contents.contains("policy.tcp"), "{net_contents}");

        let dir = hover_at(&docs, uri, src, 3, "ext");
        let dir_contents = dir["contents"]["value"].as_str().expect("Dir.ext hover");
        assert!(
            dir_contents.contains("Dir.ext(suffix: String) -> DirPolicy"),
            "{dir_contents}"
        );
        assert!(!dir_contents.contains("policy.ext"), "{dir_contents}");
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
        assert_eq!(
            qualify_signature("pub async fn ticker(clock: Clock)", "jobs."),
            "pub async fn jobs.ticker(clock: Clock)"
        );
        assert_eq!(
            qualify_signature("pub gen fn fibs() -> Int", "sequence."),
            "pub gen fn sequence.fibs() -> Int"
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
    fn local_disk_imports_feed_completion_and_hover() {
        // BUG-169: a disk-backed sibling `helper.witchy` must contribute its
        // `pub fn`s to completion and hover, exactly like a std module.
        let dir = std::env::temp_dir().join(format!("witchy-lsp-169-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("helper.witchy"),
            "// Greets someone warmly.\npub fn greet(name: String) -> String:\n    \"hi \" + name\n",
        )
        .unwrap();
        let main = dir.join("main.witchy");
        let main_src =
            "import helper\n\nfn main(console: Console):\n    console.print(helper.greet(\"x\"))\n";
        std::fs::write(&main, main_src).unwrap();
        let uri = format!("file://{}", main.to_str().unwrap());

        let mut docs = HashMap::new();
        docs.insert(uri.clone(), main_src.to_string());

        let items = completion_response(&docs, &json!({ "textDocument": { "uri": uri } }));
        let labels: Vec<String> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect();
        assert!(
            labels.contains(&"helper.greet".to_string()),
            "local import fn offered: {labels:?}"
        );

        // Hover on `greet` in `helper.greet(...)` on line 3.
        let col = main_src.lines().nth(3).unwrap().find("greet").unwrap() as u64;
        let resp = hover_response(
            &docs,
            &json!({
                "textDocument": { "uri": uri },
                "position": { "line": 3, "character": col },
            }),
        );
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(contents.contains("fn helper.greet("), "{contents}");
        assert!(contents.contains("Greets someone warmly."), "{contents}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn from_import_offers_bare_names_and_hovers() {
        // BUG-388: `from X import Y` binds `Y` unqualified. Completion must offer
        // the bare name, and hover on a bare use must resolve it in module X.
        let src = "from string import from_code\n\nfn main(console: Console):\n    console.print(from_code(65))\n";
        let uri = "file:///t.witchy";
        let docs = docs(uri, src);
        let items = completion_response(
            &docs,
            &json!({ "textDocument": { "uri": uri } }),
        );
        let labels: Vec<String> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["label"].as_str().unwrap().to_string())
            .collect();
        assert!(
            labels.contains(&"from_code".to_string()),
            "bare from-import offered: {labels:?}"
        );

        // Hover on the bare `from_code` on line 3.
        let resp = hover_at(&docs, uri, src, 3, "from_code");
        assert!(
            resp["contents"]["value"]
                .as_str()
                .is_some_and(|c| c.contains("fn string.from_code(")),
            "{resp}"
        );
    }

    #[test]
    fn hover_resolves_receiver_method_calls() {
        // BUG-174: `xs.push(1)` — word_at reads `xs.push`; treating `xs` as a
        // module used to return null. The method must resolve against the prelude
        // data modules (`list.push`).
        let src = "fn main(console: Console):\n    var xs = [1]\n    xs.push(2)\n";
        let uri = "file:///t.witchy";
        let mut docs = docs(uri, src);
        let resp = hover_at(&docs, uri, src, 2, "push");
        let contents = resp["contents"]["value"].as_str().expect("hover text");
        assert!(contents.contains("List.push("), "{contents}");

        // `xs.length` (no call) must resolve too.
        let src2 = "fn main(console: Console):\n    let xs = [1]\n    let n = xs.length\n";
        let uri2 = "file:///t2.witchy";
        docs.insert(uri2.to_string(), src2.to_string());
        let resp2 = hover_at(&docs, uri2, src2, 2, "length");
        assert!(
            resp2["contents"]["value"]
                .as_str()
                .is_some_and(|c| c.contains("List.length(")),
            "{resp2}"
        );
    }

    #[test]
    fn clean_program_has_no_diagnostics() {
        let src = r#"
fn main(console: Console):
    console.print("hi")
"#;
        assert_eq!(diags(src), Vec::<Value>::new());
    }

    /// (BUG-165) A `mode opt` file whose ownership-relevant parameter carries no
    /// convention is rejected by `witchy check`; the LSP must publish that same
    /// error instead of silently accepting a program the compiler refuses.
    #[test]
    fn mode_opt_ownership_violation_is_a_diagnostic() {
        let bad = "mode opt\n\nimport list\n\nfn tag(xs: List(Int)) -> Int:\n    list.length(xs)\n\nfn main(console: Console):\n    console.print(\"${tag([1, 2, 3])}\")\n";
        let d = diags(bad);
        assert!(
            d.iter().any(|x| x["severity"] == json!(1)
                && x["message"].as_str().unwrap().contains("ownership convention")),
            "mode opt must flag the unannotated param as an error: {d:?}"
        );

        // Declaring the convention (`let xs`) satisfies the contract — no error.
        let good = bad.replace("fn tag(xs:", "fn tag(let xs:");
        let d = diags(&good);
        assert!(
            !d.iter().any(|x| x["message"].as_str().unwrap().contains("ownership convention")),
            "an annotated parameter must be clean: {d:?}"
        );

        // A type qualifier is not a calling convention: `unique T` still needs
        // the explicit `let`/`own`/`var` protocol in a mode file.
        let qualified = bad.replace("xs: List(Int)", "xs: unique List(Int)");
        let d = diags(&qualified);
        assert!(
            d.iter().any(|x| x["message"].as_str().unwrap().contains("ownership convention")),
            "a qualifier must not hide a missing convention: {d:?}"
        );

        // The contract is opt-in: without `mode opt`, the same code is accepted.
        let no_mode = bad.replace("mode opt\n\n", "");
        let d = diags(&no_mode);
        assert!(
            !d.iter().any(|x| x["message"].as_str().unwrap().contains("ownership convention")),
            "no mode -> no ownership-convention error: {d:?}"
        );
    }

    #[test]
    fn mode_opt_no_copy_violation_names_the_alias_reason() {
        let bad = "mode opt\n\nimport dict\n\nfn main(console: Console):\n    var d = dict.new()\n    let _ = d.insert(\"a\", 1)\n    let snapshot = d\n    d.insert(\"a\", 2)\n    console.print(\"${snapshot.length()}\")\n";
        let diagnostics = diags(bad);
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic["severity"] == json!(1)
                    && diagnostic["range"]["start"]["line"] == json!(8)
                    && diagnostic["message"]
                        .as_str()
                        .is_some_and(|message| {
                            message.contains("no-copy `var` contract")
                                && message.contains("bound to a new name")
                        })
            }),
            "the editor must explain the exact ownership loss: {diagnostics:?}"
        );

        let fresh = bad.replace("    let snapshot = d\n", "").replace("snapshot.length()", "d.length()");
        let diagnostics = diags(&fresh);
        assert!(
            !diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("no-copy `var` contract"))),
            "a fresh owner satisfies the contract: {diagnostics:?}"
        );

        let normal = bad.replacen("mode opt\n\n", "", 1);
        let diagnostics = diags(&normal);
        assert!(
            !diagnostics.iter().any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("no-copy `var` contract"))),
            "normal mode keeps the copy-correct fallback: {diagnostics:?}"
        );
    }

    #[test]
    fn mode_opt_fip_violation_names_the_recursive_shape() {
        let bad = "mode opt\n\ntype State:\n    count: Int\n\nfn run(own state: unique State, n: Int) -> unique State:\n    if n == 0:\n        return state\n    let next = run(state, n - 1)\n    next\n\nfn main(console: Console):\n    console.print(\"${run(State(0), 2).count}\")\n";
        let diagnostics = diags(bad);
        assert!(
            diagnostics.iter().any(|diagnostic| {
                diagnostic["severity"] == json!(1)
                    && diagnostic["message"]
                        .as_str()
                        .is_some_and(|message| {
                            message.contains("functional-in-place contract failed")
                                && message.contains("not in tail position")
                        })
            }),
            "the editor must surface the FIP proof failure: {diagnostics:?}"
        );
    }

    #[test]
    fn importing_std_resolves_without_false_errors() {
        // `string` isn't on disk next to /tmp/main.witchy, so this exercises the
        // bundled-module fallback. A clean program must stay diagnostic-free.
        let src = "fn main(console: Console):\n    console.print(\"ab\".repeat(2))\n";
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
    fn missing_on_disk_import_resolves_from_open_buffer() {
        // BUG-168: an unsaved sibling buffer (not yet on disk) still resolves.
        let dir = std::env::temp_dir().join(format!("witchy-lsp-168b-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.witchy");
        let helper = dir.join("helper.witchy");
        let src = "import helper\n\nfn main(console: Console):\n    console.print(helper.greet(\"x\"))\n";
        let main_uri = format!("file://{}", main.to_str().unwrap());
        let helper_uri = format!("file://{}", helper.to_str().unwrap());
        let mut docs = HashMap::new();
        docs.insert(main_uri.clone(), src.to_string());
        docs.insert(
            helper_uri,
            "pub fn greet(name: String) -> String:\n    \"hi \" + name\n".to_string(),
        );
        // helper.witchy is NOT written to disk.
        let d = compute_diagnostics(&main_uri, src, &docs);
        assert_eq!(d, Vec::<Value>::new(), "{d:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn broken_sibling_import_surfaces_a_diagnostic() {
        // BUG-137: a neighbour that fails to parse must not be silently skipped.
        let dir = std::env::temp_dir().join(format!("witchy-lsp-137-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("helper.witchy"), "pub fn f( -> :\n").unwrap();
        let main = dir.join("main.witchy");
        let src = "import helper\n\nfn main(console: Console):\n    console.print(\"x\")\n";
        let uri = format!("file://{}", main.to_str().unwrap());
        let mut docs = HashMap::new();
        docs.insert(uri.clone(), src.to_string());
        let d = compute_diagnostics(&uri, src, &docs);
        assert_eq!(d.len(), 1, "{d:?}");
        let msg = d[0]["message"].as_str().unwrap();
        assert!(msg.contains("imported module `helper` failed to parse"), "{msg}");
        assert_eq!(d[0]["range"]["start"]["line"], json!(0), "{d:?}");
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
    fn missing_module_function_underlines_qualified_call() {
        let uri = "file:///tmp/witchy-lsp-162-main.witchy";
        let src = "import string\n\nfn main(console: Console):\n    console.print(\"before\")\n    console.print(string.not_real(\"x\"))\n";
        let mut docs = HashMap::new();
        docs.insert(uri.to_string(), src.to_string());

        let diagnostics = compute_diagnostics(uri, src, &docs);
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(
            diagnostics[0]["message"]
                .as_str()
                .is_some_and(|message| message.contains("module `string` has no function `not_real`")),
            "{diagnostics:?}",
        );
        assert_eq!(diagnostics[0]["range"]["start"]["line"], json!(4));
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
