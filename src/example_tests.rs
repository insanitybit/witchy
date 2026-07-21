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

    #[test]
    fn public_sources_do_not_call_legacy_render_intrinsic() {
        fn collect(root: &std::path::Path, suffix: &str, out: &mut Vec<std::path::PathBuf>) {
            if root.is_file() {
                if root.to_string_lossy().ends_with(suffix) {
                    out.push(root.to_path_buf());
                }
                return;
            }
            for entry in std::fs::read_dir(root).unwrap_or_else(|_| panic!("read {}", root.display())) {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    collect(&path, suffix, out);
                } else if path.to_string_lossy().ends_with(suffix) {
                    out.push(path);
                }
            }
        }

        let mut paths = Vec::new();
        for root in ["std", "examples"] {
            collect(std::path::Path::new(root), ".witchy", &mut paths);
        }
        for root in ["README.md", "book", "spec", "rfcs/performance-modes.md"] {
            collect(std::path::Path::new(root), ".md", &mut paths);
        }
        paths.sort();

        let mut offenders = Vec::new();
        for path in paths {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("read {}", path.display()));
            if text.contains("__render(") {
                offenders.push(path.display().to_string());
            }
        }
        assert!(
            offenders.is_empty(),
            "public source/docs must use interpolation or show.render, not __render:\n{}",
            offenders.join("\n")
        );
    }

    #[test]
    fn generic_type_aliases_resolve_on_linked_path() {
        // BUG-563: parameterized aliases were accepted at the declaration site
        // but every `Pair(Int)` use reached type resolution as an unknown type.
        let src = "type Pair(a) = (a, a)\ntype Rows(a) = List(Pair(a))\n\nfn first(p: Pair(Int)) -> Int:\n    p.0\n\nfn main(console: Console):\n    let rows: Rows(String) = [(\"a\", \"b\")]\n    console.print(\"${first((1, 2))}:${list.length(rows)}\")\n";
        assert_eq!(link_run(src), vec!["1:1"]);
    }


    /// (BUG-546) Sealed domain values display through their public canonical
    /// formatter, not their private constructor-shaped representation.
    #[test]
    fn sealed_domain_values_use_canonical_show_on_both_backends() {
        let src = "import show\nimport semver\nimport url\nimport time\n\nfn main(console: Console):\n    let v = semver.version(1, 2, 3)\n    let d = time.from_unix(0)\n    match url.parse(\"https://example.com/p\"):\n        Ok(u) ->\n            show.say(console, v)\n            show.say(console, u)\n            show.say(console, d)\n            console.print(\"${v}\")\n            console.print(\"${u}\")\n            console.print(\"${d}\")\n            console.print(show.render([v, semver.version(2, 0, 0)]))\n            console.print(show.render(Some(u)))\n        Err(e) -> console.print(url.url_error_message(e))\n";
        let expected = [
            "1.2.3",
            "https://example.com/p",
            "1970-01-01T00:00:00Z",
            "1.2.3",
            "https://example.com/p",
            "1970-01-01T00:00:00Z",
            "[1.2.3, 2.0.0]",
            "Some(https://example.com/p)",
        ];
        assert_eq!(link_run(src), expected, "interp: sealed domain Show");
        assert_eq!(
            run_linked_on_wasm(&[("main", src)], "main"),
            expected,
            "compiled: sealed domain Show",
        );
    }

    /// Type heads are not runtime values. The resolver may keep ambient type
    /// names bare (`Int`, `Set`, `Tuple2`, a local sum type name), but type
    /// checking must reject them before codegen instead of letting them look like
    /// unknown constructors with fresh result types.
    #[test]
    fn type_names_are_rejected_as_values_after_linking() {
        let cases = [
            (
                "builtin constructor-looking call",
                "fn main(console: Console):\n    Int(1)\n    console.print(\"bad\")\n",
                "type `Int` is not a value",
            ),
            (
                "prelude type",
                "fn main(console: Console):\n    Result\n    console.print(\"bad\")\n",
                "type `Result` is not a value",
            ),
            (
                "synthetic tuple type",
                "fn main(console: Console):\n    Tuple2(1, 2)\n    console.print(\"bad\")\n",
                "type `Tuple2` is not a value",
            ),
            (
                "local sum type name",
                "type Color:\n    Red\n    Blue\n\nfn main(console: Console):\n    Color\n    console.print(\"bad\")\n",
                "type `Color` is not a value",
            ),
        ];

        for (label, src, want) in cases {
            let module = parser::parse_module(src).expect(label);
            let linked = crate::pipeline::link(vec![("main".into(), module)], "main")
                .unwrap_or_else(|e| panic!("{label}: link failed: {}", e.message));
            let err = typeck::check(&linked).expect_err(label);
            assert!(
                err.message.contains(want),
                "{label}: expected `{want}`, got `{}`",
                err.message
            );
        }
    }

    /// (BUG-216) A local binding with the same name as a prelude/imported module
    /// owns dotted calls consistently. `string.to_upper("x")` below must dispatch
    /// to the local `S` method, not silently escape to std String.to_upper.
    #[test]
    fn shadowing_module_name_keeps_dotted_calls_on_local() {
        let src = "type S:\n    x: String\n\nimpl S:\n    fn to_upper(self: S, suffix: String) -> String:\n        self.x + suffix\n\nfn module_upper(s: String) -> String:\n    s.to_upper()\n\nfn main(console: Console):\n    let string = S(\"s\")\n    console.print(string.to_upper(\"x\"))\n    console.print(module_upper(\"y\"))\n";
        let expected = ["sx", "Y"];
        assert_eq!(link_run(src), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
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

    /// (SEC-045) An overflowing `Content-Length` must NOT crash the server. The old
    /// `content_length` guarded with `ascii.all_digits` (which passes an arbitrarily
    /// long digit string) then called `string.to_int`, which TRAPS on i64 overflow —
    /// an unauthenticated remote crash. The fix parses totally with `string.parse_int`
    /// (returns None on overflow) and treats a rejected value as no body (0). This
    /// mirrors `server.content_length` and must agree + not trap on both backends.
    #[test]
    fn overflowing_content_length_does_not_trap_on_either_backend() {
        // `ascii.all_digits` accepts the overflowing string (the old trap trigger),
        // but the total parse yields 0 (no body) rather than aborting the VM.
        let src = "import ascii\nimport option\n\n\
                   fn content_length_val(v: String) -> Int:\n\
                   \x20   match v.parse_int():\n\
                   \x20       Some(n) -> if n > 0: n else: 0\n\
                   \x20       None -> 0\n\n\
                   fn main(console: Console):\n\
                   \x20   let big = \"99999999999999999999999999\"\n\
                   \x20   console.print(\"${ascii.all_digits(big)}\")\n\
                   \x20   console.print(\"${content_length_val(big)}\")\n\
                   \x20   console.print(\"${content_length_val(\"42\")}\")\n\
                   \x20   console.print(\"${content_length_val(\"abc\")}\")\n";
        let want = ["true", "0", "42", "0"];
        assert_eq!(link_run(src), want, "interp: overflow -> 0, valid -> value, junk -> 0");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), want, "wasm must agree and not trap");
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

    /// (BUG-335, spec §16) `main` may return only Nil/Int/Float; a String/Bool/List
    /// return is a CHECK-TIME error (the interpreter echoes it but the compiled run
    /// wrapper drops it — a silent divergence, now rejected loud by construction).
    #[test]
    fn off_spec_main_return_is_check_error() {
        assert!(typeck::check_str("fn main(console: Console) -> String:\n    \"oops\"\n").is_err(), "String main rejected");
        assert!(typeck::check_str("fn main(console: Console) -> Bool:\n    true\n").is_err(), "Bool main rejected");
        assert!(typeck::check_str("fn main(console: Console) -> List(Int):\n    [1, 2]\n").is_err(), "List main rejected");
        typeck::check_str("fn main(console: Console) -> Int:\n    0\n").expect("Int main ok");
        typeck::check_str("fn main(console: Console) -> Float:\n    2.5\n").expect("Float main ok");
        typeck::check_str("fn main(console: Console):\n    console.print(\"hi\")\n").expect("Nil main ok");
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

    /// RFC-0006 regression: an IMPORTED tag used inside a NON-`main` function
    /// expands and runs identically on both backends. This locks in the
    /// infinite-recursion fix in `tagged::expand`: to RUN a tag the compiler links
    /// a synthetic comptime program, and `linker::link` re-runs `tagged::expand`
    /// per module — so if the comptime program still carried the CONSUMER's
    /// tag-bearing function (`render`, with its unexpanded `box"…"`), expansion
    /// would loop forever (rebuild the program → expand the tag again → …) and
    /// overflow the stack. The fix prunes the comptime program to only the items
    /// REACHABLE FROM THE TAG (its callees + the types they name), which excludes
    /// `render`/`main`, so the program holds no tagged literals and terminates.
    /// Shape mirrors the glamour `html"…"`-in-`view` case that triggered the bug.
    #[test]
    fn imported_tag_in_non_main_fn_agrees_on_both_backends() {
        use crate::runtime::{Capabilities, Runtime};
        // The tag-defining module: a tiny `box"…"` tag that emits source wrapping
        // each hole in `widget.unwrap(widget.wrap(…))`. The `Wrapped` type + `wrap`/`unwrap`
        // helpers exercise the reachable-TYPES half of the prune (the tag's
        // signature/body reach `Wrapped`, so it must be kept for the comptime
        // program to type-check), and prove a tag works when defined in an
        // IMPORTED rune, not just locally.
        let widget = "type Wrapped:\n    Wrap(String)\n\npub fn unwrap(w: Wrapped) -> String:\n    match w:\n        Wrap(s) -> s\n\npub fn wrap(s: String) -> Wrapped:\n    Wrap(s)\n\npub fn box(parts: List(String), holes: List(String)) -> String:\n    var out = \"widget.unwrap(widget.wrap(\\\"\"\n    var i = 0\n    let n = list.length(parts)\n    for p in parts:\n        out = out + p\n        if i < n - 1:\n            out = out + \"\\\" + \" + list.at(holes, i) + \" + \\\"\"\n        i = i + 1\n    out + \"\\\"))\"\n";
        // The CONSUMER: the tag appears in `render`, a NON-`main` function. This is
        // the exact shape that recursed before the fix (cf. glamour's `view`).
        let app = "import widget\n\nfn render(x: String) -> String:\n    box\"[${x}]\"\n\nfn main(console: Console):\n    console.print(render(\"hi\"))\n";

        let want = vec!["[hi]".to_string()];
        let link = || {
            let app_m = parser::parse_module(app).expect("parse app");
            let widget_m = parser::parse_module(widget).expect("parse widget");
            crate::pipeline::link(
                vec![("main".into(), app_m), ("widget".into(), widget_m)],
                "main",
            )
            .expect("link (must not overflow the stack)")
        };

        let linked = link();
        typeck::check(&linked).expect("typecheck");
        let interp_out = interpreter::run_module(linked, ".", Vec::new()).expect("interp run");
        assert_eq!(interp_out, want, "interpreter");

        let linked = link();
        let bytes = codegen::compile_module_binary(&linked)
            .expect_lowered("the binary path lowers this program");
        let mut rt = Runtime::new().expect("runtime");
        let mut actor = rt
            .spawn(&bytes, Capabilities { print: true, ..Default::default() }, 4)
            .expect("spawn");
        actor.run().expect("run");
        assert_eq!(actor.output(), want, "compiled WASM must agree");
    }

    /// An early `return` inside an `var` function must agree on both backends.
    /// An var function yields multiple results (the declared return plus one per
    /// var param), so an early return reproduces that epilogue: it pushes each
    /// var param's current value before returning. (Regression for the
    /// interpreter-only return-in-var gap.)
    #[test]
    fn return_in_var_fn_agrees_on_both_backends() {
        let src = "fn clamp(var n: Int):\n    if (n > 10):\n        n = 10\n        return\n    n = n + 1\n\nfn main(console: Console):\n    var a = 5\n    clamp(a)\n    console.print(\"${a}\")\n    var b = 50\n    clamp(b)\n    console.print(\"${b}\")\n";
        let want = vec!["6".to_string(), "10".to_string()];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
    }

    /// `float_to_int` on infinities or out-of-range finite values must saturate
    /// the same way on both backends. NaN is deliberately excluded here: BUG-466
    /// makes NaN a loud contract error, covered by `math_to_int_nan_aborts_on_both_backends`.
    #[test]
    fn wasm_float_to_int_saturates_like_the_interpreter() {
        let src = "fn main(console: Console):\n    console.print(\"${math.to_int(1.0 / 0.0)}\")\n    console.print(\"${math.to_int(0.0 - 1.0 / 0.0)}\")\n    console.print(\"${math.to_int(0.0 - 3.9)}\")\n";
        let want = vec![
            "9223372036854775807".to_string(),
            "-9223372036854775808".to_string(),
            "-3".to_string(),
        ];
        assert_eq!(interp(src), want.clone(), "interpreter");
        assert_eq!(run_on_wasm(src), want, "compiled WASM must agree");
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

    /// `clock.now_monotonic()` yields monotonic elapsed nanoseconds — a steady
    /// clock for measuring durations (used by the benchmark harness to time the
    /// compute kernel, excluding process startup). The absolute value is
    /// nondeterministic, so parity is asserted on a *derived* property (elapsed is
    /// non-negative and the kernel result is identical) that both backends agree on.
    #[test]
    fn now_monotonic_measures_elapsed_on_both_backends() {
        let src = "fn spin(n: Int) -> Int:\n    var a = 0\n    var i = 0\n    while i < n:\n        a = a + i\n        i = i + 1\n    a\n\nfn main(console: Console, clock: Clock):\n    let t0 = clock.now_monotonic()\n    let r = spin(1000)\n    let t1 = clock.now_monotonic()\n    console.print(\"${r}\")\n    console.print(\"${t1 - t0 >= 0}\")\n";
        let expected = vec!["499500".to_string(), "true".to_string()];
        assert_eq!(interpreter::run(src).expect("interp"), expected, "interp");
        assert_eq!(run_linked_on_wasm(&[("main", src)], "main"), expected, "wasm");
        // Like `now`, it needs a Clock — another capability is a type error.
        assert!(typeck::check_str("fn main(c: Console):\n    let t = now_monotonic(c)\n").is_err());
        // The Clock requirement surfaces in the capability footprint.
        let fp = crate::capabilities::analyze(
            &parser::parse_module(
                "fn main(console: Console, clock: Clock):\n    let t = clock.now_monotonic()\n",
            )
            .expect("parse"),
        );
        assert!(fp.total.contains_key("Clock"), "Clock should appear in the footprint");
    }

    /// The reference interpreter and the compiled WASM backend must produce the
    /// same output for the same program — the core promise of witchy's two-tier
    /// design. This differential test exercises a spread of features and asserts
    /// agreement directly (no hardcoded expectations), so a future codegen change
    /// that silently diverges from the interpreter is caught. Programs stay
    /// within the compiled backend's supported semantics (notably 32-bit Int).
    #[test]
    fn interpreter_and_compiled_backends_agree() {
        let programs: &[(&str, &str)] = &[
            (
                "arithmetic + control flow",
                r#"
fn main(console: Console):
    var acc = 0
    var i = 0
    while (i < 12):
        if ((i % 2) == 0):
            acc = (acc + i)
        else:
            acc = (acc - i)
        i = (i + 1)
    console.print("${acc}")
"#,
            ),
            (
                "records + update + field access",
                r#"
type Point:
    x: Int
    y: Int

fn main(console: Console):
    let p = Point(3, 4)
    let q = Point(x: ((p).x + 10), ..p)
    console.print("${((q).x * (q).y)}")
"#,
            ),
            (
                "lists + recursion + head/tail match",
                r#"
fn sum(xs: List(Int)) -> Int:
    match xs:
        [] -> 0
        [h, ..t] -> (h + sum(t))

fn main(console: Console):
    console.print("${sum([1, 2, 3, 4, 5])}")
"#,
            ),
            (
                "ADTs + match",
                r#"
type Shape:
    Circle(Int)
    Rect(Int, Int)

fn area(s: Shape) -> Int:
    match s:
        Circle(r) -> ((3 * r) * r)
        Rect(w, h) -> (w * h)

fn main(console: Console):
    console.print("${(area(Circle(5)) + area(Rect(3, 4)))}")
"#,
            ),
            (
                "capturing closures + higher-order",
                r#"
fn apply(f: fn(Int) -> Int, x: Int) -> Int:
    f(x)

fn main(console: Console):
    let k = 100
    console.print("${apply(fn(n: Int): (n + k), 5)}")
"#,
            ),
            (
                "dicts",
                r#"
fn main(console: Console):
    var d = dict.new()
    dict.insert(d, "a", 1)
    dict.insert(d, "b", 2)
    dict.insert(d, "a", 9)
    console.print("${(dict.get_or(d, "a", 0) + dict.length(d))}")
"#,
            ),
            (
                "strings",
                r#"
fn main(console: Console):
    console.print("a,b,c".replace(",", "-"))
    console.print("${"hello".contains("l")}")
    console.print("hello".substring(1, 4))
    for w in "the cat sat".split(" "):
        console.print(w)
"#,
            ),
            (
                "string equality across a List(String) parameter",
                r#"
fn count_matches(words: List(String), target: String) -> Int:
    var n = 0
    for w in words:
        if (w == target):
            n = (n + 1)
    n

fn main(console: Console):
    let words = "apple banana apple cherry apple".split(" ")
    console.print("${count_matches(words, "apple")}")
"#,
            ),
            (
                "string equality + ordering",
                r#"
fn main(console: Console):
    let a = "xapple".substring(1, 6)
    console.print("${(a == "apple")}")
    console.print("${(a == "apricot")}")
    console.print("${(a != "apricot")}")
    console.print("${("apple" < "banana")}")
    console.print("${("banana" < "apple")}")
    console.print("${("app" < "apple")}")
    console.print("${("apple" <= "apple")}")
"#,
            ),
            (
                "tuples + polymorphic to_string",
                r#"
fn main(console: Console):
    let (a, b) = (7, 8)
    console.print("${(a + b)}")
    console.print("${(a < b)}")
    console.print("${"done"}")
"#,
            ),
            (
                // Regression (M7): an inline `else:` ending in a bare identifier,
                // immediately followed by a `"${...}"` interpolation, must parse as
                // two statements (not `count(...)`). (Builtins/prelude only — this
                // harness doesn't link std modules.)
                "inline else bare-ident before an interpolation",
                r#"
fn describe(n: Int) -> String:
    let label = if n < 0: "neg" else: "pos"
    let mag = if n < 0: 0 - n else: n
    "${label}:${mag}"

fn main(console: Console):
    console.print(describe(0 - 4250))
    console.print(describe(150000))
"#,
            ),
        ];
        for (name, src) in programs {
            let interpreted = interp(src);
            let compiled = run_on_wasm(src);
            assert_eq!(
                interpreted, compiled,
                "interpreter and compiled backends diverged for `{name}`"
            );
        }
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


    // A guard on a constructor pattern must bind the field first, then test it
    // (`Yep(n) if n > 10`), and fall through to the next arm when the guard
    // fails. Mutual recursion exercises forward references between compiled
    // functions. Both must agree across backends.
    #[test]
    fn adt_guards_and_mutual_recursion_backends_agree() {
        let src = r#"
type Opt:
    Nope
    Yep(Int)

fn describe(o: Opt) -> String:
    match o:
        Yep(n) if (n > 10) -> "big"
        Yep(n) -> "small"
        Nope -> "none"

fn is_even(n: Int) -> Bool:
    if (n == 0):
        true
    else:
        is_odd((n - 1))

fn is_odd(n: Int) -> Bool:
    if (n == 0):
        false
    else:
        is_even((n - 1))

fn main(console: Console):
    console.print(describe(Yep(50)))
    console.print(describe(Yep(3)))
    console.print(describe(Nope))
    console.print("${if is_even(10): 1 else: 0}")
    console.print("${if is_odd(7): 1 else: 0}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "adt guards / mutual recursion diverged");
        assert_eq!(run_on_wasm(src), vec!["big", "small", "none", "1", "1"]);
    }

    // A recursive ADT (binary tree) with nested constructor patterns, exercised
    // by two recursive traversals (sum and depth). Recursion through a
    // heap-allocated ADT and destructuring `Node(l, v, r)` must agree across
    // backends.
    #[test]
    fn recursive_tree_adt_backends_agree() {
        let src = r#"
type Tree:
    Leaf
    Node(Tree, Int, Tree)

fn sum_tree(t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, v, r) -> ((sum_tree(l) + v) + sum_tree(r))

fn depth(t: Tree) -> Int:
    match t:
        Leaf -> 0
        Node(l, v, r) ->
            let dl = depth(l)
            let dr = depth(r)
            (1 + if (dl > dr): dl else: dr)

fn main(console: Console):
    let t = Node(Node(Leaf, 1, Node(Leaf, 5, Leaf)), 2, Node(Leaf, 3, Leaf))
    console.print("${sum_tree(t)}")
    console.print("${depth(t)}")
"#;
        assert_eq!(interp(src), run_on_wasm(src), "recursive tree ADT diverged");
        assert_eq!(run_on_wasm(src), vec!["11", "3"]);
    }

    // Regression: a local variable that shares its name with a same-module
    // function must stay a local, not be rewritten into a first-class reference
    // to that function by the linker. (The function-as-value feature qualifies
    // bare function-name Vars; it must skip names shadowed by a local.)
    #[test]
    fn local_shadowing_function_name_backends_agree() {
        let client = r#"
fn size(n: Int) -> Int:
    (n * 100)

fn main(console: Console):
    var size = 3
    size = (size + 4)
    console.print("${size}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "local shadowing a function name diverged");
        assert_eq!(compiled, vec!["7"]);
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
