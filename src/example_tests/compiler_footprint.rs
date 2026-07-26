use super::*;
use crate::{ast, interpreter, parser, typeck};

    #[test]
    fn coven_maintainer_policy_string_array_is_strict() {
        let src = "import coven_trust\nimport list\n\nfn report(text: String) -> String:\n    match coven_trust.string_array(text):\n        Ok(xs) -> \"ok:\" + list.join(xs, \",\")\n        Err(e) -> \"err:\" + coven_trust.trust_policy_error_message(e)\n\nfn tag(text: String) -> String:\n    match coven_trust.string_array(text):\n        Ok(_xs) -> \"typed:ok\"\n        Err(e) ->\n            match e:\n                coven_trust.PolicyInvalidJson(_) -> \"typed:json\"\n                coven_trust.PolicyNotStringArray -> \"typed:shape\"\n                coven_trust.PolicyNonString(i) -> \"typed:item:\" + \"${i}\"\n\nfn main(console: Console):\n    console.print(report(\"[\\\"gha|alice\\\",\\\"gha|bob\\\"]\"))\n    console.print(report(\"{\\\"maintainers\\\":[]}\"))\n    console.print(report(\"[\\\"gha|alice\\\",7]\"))\n    console.print(report(\"[\"))\n    console.print(tag(\"[\\\"gha|alice\\\",7]\"))\n";
        let sources = [
            ("coven_json", include_str!("../../projects/coven/src/coven_json.witchy")),
            ("coven_trust", include_str!("../../projects/coven/src/coven_trust.witchy")),
            ("main", src),
        ];
        let modules: Vec<(String, ast::Module)> = sources
            .iter()
            .map(|(name, source)| ((*name).to_string(), parser::parse_module(source).expect("parse")))
            .collect();
        let linked = crate::pipeline::link(modules, "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        let interp = interpreter::run_module(linked, ".", Vec::new()).expect("interp");
        let wasm = run_linked_on_wasm(&sources, "main");
        assert_eq!(interp, wasm, "strict maintainer policy decode must be backend-stable");
        assert_eq!(
            wasm,
            [
                "ok:gha|alice,gha|bob",
                "err:expected a JSON array of strings",
                "err:expected a string at index 1",
                "err:unexpected end of input",
                "typed:item:1",
            ],
        );
    }

    #[test]
    fn native_compiler_intrinsics_reject_comptime_source_strings() {
        // The compiler-service natives live above the runtime kernel in
        // `witchy-interp`; install their vtable before resolving them here.
        witchy_interp::compiler_natives::install();
        use witchy_runtime::value::NativeValue;

        let invalid = "fn main(console: Console):\n    missing(console)\n";
        let footprint = witchy_runtime::native::lookup("compiler.footprint")
            .expect("compiler.footprint native");
        let NativeValue::Str(json) = footprint(&[NativeValue::Str(invalid.into())]).expect("native call") else {
            panic!("compiler.footprint must return a JSON string");
        };
        assert!(json.contains("\"error\""), "type-invalid source must fail closed: {json}");
        assert!(json.contains("unknown function `missing`"), "error must include the type error: {json}");

        let src = "comptime:\n    emit(\"pub fn generated(net: Net) -> Int:\")\n    emit(\"    7\")\n";
        let NativeValue::Str(json) = footprint(&[NativeValue::Str(src.into())]).expect("native call") else {
            panic!("compiler.footprint must return a JSON string");
        };
        assert!(json.contains("\"error\""), "comptime source must fail closed: {json}");
        assert!(json.contains("does not support comptime"), "error must name the boundary: {json}");

        let diff =
            witchy_runtime::native::lookup("compiler.diff").expect("compiler.diff native");
        let NativeValue::Str(json) = diff(&[
            NativeValue::Str("pub fn direct() -> Int:\n    0\n".into()),
            NativeValue::Str(invalid.into()),
        ])
        .expect("native diff")
        else {
            panic!("compiler.diff must return a JSON string");
        };
        assert!(json.contains("\"error\""), "type-invalid diff must fail closed: {json}");
        assert!(json.contains("unknown function `missing`"), "diff error must include the type error: {json}");

        let NativeValue::Str(json) =
            diff(&[NativeValue::Str("pub fn direct() -> Int:\n    0\n".into()), NativeValue::Str(src.into())])
                .expect("native diff")
        else {
            panic!("compiler.diff must return a JSON string");
        };
        assert!(json.contains("\"error\""), "comptime diff must fail closed: {json}");
        assert!(json.contains("does not support comptime"), "diff error must name the boundary: {json}");

        let doc = witchy_runtime::native::lookup("compiler.doc").expect("compiler.doc native");
        let NativeValue::Str(md) =
            doc(&[NativeValue::Str("generated".into()), NativeValue::Str(src.into())]).expect("native doc")
        else {
            panic!("compiler.doc must return a markdown string");
        };
        assert!(md.contains("doc error:"), "comptime doc must return an error comment: {md}");
        assert!(md.contains("does not support comptime"), "doc error must name the boundary: {md}");
    }

    #[test]
    fn compiler_footprint_rejects_type_invalid_sources_on_both_backends() {
        let src = "import compiler\n\nfn main(console: Console):\n    let bad = \"fn main(console: Console):\\n    missing(console)\\n\"\n    let fp = compiler.footprint(bad)\n    console.print(\"${fp.contains(\"error\")}\")\n    console.print(\"${fp.contains(\"missing\")}\")\n    let diff = compiler.diff(\"pub fn direct() -> Int:\\n    0\\n\", bad)\n    console.print(\"${diff.contains(\"error\")}\")\n    console.print(\"${diff.contains(\"missing\")}\")\n";
        let expected = ["true", "true", "true", "true"];
        assert_eq!(link_run(src), expected, "interp: compiler.footprint type gate");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: compiler.footprint type gate",
        );
    }

    #[test]
    fn compiler_try_doc_reports_parse_errors_as_result_on_both_backends() {
        let src = "import compiler\n\nfn via_string() -> Result(String, String):\n    let md = compiler.try_doc(\"bad\", \"fn broken( ->\")?\n    Ok(md)\n\nfn main(console: Console):\n    match compiler.try_doc(\"bad\", \"fn broken( ->\"):\n        Ok(_) -> console.print(\"unexpected ok\")\n        Err(compiler.SourceRejected(message)) -> console.print(message)\n        Err(e) -> console.print(compiler.compiler_error_message(e))\n    match via_string():\n        Ok(_) -> console.print(\"unexpected string ok\")\n        Err(e) -> console.print(e)\n    console.print(compiler.doc(\"bad\", \"fn broken( ->\"))\n";
        let expected = [
            "parse error at 1:12: expected an identifier, found `->`",
            "parse error at 1:12: expected an identifier, found `->`",
            "<!-- doc error: parse error at 1:12: expected an identifier, found `->` -->",
        ];
        assert_eq!(link_run(src), expected, "interp: compiler.try_doc error channel");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: compiler.try_doc error channel",
        );
    }

    /// `compiler.footprint` runs in the WASM backend (staged-JSON host bridge)
    /// and agrees byte-for-byte with the interpreter — a self-hosted package
    /// manager can compute footprints from inside the sandbox.
    #[test]
    fn compiler_footprint_runs_in_the_wasm_backend() {
        let prog = "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"pub fn read_all(d: Dir[Read]) -> String:\\n    d.read(\\\"x\\\")\\n\"))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].contains("Dir[Read]"), "{out:?}");
    }

    /// `compiler.diff` runs in the WASM backend and flags widening exactly as
    /// the interpreter does.
    #[test]
    fn compiler_diff_runs_in_the_wasm_backend() {
        let prog = "import compiler\nfn main(console: Console):\n    let old = \"pub fn pure(x: Int) -> Int:\\n    x\\n\"\n    let new = \"pub fn pure(x: Int, d: Dir) -> Int:\\n    x\\n\"\n    console.print(compiler.diff(old, new))\n";
        let out = wasm_run(prog);
        assert_eq!(out, link_run(prog));
        assert!(out[0].contains("\"widened\":true"), "{out:?}");
    }

    /// `compiler.footprint` exposes witchy's own capability analyzer to witchy
    /// programs (the heart of a self-hosted package manager): it returns the
    /// rights-precise footprint as JSON, which composes with `std/json`.
    #[test]
    fn compiler_footprint_exposes_the_analyzer() {
        // The rights-precise footprint comes back as JSON.
        let out = link_run(
            "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"pub fn load(d: Dir[Read]) -> String:\\n    d.read(\\\"x\\\")\\n\"))\n",
        );
        assert!(out[0].contains("\"total\":[\"Dir[Read]\"]"), "total wrong: {}", out[0]);
        assert!(out[0].contains("\"name\":\"load\""), "entry missing: {}", out[0]);
        // The output is valid JSON — it round-trips through `std/json`.
        let composed = link_run(
            "import compiler\nimport json\nfn main(console: Console):\n    match json.decode(compiler.footprint(\"pub fn serve(n: Net) -> Int:\\n    0\\n\")):\n        Ok(doc) -> console.print(\"valid\")\n        Err(e) -> console.print(\"invalid: \" + json.decode_error_message(e))\n",
        );
        assert_eq!(composed, vec!["valid"]);
        // `comptime fn` helpers are not runtime entrypoints and must not widen
        // source-string footprint reports even if their helper signature mentions
        // a capability.
        let comptime_helper = link_run(
            "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"comptime fn helper(d: Dir[Read]) -> Int:\\n    0\\n\\npub fn visible() -> Int:\\n    1\\n\"))\n",
        );
        assert!(
            !comptime_helper[0].contains("Dir[Read]") && !comptime_helper[0].contains("helper"),
            "comptime helper leaked into footprint: {}",
            comptime_helper[0]
        );
        assert!(
            comptime_helper[0].contains("\"name\":\"visible\""),
            "visible function missing: {}",
            comptime_helper[0]
        );
        // Malformed source degrades to an error object, not a crash.
        let bad = link_run(
            "import compiler\nfn main(console: Console):\n    console.print(compiler.footprint(\"fn oops(\"))\n",
        );
        assert!(bad[0].contains("\"error\""), "expected an error object: {}", bad[0]);
    }

    /// `compiler.diff` is the rights-precise block-on-widening gate (the package
    /// manager's core safety check), exposed to witchy as JSON.
    #[test]
    fn compiler_diff_is_the_widening_gate() {
        let diff = |old: &str, new: &str| -> String {
            link_run(&format!(
                "import compiler\nfn main(console: Console):\n    console.print(compiler.diff(\"{old}\", \"{new}\"))\n"
            ))
            .remove(0)
        };
        // A connect-only client that gains `Listen` is a widening (the gate blocks).
        let widen = diff(
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect, Listen]) -> Int:\\n    0\\n",
        );
        assert!(widen.contains("\"widened\":true"), "should widen: {widen}");
        assert!(widen.contains("\"added\":[\"Net[Listen]\"]"), "added wrong: {widen}");
        // The reverse (tightening to connect-only) is a safe narrowing.
        let narrow = diff(
            "pub fn f(n: Net[Connect, Listen]) -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
        );
        assert!(narrow.contains("\"widened\":false"), "should not widen: {narrow}");
        assert!(narrow.contains("\"removed\":[\"Net[Listen]\"]"), "removed wrong: {narrow}");
    }

    /// `compiler.diff` includes build-axis and user-cap axes in the JSON report
    /// so self-hosted gates can explain which axis caused a widening.
    #[test]
    fn compiler_diff_includes_build_and_user_cap_axes() {
        let diff = |old: &str, new: &str| -> String {
            link_run(&format!(
                "import compiler\nfn main(console: Console):\n    console.print(compiler.diff(\"{old}\", \"{new}\"))\n"
            ))
            .remove(0)
        };
        // A pure function gaining a build-time BuildExec is a build-axis widening.
        let build = diff(
            "pub fn pure() -> Int:\\n    0\\n",
            "pub fn pure() -> Int:\\n    0\\npub fn build(e: BuildExec) -> Int:\\n    0\\n",
        );
        assert!(build.contains("\"widened\":true"), "build widening should set widened: {build}");
        assert!(build.contains("\"build_added\":[\"BuildExec\"]"), "build_added should list BuildExec: {build}");
        assert!(build.contains("\"build_removed\":[]"), "build_removed should be empty: {build}");
        // Every diff includes all axes.
        let runtime = diff(
            "pub fn pure() -> Int:\\n    0\\n",
            "pub fn f(n: Net[Connect]) -> Int:\\n    0\\n",
        );
        assert!(runtime.contains("\"build_added\":[]"), "no-build diff should have empty build_added: {runtime}");
        assert!(runtime.contains("\"user_caps_added\":[]"), "no-user-cap diff should have empty user_caps_added: {runtime}");
        assert!(runtime.contains("\"user_caps_removed\":[]"), "no-user-cap diff should have empty user_caps_removed: {runtime}");
    }

    /// (BUG-373) The lockfile `[[rune]]` grammar has exactly ONE parser now:
    /// `toml.decode` + the structured navigation helpers. This exercises the four
    /// contract points the migration must uphold, on BOTH backends:
    ///   - two `[[rune]]` entries stay DISTINCT and in DECLARATION ORDER;
    ///   - a capability array (`runtime_footprint`) is read from the structured
    ///     table via `string_array_field`;
    ///   - a WRONG-TYPED field (`name = 42`) FAILS CLOSED with a kind error rather
    ///     than reading as empty/default;
    ///   - the entry a "verifier" checks and the entry a "resolver" enumerates are
    ///     the SAME decoded model (name+hash+caps agree entry-for-entry).
    #[test]
    fn structured_lockfile_is_the_single_rune_parser() {
        let src = r#"import toml

fn main(console: Console):
    // Two [[rune]] entries; the second repeats the `name` key shape and carries a
    // capability array. Order and repetition must be preserved.
    let lock = "[[rune]]\nname = \"money\"\nhash = \"h-money\"\nruntime_footprint = [\"Console\"]\n\n[[rune]]\nname = \"ledger\"\nhash = \"h-ledger\"\nruntime_footprint = [\"Console\", \"Dir[Read]\"]\n"
    match toml.decode(lock):
        Err(e) -> console.print("decode: " + toml.decode_error_message(e))
        Ok(doc) ->
            match toml.array_of_tables(doc, "rune"):
                Err(e) -> console.print("aot: " + toml.decode_error_message(e))
                Ok(entries) ->
                    console.print("count=${list.length(entries)}")
                    // distinct + ordered: name and hash per entry, in file order.
                    for entry in entries:
                        let name = req(entry, "name")
                        let hash = req(entry, "hash")
                        let caps = caps_of(entry)
                        console.print(name + "@" + hash + " caps=[" + caps + "]")
    // wrong-typed field fails closed (not read as empty).
    let bad = "[[rune]]\nname = 42\n"
    match toml.decode(bad):
        Err(e) -> console.print("decode: " + toml.decode_error_message(e))
        Ok(doc) ->
            match toml.array_of_tables(doc, "rune"):
                Err(e) -> console.print("aot: " + toml.decode_error_message(e))
                Ok(entries) ->
                    for entry in entries:
                        match toml.required_string(entry, "name", "a [[rune]]"):
                            Ok(s) -> console.print("wrongly accepted: " + s)
                            Err(e) -> console.print("fail-closed: " + toml.decode_error_message(e))

fn req(entry: toml.Toml, key: String) -> String:
    match toml.required_string(entry, key, "a [[rune]]"):
        Ok(s) -> s
        Err(e) -> "(err:" + toml.decode_error_message(e) + ")"

fn caps_of(entry: toml.Toml) -> String:
    match toml.string_array_field(entry, "runtime_footprint"):
        Ok(cs) -> list.join(cs, ",")
        Err(e) -> "(err:" + toml.decode_error_message(e) + ")"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "count=2",
                "money@h-money caps=[Console]",
                "ledger@h-ledger caps=[Console,Dir[Read]]",
                "fail-closed: a [[rune]]'s `name` field is not a string (found an integer)",
            ]
        );
    }

    /// `std/semver` (pure witchy) parses `major.minor.patch`, matches the `^`/`~`/
    /// exact/`>=`/`*` constraints (rights matching the Rust resolver's semver),
    /// and picks the highest satisfying version — what dependency resolution needs.
    #[test]
    fn semver_module_parses_and_matches_constraints() {
        let src = r#"import semver

fn main(console: Console):
    console.print(yes(req_matches("^1.2.0", "1.9.9")))
    console.print(yes(req_matches("^1.2.0", "2.0.0")))
    console.print(yes(req_matches("^0.4.0", "0.5.0")))
    console.print(yes(req_matches("^0", "0.9.9")))
    console.print(yes(req_matches("^0", "1.0.0")))
    console.print(yes(req_matches("^0.0", "0.0.9")))
    console.print(yes(req_matches("^0.0", "0.1.0")))
    console.print(yes(req_matches("^0.0.3", "0.0.4")))
    console.print(yes(req_matches("~1.2.0", "1.2.9")))
    console.print(yes(req_matches("~1.2.0", "1.3.0")))
    console.print(yes(req_matches("~1", "1.9.9")))
    console.print(yes(req_matches("~1", "2.0.0")))
    console.print(yes(req_matches("~1.2", "1.2.9")))
    console.print(yes(req_matches("~1.2", "1.3.0")))
    console.print(yes(req_matches(">=1.0.0", "3.0.0")))
    console.print(best_of("^1.2.0"))
    console.print(best_zero("^0"))

fn req_matches(r: String, v: String) -> Bool:
    match semver.parse_req(r):
        Ok(req) -> match semver.parse(v):
            Ok(ver) -> semver.matches(req, ver)
            Err(e) -> false
        Err(e) -> false

fn best_of(r: String) -> String:
    let vs = [semver.version(1, 0, 0), semver.version(1, 2, 0), semver.version(1, 9, 9), semver.version(2, 0, 0)]
    match semver.parse_req(r):
        Ok(req) -> match semver.best(vs, req):
            Some(v) -> semver.format(v)
            None -> "(none)"
        Err(e) -> "err"

fn best_zero(r: String) -> String:
    let vs = [semver.version(0, 0, 0), semver.version(0, 4, 2), semver.version(0, 9, 9), semver.version(1, 0, 0)]
    match semver.parse_req(r):
        Ok(req) -> match semver.best(vs, req):
            Some(v) -> semver.format(v)
            None -> "(none)"
        Err(e) -> "err"

fn yes(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "y", "n", "n", "y", "n", "y", "n", "n", "y", "n", "y", "n", "y",
                "n", "y", "1.9.9", "0.9.9"
            ]
        );
    }

    /// `witchy compile <entry> --dep name=path` (RFC-0004 §4) links the entry
    /// with an explicitly-provided dependency source — one that is NOT a sibling
    /// or std module — type-checks, and compiles to wasm. This is the surface the
    /// witchy CLI front-end drives to build a multi-rune project.
    #[test]
    fn compile_resolves_explicit_deps() {
        let base = std::env::temp_dir().join(format!("witchy_compile_dep_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let depdir = base.join("dep");
        let appdir = base.join("app");
        std::fs::create_dir_all(&depdir).unwrap();
        std::fs::create_dir_all(&appdir).unwrap();
        std::fs::write(depdir.join("mylib.witchy"), "pub fn greet() -> String:\n    \"hi\"\n").unwrap();
        let app = appdir.join("app.witchy");
        std::fs::write(
            &app,
            "import mylib\n\nfn main(console: Console):\n    console.print(mylib.greet())\n",
        )
        .unwrap();

        // Without the dep mapping, the import resolves to neither a sibling nor std.
        assert!(crate::link_file(app.to_str().unwrap()).is_err(), "no sibling/std mylib");

        // With the dep mapping, it links, type-checks, and compiles to wasm.
        let mut deps = std::collections::HashMap::new();
        deps.insert("mylib".to_string(), depdir.join("mylib.witchy"));
        let (linked, _) =
            crate::link_file_with_deps(app.to_str().unwrap(), &deps).expect("link with dep");
        crate::typeck::check(&linked).expect("typecheck");
        let bytes = crate::codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        assert!(!bytes.is_empty(), "produced a wasm binary");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn caps_diff_gate_flags_a_widening_across_versions() {
        // The supply-chain gate end-to-end: a dependency update whose public API
        // newly demands `Net` is reported as a widening (so CI/install can block),
        // while an unchanged footprint is not.
        let dir = std::env::temp_dir().join(format!("witchy_capsdiff_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let v1 = dir.join("v1.witchy");
        let v2 = dir.join("v2.witchy");
        std::fs::write(&v1, r#"
pub fn serve(console: Console) -> Int:
    0
"#).unwrap();
        std::fs::write(
            &v2,
            r#"
pub fn serve(console: Console, net: Net) -> Int:
    0
"#,
        )
        .unwrap();
        assert!(
            crate::report_capability_diff(v1.to_str().unwrap(), v2.to_str().unwrap()).unwrap(),
            "newly demanding Net must be flagged as a widening"
        );
        assert!(
            !crate::report_capability_diff(v1.to_str().unwrap(), v1.to_str().unwrap()).unwrap(),
            "an unchanged footprint must not be a widening"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
