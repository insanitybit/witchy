use super::*;

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
        assert_eq!(wasm_run(src), want, "wasm");
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
        assert_eq!(wasm_run(src), expected, "wasm");
    }

