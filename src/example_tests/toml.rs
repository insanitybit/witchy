use super::*;

    #[test]
    fn rfc0054_toml_decode_uses_typed_error_and_converts_to_string() {
        let src = "import json\nimport toml\nfrom toml import Toml\n\nfn via_string() -> Result(Toml, String):\n    let doc = toml.decode(\"not a toml line\")?\n    Ok(doc)\n\nfn main(console: Console):\n    match json.decode(\"1 2\"):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(json.decode_error_message(e))\n    match toml.decode(\"not a toml line\"):\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(toml.decode_error_message(e))\n    match via_string():\n        Ok(_) -> console.print(\"bad\")\n        Err(e) -> console.print(e)\n";
        let expected = [
            "unexpected trailing content at 2",
            "`not a toml line` is not a TOML line (expected `key = value`, a `[section]` header, or a `#` comment)",
            "`not a toml line` is not a TOML line (expected `key = value`, a `[section]` header, or a `#` comment)",
        ];
        assert_eq!(link_run(src), expected, "interp: typed toml.TomlDecodeError");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: typed toml.TomlDecodeError",
        );
    }

    #[test]
    fn toml_decode_rejects_empty_table_path_segments() {
        let src = "import toml\n\nfn status(text: String) -> String:\n    match toml.decode(text):\n        Ok(_) -> \"ok\"\n        Err(e) ->\n            let msg = toml.decode_error_message(e)\n            if msg.contains(\"empty path segment\"):\n                \"err:empty\"\n            else:\n                \"err:\" + msg\n\nfn main(console: Console):\n    console.print(\"empty_header=\" + status(\"[]\\nroot = 1\\n\"))\n    console.print(\"empty_mid=\" + status(\"[a..b]\\nx = 1\\n\"))\n    console.print(\"empty_tail=\" + status(\"[a.]\\nx = 1\\n\"))\n    console.print(\"empty_head=\" + status(\"[.a]\\nx = 1\\n\"))\n    console.print(\"array_empty_mid=\" + status(\"[[a..b]]\\nx = 1\\n\"))\n    console.print(\"quoted_dot=\" + status(\"[\\\"a..b\\\"]\\nx = 1\\n\"))\n";
        let expected = [
            "empty_header=err:empty",
            "empty_mid=err:empty",
            "empty_tail=err:empty",
            "empty_head=err:empty",
            "array_empty_mid=err:empty",
            "quoted_dot=ok",
        ];
        assert_eq!(link_run(src), expected, "interp: TOML empty table path segments");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: TOML empty table path segments",
        );
    }

    /// `std/toml` (pure witchy) reads `witchy.toml` manifests: `toml.get` for
    /// string values by `section.key`, `toml.get_array` for string arrays — what
    /// a self-hosted package manager needs to read a manifest.
    #[test]
    fn toml_module_reads_manifest_values() {
        let src = r#"import toml

fn main(console: Console):
    let m = "[rune]\nname = \"acme/widget\"\nversion = \"1.2.0\"\n\n[capabilities]\nruntime = [\"Net\", \"Console\"]\n"
    console.print(opt(toml.get(m, "rune.name")))
    console.print(opt(toml.get(m, "rune.version")))
    console.print(list.join(toml.get_array(m, "capabilities.runtime"), "|"))
    console.print(opt(toml.get(m, "rune.absent")))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec!["acme/widget", "1.2.0", "Net|Console", "(none)"]
        );
    }

    /// `toml.decode` builds a structured `Toml` tree — top-level keys, `[section]`
    /// and dotted `[a.b]` tables, and typed string/int/bool/array values —
    /// identically on both backends.
    #[test]
    fn toml_decode_builds_typed_tree_on_both_backends() {
        let src = r#"import toml

fn main(console: Console):
    let doc = "title = \"demo\"\nport = 8080\nenabled = true\ntags = [\"a\", \"b\"]\n\n[server]\nhost = \"localhost\"\nworkers = 4\n\n[server.tls]\nenabled = false\n"
    match toml.decode(doc):
        Ok(t) -> console.print("${t}")
        Err(e) -> console.print(toml.decode_error_message(e))
"#;
        let want = vec!["TomlTable([(title, TomlString(demo)), (port, TomlInt(8080)), (enabled, TomlBool(true)), (tags, TomlArray([TomlString(a), TomlString(b)])), (server, TomlTable([(host, TomlString(localhost)), (workers, TomlInt(4)), (tls, TomlTable([(enabled, TomlBool(false))]))]))])".to_string()];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (RFC-0044 rule 2) `toml.decode`'s `Err` is reachable: a non-blank,
    /// non-comment line that is neither a `[section]` header nor a `key = value`
    /// pair is malformed, and decoding errors naming the offending line — the
    /// always-`Ok` mimicry the RFC banned. Both backends agree.
    #[test]
    fn toml_decode_errors_on_a_malformed_line() {
        let src = r#"import toml

fn main(console: Console):
    match toml.decode("title = \"ok\"\nthis line has no equals\n"):
        Ok(_) -> console.print("unexpected-ok")
        Err(e) -> console.print(toml.decode_error_message(e))
"#;
        let want = vec!["`this line has no equals` is not a TOML line (expected `key = value`, a `[section]` header, or a `#` comment)".to_string()];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// (BUG-488) `toml.decode` rejects scalar/table namespace collisions: a key
    /// defined as a scalar cannot later be opened as a table, and vice versa.
    #[test]
    fn toml_decode_rejects_scalar_table_namespace_collisions() {
        let src = r#"import toml

fn try(doc: String) -> String:
    match toml.decode(doc):
        Ok(_) -> "ok"
        Err(e) -> toml.decode_error_message(e)

fn main(console: Console):
    // scalar then table: top-level `a = 1` then `[a]` header
    console.print(try("a = 1\n[a]\nb = 2\n"))
    // array-of-tables collides with scalar
    console.print(try("a = 1\n[[a]]\nb = 2\n"))
    // nested: scalar `a` blocks `[a.b]`
    console.print(try("a = 1\n[a.b]\nc = 3\n"))
    // nested scalar blocks sub-table: `[a]` defines `b = 1`, then `[a.b]` opens it
    console.print(try("[a]\nb = 1\n[a.b]\nc = 2\n"))
    // valid: separate namespaces
    console.print(try("x = 1\n[y]\nz = 2\n"))
"#;
        let want = vec![
            "`a` is already defined as a value, cannot redefine as a table".to_string(),
            "`a` is already defined as a value, cannot redefine as `[[a]]`".to_string(),
            "`a` is already defined as a value, cannot redefine as a table".to_string(),
            "`a.b` is already defined as a value, cannot redefine as a table".to_string(),
            "ok".to_string(),
        ];
        assert_eq!(link_run(src), want.clone(), "interpreter");
        assert_eq!(wasm_run(src), want, "wasm");
    }

    /// Trailing `# comments` on values and arrays are stripped, but a `#` inside a
    /// quoted string and a `]` inside an array element (e.g. "Dir[Read]") are
    /// preserved — real manifests carry comments, so the reader must tolerate them.
    #[test]
    fn toml_module_ignores_trailing_comments() {
        let src = r#"import toml

fn main(console: Console):
    let m = "[rune]\nname = \"acme/widget\"  # the canonical name\ntag = \"v#1\"  # has a hash inside\n\n[capabilities]\nruntime = [\"Console\", \"Dir[Read]\"]  # what it needs\n"
    console.print(opt(toml.get(m, "rune.name")))
    console.print(opt(toml.get(m, "rune.tag")))
    console.print(list.join(toml.get_array(m, "capabilities.runtime"), "|"))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec!["acme/widget", "v#1", "Console|Dir[Read]"]
        );
    }

    /// `toml.table`/`keys`/`inline_get` enumerate a table whose keys aren't known
    /// ahead of time (`[dependencies]`, whose values are inline tables), and the
    /// structured `toml.decode` + `array_of_tables`/`table_field`/`as_string` walk
    /// a `[[rune]]` array-of-tables (a `witchy.lock`) through the ONE strict parser
    /// (BUG-373) — the manifest+lock shapes a self-hosted package manager reads.
    #[test]
    fn toml_module_enumerates_tables_and_arrays() {
        let src = r#"import toml

fn main(console: Console):
    let m = "[rune]\nname = \"ledger\"\n\n[dependencies]\n\"money\" = { path = \"../money\" }\n\"acme/util\" = { path = \"../util\", version = \"1.2\" }\n"
    console.print(list.join(toml.keys(m, "dependencies"), "|"))
    console.print(opt(toml.inline_get("{ path = \"../money\" }", "path")))
    console.print(opt(toml.inline_get("{ path = \"../util\", version = \"1.2\" }", "version")))
    console.print(list.join(toml.inline_get_array("{ programs = [\"git\", \"witchy\"], child-paths = [\"~/.gitconfig\"] }", "programs"), "|"))
    console.print(list.join(toml.inline_get_array("{ programs = [\"git\", \"witchy\"], child-paths = [\"~/.gitconfig\"] }", "child-paths"), "|"))
    let lock = "[[rune]]\nname = \"money\"\nhash = \"sha256:aa\"\nruntime_footprint = [\"Console\"]\n\n[[rune]]\nname = \"util\"\nhash = \"sha256:bb\"\nruntime_footprint = [\"Console\", \"Dir[Read]\"]\n"
    console.print(rune_summary(lock))

fn rune_summary(lock: String) -> String:
    match toml.decode(lock):
        Err(e) -> "decode error: " + toml.decode_error_message(e)
        Ok(doc) ->
            match toml.array_of_tables(doc, "rune"):
                Err(e) -> "aot error: " + toml.decode_error_message(e)
                Ok(entries) ->
                    var names = []
                    for entry in entries:
                        let caps = match toml.string_array_field(entry, "runtime_footprint"):
                            Ok(xs) -> list.join(xs, ",")
                            Err(e) -> "(bad:" + toml.decode_error_message(e) + ")"
                        list.push(names, field(entry, "name") + "=" + field(entry, "hash") + "[" + caps + "]")
                    list.join(names, "|") + "|" + bad_name()

fn bad_name() -> String:
    match toml.decode("[[rune]]\nname = 42\n"):
        Err(e) -> "decode: " + toml.decode_error_message(e)
        Ok(doc) ->
            match toml.array_of_tables(doc, "rune"):
                Err(e) -> "aot: " + toml.decode_error_message(e)
                Ok(entries) ->
                    match toml.required_string(list.at(entries, 0), "name", "a [[rune]]"):
                        Ok(s) -> "wrongly accepted: " + s
                        Err(e) -> "fail-closed: " + toml.decode_error_message(e)

fn field(entry: toml.Toml, key: String) -> String:
    match toml.table_field(entry, key):
        None -> "(none)"
        Some(v) ->
            match toml.as_string(v, key):
                Ok(s) -> s
                Err(e) -> "(bad:" + toml.decode_error_message(e) + ")"

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "money|acme/util",
                "../money",
                "1.2",
                "git|witchy",
                "~/.gitconfig",
                "money=sha256:aa[Console]|util=sha256:bb[Console,Dir[Read]]|fail-closed: a [[rune]]'s `name` field is not a string (found an integer)"
            ]
        );
    }

    /// std/toml: `decode` rejects an unterminated string / array (BUG-196) and
    /// duplicate keys / `[table]` headers (BUG-245), represents `[[table]]` as an
    /// array-of-tables instead of collapsing it (BUG-373), and reads a `[section]`
    /// header that carries a trailing `# comment` (BUG-389); `inline_get` respects
    /// commas inside quoted values (BUG-355). Both backends agree.
    #[test]
    fn toml_rejects_malformed_and_supports_array_tables_on_both_backends() {
        let src = "import toml\n\
                   import option\n\
                   fn dec(label: String, text: String, console: Console):\n\
                   \x20   match toml.decode(text):\n\
                   \x20       Ok(t) -> console.print(label + \": \" + \"${t}\")\n\
                   \x20       Err(e) -> console.print(label + \": ERR\")\n\
                   fn main(console: Console):\n\
                   \x20   dec(\"unterm_string\", \"name = \\\"oops\", console)\n\
                   \x20   dec(\"unterm_array\", \"deps = [\\\"a\\\",\\\"b\\\"\", console)\n\
                   \x20   dec(\"dup_key\", \"a = 1\\na = 2\", console)\n\
                   \x20   dec(\"dup_table\", \"[s]\\nx = 1\\n[s]\\ny = 2\", console)\n\
                   \x20   dec(\"header_comment\", \"[s] # note\\nx = 1\", console)\n\
                   \x20   dec(\"array_tables\", \"[[r]]\\nn = \\\"a\\\"\\n[[r]]\\nn = \\\"b\\\"\", console)\n\
                   \x20   console.print(\"inline: \" + option.unwrap_or(toml.inline_get(\"{ path = \\\"../foo,bar\\\" }\", \"path\"), \"?\"))\n";
        let expected = [
            "unterm_string: ERR",
            "unterm_array: ERR",
            "dup_key: ERR",
            "dup_table: ERR",
            "header_comment: TomlTable([(s, TomlTable([(x, TomlInt(1))]))])",
            "array_tables: TomlTable([(r, TomlArray([TomlTable([(n, TomlString(a))]), TomlTable([(n, TomlString(b))])]))])",
            "inline: ../foo,bar",
        ];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// (BUG-447) `std/toml` shares ONE quote-aware key grammar across decode/get/
    /// table: a quoted literal-dot table key `["a.b"]` is one key (not nested
    /// a->b), a quoted key holding `=` (`"k=v" = 7`) isn't mis-split, and quoted
    /// dependency names round-trip. Runs on BOTH backends.
    #[test]
    fn toml_quoted_keys_share_one_grammar() {
        let src = "import toml\n\nfn main(console: Console):\n    let doc = \"[\\\"a.b\\\"]\\nx = 1\\n\\n[plain]\\ny = 2\\n\"\n    console.print(toml.get(doc, \"\\\"a.b\\\".x\") ?? \"MISS\")\n    console.print(toml.get(doc, \"plain.y\") ?? \"MISS\")\n    let doc2 = \"[t]\\n\\\"k=v\\\" = 7\\n\"\n    console.print(toml.get(doc2, \"t.\\\"k=v\\\"\") ?? \"MISS\")\n    let doc3 = \"[dependencies]\\n\\\"acme/money\\\" = \\\"1.0\\\"\\n\"\n    console.print(\"${list.length(toml.table(doc3, \"dependencies\"))}\")\n";
        let expected: Vec<String> = ["1", "2", "7", "1"].iter().map(|s| s.to_string()).collect();
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }

    /// (CJ-06) `toml.dep_requirement` reads a `[dependencies]` version requirement
    /// in EITHER form — a bare string (`"acme/x" = "^1.0"`), whose value IS the
    /// requirement, or an inline table (`{ version = "^2.0" }`) — and returns
    /// `None` for a path-only dep. This is what stops a bare-string dep from being
    /// silently skipped by transitive resolution / `outdated`. Both backends.
    #[test]
    fn toml_dep_requirement_reads_bare_string_and_inline_table() {
        let src = r#"import toml

fn main(console: Console):
    let m = "[dependencies]\n\"acme/x\" = \"^1.0\"\n\"acme/y\" = { version = \"^2.0\" }\n\"acme/z\" = { path = \"../z\" }\n"
    for name, inline in toml.table(m, "dependencies"):
        console.print(name + "=" + opt(toml.dep_requirement(inline)))

fn opt(o: Option(String)) -> String:
    match o:
        Some(s) -> s
        None -> "(none)"
"#;
        let expected = ["acme/x=^1.0", "acme/y=^2.0", "acme/z=(none)"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(wasm_run(src), expected, "wasm");
    }
