    use crate::{ast, codegen, interpreter, parser, typeck};
    use wasmtime::{Engine, Module};

    fn validates_wasm_gc(bytes: &[u8]) -> bool {
        let features = wasmparser::WasmFeatures::default()
            | wasmparser::WasmFeatures::GC
            | wasmparser::WasmFeatures::REFERENCE_TYPES
            | wasmparser::WasmFeatures::FUNCTION_REFERENCES;
        wasmparser::Validator::new_with_features(features)
            .validate_all(bytes)
            .is_ok()
    }

    fn wasm_gc_engine() -> Engine {
        let mut config = wasmtime::Config::new();
        config.wasm_reference_types(true);
        config.wasm_function_references(true);
        config.wasm_gc(true);
        Engine::new(&config).expect("Wasm GC engine")
    }

    fn interp(src: &str) -> Vec<String> {
        link_run(src)
    }

    /// Link a single-`main` source (pulling in any imported std module) and run
    /// it on the interpreter — the path that resolves `import crypto`.
    fn link_run(src: &str) -> Vec<String> {
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        interpreter::run_module(linked, ".", Vec::new()).expect("run")
    }

    /// Resolve `src`'s `import`s against the bundled std and link the whole set —
    /// the source-level analog of the CLI's `link_file` / the lib's
    /// `resolve_std_only`. The COMPILED backend needs every reached std function
    /// present (only builtins are inlined), so a single-module `linker::link` is
    /// insufficient; the interpreter (`link_run`) tolerates it because it resolves
    /// std at run time, but `compile_module_binary` does not.
    fn resolve_std_src(src: &str) -> ast::Module {
        use std::collections::{HashSet, VecDeque};
        let entry = parser::parse_module(src).expect("parse");
        let mut modules: Vec<(String, ast::Module)> = vec![("main".to_string(), entry.clone())];
        let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
        let mut queue: VecDeque<ast::Module> = VecDeque::from([entry]);
        while let Some(module) = queue.pop_front() {
            for name in module.imports.clone() {
                if !loaded.insert(name.clone()) {
                    continue;
                }
                let source = crate::bundled_module(&name).expect("a bundled std module");
                let parsed = parser::parse_module(source).expect("parse std module");
                queue.push_back(parsed.clone());
                modules.push((name, parsed));
            }
        }
        crate::pipeline::link(modules, "main").expect("link")
    }

    /// Like [`resolve_std_src`] but RETURNS the link result (its error message on
    /// failure) instead of panicking — for tests that assert a link-time rejection
    /// (e.g. RFC-0065 sealed-construction). Parsing still panics (the source must
    /// be syntactically valid); only the final link may legitimately fail.
    fn try_link_std(src: &str) -> Result<ast::Module, String> {
        use std::collections::{HashSet, VecDeque};
        let entry = parser::parse_module(src).expect("parse");
        let mut modules: Vec<(String, ast::Module)> = vec![("main".to_string(), entry.clone())];
        let mut loaded: HashSet<String> = HashSet::from(["main".to_string()]);
        let mut queue: VecDeque<ast::Module> = VecDeque::from([entry]);
        while let Some(module) = queue.pop_front() {
            for name in module.imports.clone() {
                if !loaded.insert(name.clone()) {
                    continue;
                }
                let source = crate::bundled_module(&name).expect("a bundled std module");
                let parsed = parser::parse_module(source).expect("parse std module");
                queue.push_back(parsed.clone());
                modules.push((name, parsed));
            }
        }
        crate::pipeline::link(modules, "main").map_err(|e| e.message)
    }

    /// Link a single source as the entry module `t`, for performance-mode tests.
    fn link_mode(src: &str) -> ast::Module {
        let module = parser::parse_module(src).expect("parse");
        crate::pipeline::link(vec![("t".into(), module)], "t").expect("link")
    }

    /// Every shipped example's entry module: the source file named by its
    /// `[rune].name` manifest field (the one bearing `main`).
    /// Skips `examples/projects/` (multi-rune workspaces, covered by the pm tests)
    /// and each rune's `*_test.witchy` modules and helper modules.
    fn example_entries() -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir("examples").expect("examples directory") {
            let dir = entry.expect("dir entry").path();
            if !dir.is_dir() {
                continue;
            }
            let Some(dir_name) = dir.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if dir_name == "projects" {
                continue;
            }
            let manifest_path = dir.join("witchy.toml");
            let module_name = if manifest_path.exists() {
                let source = std::fs::read_to_string(&manifest_path).expect("read example manifest");
                let manifest: ::toml::Value = ::toml::from_str(&source).expect("parse example manifest");
                manifest
                    .get("rune")
                    .and_then(|rune| rune.get("name"))
                    .and_then(::toml::Value::as_str)
                    .expect("example manifest has [rune].name")
                    .to_string()
            } else {
                dir_name.to_string()
            };
            let entry_file = dir.join("src").join(format!("{module_name}.witchy"));
            if manifest_path.exists() {
                assert!(entry_file.exists(), "example manifest entry missing: {}", entry_file.display());
                out.push(entry_file);
            } else if entry_file.exists() {
                out.push(entry_file);
            }
        }
        out.sort();
        out
    }


    /// WebAuthn treats authenticatorData as a trust-boundary decoder input. A
    /// malformed hex flag byte must fail as malformed input before semantic flag
    /// checks can turn it into a misleading user-presence error.
    #[test]
    fn webauthn_authenticator_data_rejects_malformed_hex_before_flags_on_both_backends() {
        let src = r#"import crypto
import webauthn

fn main(console: Console):
    let rp = "example.com"
    let client = "{\"type\":\"webauthn.get\",\"challenge\":\"c\",\"origin\":\"https://example.com\"}"
    let malformed = crypto.sha256(rp) + "zz00000000"
    match webauthn.verify_assertion("00", malformed, client, "00", "c", "https://example.com", rp, true):
        Ok(v) -> console.print("ok ${v}")
        Err(e) ->
            match e:
                webauthn.AuthenticatorDataHex(_) -> console.print("typed")
                _ -> console.print("wrong")
            console.print(webauthn.assertion_error_message(e))
"#;

        let interp = link_run(src);
        assert_eq!(interp.len(), 2, "interpreter produced unexpected output: {interp:?}");
        assert_eq!(interp[0], "typed", "interpreter must expose typed malformed-authenticatorData error");
        assert!(
            interp[1].starts_with("authenticatorData is not valid hex:"),
            "interpreter must reject malformed authenticatorData as malformed hex: {interp:?}"
        );
        assert!(
            !interp[1].contains("user-presence flag not set"),
            "interpreter must not report a semantic flag error for malformed hex: {interp:?}"
        );

        let wasm = run_linked_on_wasm(&[("main", src)], "main");
        assert_eq!(wasm.len(), 2, "WASM produced unexpected output: {wasm:?}");
        assert_eq!(wasm[0], "typed", "WASM must expose typed malformed-authenticatorData error");
        assert!(
            wasm[1].starts_with("authenticatorData is not valid hex:"),
            "WASM must reject malformed authenticatorData as malformed hex: {wasm:?}"
        );
        assert!(
            !wasm[0].contains("user-presence flag not set"),
            "WASM must not report a semantic flag error for malformed hex: {wasm:?}"
        );
    }

    /// JWT/OIDC registered claims are trust-boundary inputs. Missing or
    /// wrong-shaped claims must fail as malformed payloads after the RS256
    /// signature verifies, not default into ordinary expiry/mismatch outcomes.
    #[test]
    fn jwt_registered_claims_reject_malformed_values_on_both_backends() {
        use crate::idp::RegistryKey;

        fn b64url(bytes: &[u8]) -> String {
            const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            let mut out = String::new();
            for c in bytes.chunks(3) {
                let n = ((c[0] as u32) << 16)
                    | ((*c.get(1).unwrap_or(&0) as u32) << 8)
                    | (*c.get(2).unwrap_or(&0) as u32);
                out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
                out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
                if c.len() > 1 {
                    out.push(ALPHABET[(n >> 6 & 63) as usize] as char);
                }
                if c.len() > 2 {
                    out.push(ALPHABET[(n & 63) as usize] as char);
                }
            }
            out
        }

        fn signed_jwt(key: &RegistryKey, payload_json: &str) -> String {
            let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
            let payload = b64url(payload_json.as_bytes());
            let signing_input = format!("{header}.{payload}");
            let sig = key.sign(signing_input.as_bytes()).expect("sign JWT");
            format!("{signing_input}.{}", b64url(&sig))
        }

        let dir = std::env::temp_dir().join(format!("witchy_jwt_claims_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let key = RegistryKey::load_or_create(&dir).expect("issuer key");
        let pubkey = key.public_hex();
        let missing_exp = signed_jwt(
            &key,
            r#"{"iss":"https://issuer","sub":"s","aud":"aud","iat":1}"#,
        );
        let wrong_aud = signed_jwt(
            &key,
            r#"{"iss":"https://issuer","sub":"s","aud":123,"exp":2000000000,"iat":1}"#,
        );
        let wrong_iss = signed_jwt(
            &key,
            r#"{"iss":123,"sub":"s","aud":"aud","exp":2000000000,"iat":1}"#,
        );

        let src = format!(
            r#"import jwt

fn report(console: Console, token: String):
    match jwt.verify_oidc(token, "{pubkey}", "https://issuer", "aud", 1000):
        Ok(_) -> console.print("ok")
        Err(e) ->
            match e:
                jwt.MissingClaim(name) -> console.print("missing:" + name)
                jwt.AudienceClaimExpected -> console.print("aud-shape")
                jwt.StringClaimExpected(name) -> console.print("string:" + name)
                _ -> console.print("other")
            console.print(jwt.jwt_error_message(e))

fn main(console: Console):
    report(console, "{missing_exp}")
    report(console, "{wrong_aud}")
    report(console, "{wrong_iss}")
"#
        );
        let want = [
            "missing:exp",
            "JWT payload is missing `exp`",
            "aud-shape",
            "JWT payload `aud` must be a string or array of strings",
            "string:iss",
            "JWT payload `iss` must be a string",
        ];
        assert_eq!(link_run(&src), want, "interpreter must reject malformed registered claims");
        assert_eq!(
            run_linked_on_wasm(&[("main", &src)], "main"),
            want,
            "WASM must reject malformed registered claims"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Property tests: a `for` over a random range must compute exactly the same
    /// result as a Rust reference range, on BOTH backends (so they also agree
    /// with each other) — across sign, inclusive/exclusive, empty, and `continue`.
    mod range_for_properties {
        use super::{interp, run_on_wasm};
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(96))]

            #[test]
            fn sum_matches_reference(lo in -300i64..300, len in 0i64..600, inclusive in any::<bool>()) {
                let hi = lo + len;
                let op = if inclusive { "..=" } else { ".." };
                let src = format!(
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}{op}{hi}:\n        s = s + i\n    console.print(\"${{s}}\")\n"
                );
                let reference: i64 = if inclusive { (lo..=hi).sum() } else { (lo..hi).sum() };
                let want = vec![reference.to_string()];
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm(&src), want);
            }

            #[test]
            fn continue_skipping_odds_matches_reference(lo in -100i64..100, len in 0i64..300) {
                let hi = lo + len;
                let src = format!(
                    "fn main(console: Console):\n    var s = 0\n    for i in {lo}..{hi}:\n        if i % 2 != 0:\n            continue\n        s = s + i\n    console.print(\"${{s}}\")\n"
                );
                let reference: i64 = (lo..hi).filter(|x| x % 2 == 0).sum();
                let want = vec![reference.to_string()];
                prop_assert_eq!(interp(&src), want.clone());
                prop_assert_eq!(run_on_wasm(&src), want);
            }
        }
    }

    mod stdlib_evidence;
    mod concurrency;
    mod traits;
    mod records;
    mod comptime;
    mod quote;
    mod ownership;
    mod json;
    mod capabilities;
    mod region;
    mod network;
    mod iter;
    mod reflection;
    mod keyword_args;
    mod tailcalls;
    mod bytes;
    mod crypto;
    mod toml;
    mod bigint;
    mod pm_coven;
    mod capability_narrowing;
    mod cli_std_modules;
    mod std_result_list_combinators;
    mod record_match_integration;
    mod func_values;
    mod rc_corpus;
    mod crypto_jwt_oauth;
    mod glamour;
    mod rfc0046_dispatch;
    mod gen_async;
    mod closures;
    mod collections;
    mod match_control;
    mod mutation;
    mod structs;
    mod strings;
    mod traits_generics;
    mod misc_semantics;
    mod syntax_forms;
    mod string_methods;
    mod container_places;
    mod regex_module;
    mod interpolation;
    mod try_result;
    mod abort_contract;
    mod duration_prng;
    mod equality;
    mod math_float;
    mod async_channels;
    mod sandbox_vm;
    mod wir_binary;
    mod compiler_footprint;
    mod example_sweeps;
    mod examples_programs;
    mod modes;

    fn wasm_run(src: &str) -> Vec<String> {
        witchy_interp::compiler_natives::install();
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        crate::run_wasm_bytes(&bytes).expect("wasm run")
    }

    /// `wasm_run` that also reads the exported `__witchy_reowns` counter —
    /// the timing-free proof of whether accumulation ran in place (O(1)
    /// re-owns) or fell to the copying path (O(n) re-owns).
    fn wasm_run_reowns(src: &str) -> (Vec<String>, i64) {
        use crate::runtime::{Capabilities, Runtime};
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        let reowns = actor.reowns().unwrap_or(0);
        (actor.output(), reowns)
    }

    /// `wasm_run` that also reads the `__heap` frontier and `__rc_reused_bytes` —
    /// the timing-free proof of whether the RC floor bounded the heap (flat frontier)
    /// by recycling freed blocks (reused > 0), rather than leaking O(iterations).
    fn wasm_run_heap(src: &str) -> (Vec<String>, i64, i64) {
        use crate::runtime::{Capabilities, Runtime};
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        let heap = actor.heap_bytes().unwrap_or(0);
        let reused = actor.rc_reused_bytes().unwrap_or(0);
        (actor.output(), heap, reused)
    }

    // ---- RFC-0052: one pattern grammar ------------------------------------


    // HEAP-TYPE MATRIX (RFC-0035 step 3 gate). Corpus 1-3 above only exercised RECORD/ADT
    // elements — the false confidence that let the reverted emission ship (5e9e167): it
    // assumed every i32 element was an offset-0 rc_alloc object, which was FALSE for the
    // header-less strings/lists/dicts from the direct-bump helpers. Phase A now routes every
    // value producer through $rc_alloc, so these element types are all headered. This matrix
    // is the gate that would have caught the revert: each element type the revert corrupted,
    // read-past-set_at / aliased / stored / match-on-read, must stay byte-identical across
    // interp == wasm == wasm(rc-floor). Authored FIRST, before re-applying the dup/drop
    // emission — a premature/wrong dec flips one of these red.


    /// `std/path` does pure '/'-path surgery: base/dir/ext/stem, join (an absolute
    /// right-hand side replaces), and normalize (collapsing `.`/`..`, never
    /// escaping an absolute root, keeping leading `..` when relative).
    #[test]
    fn path_module_components_and_normalize() {
        let src = r#"import path
import option

fn main(console: Console):
    console.print(path.base("a/b/c.txt") + "|" + (path.dir("a/b/c.txt") ?? "<none>"))
    console.print((path.ext("a/b.tar.gz") ?? "<none>") + "|" + path.stem("a/b.tar.gz"))
    console.print("[" + (path.ext(".bashrc") ?? "<none>") + "]|" + path.base("a/b/"))
    console.print((path.dir("c") ?? "<none>") + "|" + (path.ext("README") ?? "<none>"))
    console.print(path.join("a/b", "c") + "|" + path.join("a", "/x"))
    console.print(path.normalize("a/./b/../c/") + "|" + path.normalize("/a/b/../../../x"))
    console.print(path.normalize("../a/../../b"))
"#;
        assert_eq!(
            link_run(src),
            vec![
                "c.txt|a/b",
                "gz|b.tar",
                "[<none>]|b",
                "<none>|<none>",
                "a/b/c|/x",
                "a/c|/x",
                "../../b",
            ]
        );
    }

    /// Pull the contents of every fenced block tagged exactly `witchy`.
    fn extract_witchy_blocks(markdown: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut current: Option<String> = None;
        for line in markdown.lines() {
            match &mut current {
                None if line.trim_end() == "```witchy" => current = Some(String::new()),
                Some(body) => {
                    if line.trim_end() == "```" {
                        blocks.push(current.take().unwrap());
                    } else {
                        body.push_str(line);
                        body.push('\n');
                    }
                }
                None => {}
            }
        }
        blocks
    }

    /// The Markdown docs whose ```` ```witchy ```` blocks are validated + classified: the
    /// root docs, `spec/`, and `book/src/` (sorted for a stable manifest). Shared by
    /// `documentation_examples_are_valid` and the manifest generator so both walk one list.
    fn doc_markdown_files() -> Vec<std::path::PathBuf> {
        let mut files: Vec<std::path::PathBuf> = vec![
            "README.md".into(),
            "CONTRIBUTING.md".into(),
            "examples/README.md".into(),
        ];
        for dir in ["spec", "book/src"] {
            let Ok(entries) = std::fs::read_dir(dir) else { continue };
            let mut md: Vec<_> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
                .collect();
            md.sort();
            files.extend(md);
        }
        files
    }

    /// (RFC-0041 Phase 3) Generate the runnable-example classification manifest as pretty
    /// JSON. Reuses the SAME classifier as `documentation_examples_are_valid`: per doc
    /// `witchy` block, whether it is `runnable` (a `Console`-only `main`, no actor/argv) and
    /// `console_only`, its capability `footprint`, and — for runnable blocks — the interpreter
    /// `output`. This is the single source of truth the runnable book reads, so the browser
    /// never re-derives classification (which could disagree with the authoritative Rust one).
    fn generate_examples_manifest() -> String {
        let files = doc_markdown_files();
        let per_file: Vec<Vec<serde_json::Value>> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|file| {
                s.spawn(move || {
                    let mut file_entries = Vec::new();
                    let Ok(text) = std::fs::read_to_string(file) else { return file_entries };
                    for (idx, snippet) in extract_witchy_blocks(&text).into_iter().enumerate() {
                        let context = format!("{}: ```witchy block #{}", file.display(), idx + 1);
                        let module = parser::parse_module(&snippet)
                            .unwrap_or_else(|e| panic!("{context} fails to parse: {e:?}"));
                        let mut footprint_module = module.clone();
                        crate::comptime::expand("main", &mut footprint_module)
                            .unwrap_or_else(|e| panic!("{context} fails compile-time expansion: {e}"));
                        let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
                            .unwrap_or_else(|e| panic!("{context} fails to link: {e}"));
                        typeck::check(&linked).unwrap_or_else(|e| panic!("{context} fails to type-check: {e}"));

                        let has_main = linked
                            .items
                            .iter()
                            .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
                        let reads_argv = linked.items.iter().any(|it| {
                            matches!(it, ast::Item::Function(f) if f.name == "main"
                                && f.params.iter().any(|p| matches!(&p.ty,
                                    Some(ast::Type::Named(n, args)) if n == "List"
                                        && matches!(args.first(),
                                            Some(ast::Type::Named(s, _)) if s == "String"))))
                        });
                        let fp = crate::capabilities::analyze(&footprint_module);
                        let console_only = fp.total.keys().all(|k| *k == "Console");
                        let uses_workers = footprint_module.imports.iter().any(|m| m == "vm")
                            || linked.imports.iter().any(|m| m == "vm");
                        let runnable = has_main && console_only && !reads_argv && !uses_workers;
                        // (RFC-0091) `browser_runnable`: whether the OPT-IN playground host
                        // (`instantiate(..., { capabilities })` in web/witchy-runtime) can run
                        // this block. That host backs exactly the browser-honest capability
                        // families — `Console`, `Clock` (real wall/monotonic time), `Env` (an
                        // empty/page-supplied map), and `Dir` (a per-run in-memory tree) — and
                        // leaves `Net`/`Exec`/`Secret`/argv/workers denied by omission. This is a
                        // SUPERSET of `runnable` (which stays Console-only + output-pinned): a
                        // `browser_runnable`-but-not-`runnable` block has NO pinned `output`,
                        // because its result depends on real time or on host-supplied Dir/Env
                        // fixtures rather than a deterministic oracle run. The docs cell uses
                        // this to offer a Run button (empty fixtures) without claiming a golden.
                        const BROWSER_CAP_FAMILIES: &[&str] = &["Console", "Clock", "Env", "Dir"];
                        let browser_caps_ok = fp
                            .total
                            .keys()
                            .all(|k| BROWSER_CAP_FAMILIES.iter().any(|f| f == k));
                        let browser_runnable =
                            has_main && browser_caps_ok && !reads_argv && !uses_workers;
                        let footprint: Vec<String> = fp
                            .total
                            .iter()
                            .map(|(cap, rights)| {
                                if rights.is_empty() {
                                    (*cap).to_string()
                                } else {
                                    format!("{}[{}]", cap, rights.iter().copied().collect::<Vec<_>>().join(","))
                                }
                            })
                            .collect();
                        let output: Vec<String> = if runnable {
                            interpreter::run_module(linked, std::path::Path::new("."), Vec::new())
                                .unwrap_or_else(|e| panic!("{context} fails on the interpreter: {e}"))
                        } else {
                            Vec::new()
                        };
                        file_entries.push(serde_json::json!({
                            "file": file.display().to_string(),
                            "block": idx + 1,
                            "runnable": runnable,
                            "browser_runnable": browser_runnable,
                            "console_only": console_only,
                            "expect_error": false,
                            "footprint": footprint,
                            "output": output,
                        }));
                    }
                    file_entries
                })
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let entries: Vec<serde_json::Value> = per_file.into_iter().flatten().collect();
        serde_json::to_string_pretty(&serde_json::Value::Array(entries)).unwrap() + "\n"
    }

    /// `fs.collect_files(root, "", "", ext)` walks from the Dir root itself:
    /// root-level files are collected with bare relative paths (never "/name"),
    /// and root-level directories recurse instead of being silently skipped by
    /// the confinement resolver rejecting absolute-looking paths.
    #[test]
    fn fs_collect_files_walks_from_dir_root() {
        let dir = std::env::temp_dir().join(format!("witchy-collect-root-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("top.witchy"), "// top").unwrap();
        std::fs::write(dir.join("sub/nested.witchy"), "// nested").unwrap();
        std::fs::write(dir.join("skip.txt"), "no").unwrap();
        let src = r#"import fs
import list

fn main(console: Console, root: Dir):
    var names = []
    for f in fs.collect_files(root, "", "", ".witchy"):
        let (rel, _contents) = f
        list.push(names, rel)
    list.sort(names)
    console.print(list.join(names, ","))
"#;
        let linked = resolve_std_src(src);
        let interpreted =
            interpreter::run_module(linked, dir.to_str().unwrap(), Vec::new()).expect("interp run");
        assert_eq!(interpreted, vec!["sub/nested.witchy,top.witchy"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `std/time` computes the civil UTC date from a unix timestamp (Hinnant's
    /// days<->civil algorithm), cross-checked against Python's datetime: leap
    /// years, weekday, an exact round-trip, and a pre-1970 timestamp (floor
    /// division, so the day is right).
    #[test]
    fn time_module_civil_date_from_unix() {
        let src = r#"import time

fn main(console: Console):
    console.print(time.iso8601(time.from_unix(1780000000)))
    console.print(time.weekday_name(time.from_unix(0)) + " " + time.iso8601(time.from_unix(0)))
    console.print(time.iso8601(time.from_unix(-86401)))
    console.print(yn(time.is_leap(2000)) + yn(time.is_leap(1900)) + yn(time.is_leap(2024)))
    console.print(yn(time.to_unix(time.from_unix(1780000000)) == 1780000000))

fn yn(b: Bool) -> String:
    if b: "y" else: "n"
"#;
        assert_eq!(
            link_run(src),
            vec![
                "2026-05-28T20:26:40Z",
                "Thursday 1970-01-01T00:00:00Z",
                "1969-12-30T23:59:59Z",
                "yny",
                "y",
            ]
        );
    }

    /// `std/fs` parent_dir + (with a real Dir) the recursive collect — exercised
    /// here for the pure part to confirm the module's functions resolve on import.
    #[test]
    fn fs_module_parent_dir_resolves() {
        let src = "import fs\nimport option\nfn main(console: Console):\n    console.print(fs.parent_dir(\"a/b/c\") ?? \"<none>\")\n    console.print(fs.parent_dir(\"top\") ?? \"<none>\")\n";
        assert_eq!(link_run(src), vec!["a/b", "<none>"]);
    }

    /// The oracle-based sweep run under an OPT-IN codegen lever: EVERY example — including the
    /// capability-using ones (coven_check's `Dir[Read]`, networking, …) — must still agree
    /// interpreter-vs-compiled with the lever on. The default `every_compilable_example_*` sweep
    /// only exercises the DEFAULT-ON opts, so an opt-in lever's frees / layout changes go
    /// unexercised on real programs — exactly how the free-at-overwrite alias-init use-after-free
    /// hid for ~2 days (it only fired under `WITCHY_OPT=rc-floor`, which no sweep ran). Every
    /// opt-in lever gets a sweep here so that class cannot recur. `set_for_tests` is thread-local,
    /// so the lever is isolated from the parallel test threads.
    fn assert_examples_agree_under(set: crate::opt::OptSet, lever: &str) {
        let entries = example_entries();
        let diverged: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    crate::opt::set_for_tests(Some(set));
                    let p = path.to_str().unwrap();
                    let result = if let crate::ParityOutcome::Diverge { message, .. } = crate::parity_check(p) {
                        Some(message)
                    } else {
                        None
                    };
                    crate::opt::set_for_tests(None);
                    result
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            diverged.is_empty(),
            "examples diverge under WITCHY_OPT={lever} (a codegen bug the default sweep misses):\n{}",
            diverged.join("\n")
        );
    }

    fn assert_fn_compiles(src: &str) {
        assert!(typeck::check_str(src).is_ok(), "{:?}", typeck::check_str(src));
        let module = parser::parse_module(src).expect("parse");
        let bytes = codegen::compile_module_binary(&module)
            .expect_lowered("the binary path lowers this program");
        Module::new(&wasm_gc_engine(), &bytes).expect("valid wasm");
    }

    /// End-to-end through the *compiled* path: type-check, compile to WASM, run
    /// on the wasmtime runtime with the output capabilities granted, and return
    /// what the program printed.
    fn run_on_wasm(src: &str) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let linked = resolve_std_src(src);
        typeck::check(&linked).expect("typecheck");
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                // Mirror the interpreter's automatic grants (output + the
                // read-only ambient Clock/Env), like `run_wasm_bytes`.
                Capabilities {
                    print: true,
                    print_int: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }

    /// Run a WIR-assembled binary with ONLY `print` granted — nothing else. If
    /// the module imported any other authority, instantiate would fail. Proves
    /// the pruned WIR-helper path emits capability-minimal modules.
    fn run_bytes_print_only(bytes: &[u8]) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                bytes,
                Capabilities { print: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn under a print-only grant");
        actor.run().expect("run");
        actor.output()
    }

    /// Run a WIR-assembled binary with EVERY capability granted. The static
    /// prelude is "all features on", so a raw-body-path module imports the full
    /// host surface; granting everything lets it instantiate. (The pruned
    /// WIR-helper path emits capability-minimal modules — see `run_bytes_print_only`.)
    fn run_bytes_all_caps(bytes: &[u8]) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    quiet: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
                    signing_key: Some([0u8; 32]),
                    build_out: Some(std::env::temp_dir()),
                    build_read_roots: vec![std::path::PathBuf::from(".")],
                    ..Default::default()
                },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }


    /// Run a WIR-binary under a print-only grant and read the exported
    /// `__witchy_reowns` counter — the timing-free proof of in-place (O(1)) vs
    /// copying (O(n)) accumulation.
    fn binary_run_reowns(bytes: &[u8]) -> (Vec<String>, i64) {
        use crate::runtime::{Capabilities, Runtime};
        let mut rt = Runtime::batch().expect("runtime");
        let mut actor = rt
            .spawn(
                bytes,
                Capabilities { print: true, quiet: true, ..Default::default() },
                crate::RUN_MEMORY_PAGES,
            )
            .expect("spawn");
        actor.run().expect("run");
        let reowns = actor.reowns().unwrap_or(0);
        (actor.output(), reowns)
    }

    /// Link a multi-module program, compile the flat module to WASM, run it on
    /// the runtime with output capabilities, and return what it printed.
    fn run_linked_on_wasm(sources: &[(&str, &str)], entry: &str) -> Vec<String> {
        witchy_interp::compiler_natives::install();
        use crate::runtime::{Capabilities, Runtime};
        let mods: Vec<(String, ast::Module)> = sources
            .iter()
            .map(|(n, s)| ((*n).to_string(), parser::parse_module(s).expect("parse")))
            .collect();
        let linked = crate::pipeline::link(mods, entry).expect("link");
        assert!(typeck::check(&linked).is_ok(), "{:?}", typeck::check(&linked));
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                // Mirror the interpreter's automatic grants (output + the
                // read-only ambient Clock/Env), like `run_wasm_bytes`.
                Capabilities {
                    print: true,
                    print_int: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(Vec::new()),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }


    /// Link + run a single-`main` source on the interpreter with a `Net` allowlist grant.
    fn link_run_net(src: &str, net_allow: &[&str]) -> Vec<String> {
        let module = parser::parse_module(src).expect("parse");
        let linked = crate::pipeline::link(vec![("main".into(), module)], "main").expect("link");
        typeck::check(&linked).expect("typecheck");
        interpreter::run_module(linked, ".", net_allow.iter().map(|s| s.to_string()).collect())
            .expect("run")
    }


    /// Like `run_linked_on_wasm` but with an explicit `Net` allowlist grant, for
    /// programs that `restrict`/`connect` to specific addresses.
    fn run_linked_on_wasm_net(sources: &[(&str, &str)], entry: &str, net_allow: &[&str]) -> Vec<String> {
        use crate::runtime::{Capabilities, Runtime};
        let mods: Vec<(String, ast::Module)> = sources
            .iter()
            .map(|(n, s)| ((*n).to_string(), parser::parse_module(s).expect("parse")))
            .collect();
        let linked = crate::pipeline::link(mods, entry).expect("link");
        assert!(typeck::check(&linked).is_ok(), "{:?}", typeck::check(&linked));
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(
                &bytes,
                Capabilities {
                    print: true,
                    print_int: true,
                    clock: true,
                    env: true,
                    dir_root: Some(std::path::PathBuf::from(".")),
                    dir_read: true,
                    dir_write: true,
                    net_allow: Some(net_allow.iter().map(|s| (*s).to_string()).collect()),
                    net_connect: true,
                    net_listen: true,
                    ..Default::default()
                },
                4,
            )
            .expect("spawn");
        actor.run().expect("run");
        actor.output()
    }





    /// The glamour rune's source, embedded so tests can `import glamour` without a
    /// sibling file on disk — the same trick `coven`'s server modules use.
    const GLAMOUR_SRC: &str = include_str!("../projects/glamour/src/glamour.witchy");


    /// `std/markdown`'s source, embedded so a test can `import markdown` (it `import glamour`
    /// transitively) without sibling files on disk.
    const MARKDOWN_SRC: &str = include_str!("../projects/glamour/src/markdown.witchy");


    // ---- RFC-0043: write-back by declaration (not the name census) ----

    // ---- RFC-0087: uniform var write-back ----

    /// REGRESSION (BUG-408/BUG-250): `jwt.rsa_key_from_jwk` rejects an empty JWK (it
    /// used to return an `Ok` bogus key), and `verify_oidc` pins the header `alg` to
    /// RS256 — an `alg: none` token is refused before the signature is even checked
    /// (algorithm-confusion defense, fail closed). Identical on both backends.
    #[test]
    fn jwt_rejects_empty_jwk_and_non_rs256_alg_backends_agree() {
        let src = "import jwt\nfn tag(r: Result(String, jwt.JwtError)) -> String:\n    match r:\n        Ok(_k) -> \"ok\"\n        Err(_e) -> \"err\"\nfn main(console: Console):\n    console.print(tag(jwt.rsa_key_from_jwk(\"\", \"\")))\n    let token = \"eyJhbGciOiJub25lIn0.eyJpc3MiOiJpIiwiYXVkIjoiYSIsImV4cCI6OTk5OTk5OTk5OX0.AAAA\"\n    match jwt.verify_oidc(token, \"00\", \"i\", \"a\", 0):\n        Ok(_c) -> console.print(\"accepted\")\n        Err(e) -> console.print(jwt.jwt_error_message(e))\n";
        let expected = ["err", "JWT `alg` is `none`, not the required `RS256`"];
        let linked = resolve_std_src(src);
        assert_eq!(
            interpreter::run_module(linked, ".", Vec::new()).expect("interp"),
            expected,
            "interp"
        );
        assert_eq!(wasm_run(src), expected, "wasm");
    }
