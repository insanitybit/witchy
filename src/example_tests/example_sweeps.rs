use super::*;
use crate::{ast, codegen, interpreter, parser, typeck};

    /// The bundled stdlib must stay on the in-place fast path: a performance cliff
    /// (an accumulator that reverts to copy-per-iteration, i.e. O(n²)) anywhere in
    /// std silently slows every program that touches it. This applies `mode opt`'s
    /// cliff detection to ALL 42 std modules as a regression guard — the same check
    /// that turned `string.chars`/`json.decode` from O(n²) into O(n). Adding a
    /// growing-buffer loop to a std module without keeping it on the fast path fails
    /// HERE, loudly, with the function and the offending accumulator.
    #[test]
    fn stdlib_has_no_performance_cliffs() {
        // Link a synthetic entry that imports every std module, so cross-module call
        // summaries resolve exactly as in real compilation (a per-module scan would
        // false-positive on calls like `list.join` whose summary lives elsewhere).
        let imports: String =
            crate::linker::STD_MODULES.iter().map(|m| format!("import {m}\n")).collect();
        let entry_src = format!("{imports}\npub fn perfcheck() -> Int:\n    0\n");
        let entry = parser::parse_module(&entry_src).expect("parse synthetic std-import entry");
        let linked = crate::pipeline::link(vec![("perfcheck".into(), entry)], "perfcheck")
            .expect("link all std modules");
        // The whole stdlib is cliff-free: every "build a sub-list, then collect it into a
        // result list" shape (`out = push(out, move cur); cur = []`) transfers ownership with
        // `move`, so the sub-list's per-element pushes stay in place (the `move`-resets-cap fix
        // makes that sound). No allowlist — a new cliff is a hard failure.
        let offenders: Vec<String> = crate::analysis::module_cliffs(&linked)
            .into_iter()
            .map(|(func, c)| {
                format!("{func} (line {}): `{}` is rebuilt by copy each iteration — {}", c.line, c.var, c.reason)
            })
            .collect();
        assert!(
            offenders.is_empty(),
            "stdlib performance cliffs (O(n²) accumulation) found:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// The committed `spec/stdlib.md` must match what `witchy doc` generates from
    /// the std sources — so a std module change that isn't re-documented fails
    /// loudly. Regenerate with: `witchy doc std/*.witchy > spec/stdlib.md`.
    #[test]
    fn stdlib_docs_are_current() {
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir("std")
            .expect("read std/")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("witchy"))
            .collect();
        files.sort();
        let mut generated = String::from("# API reference\n\n");
        for f in &files {
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap();
            let src = std::fs::read_to_string(f).unwrap();
            generated.push_str(&crate::doc::render(stem, &src).expect("render"));
        }
        let committed = std::fs::read_to_string("spec/stdlib.md").expect("read spec/stdlib.md");
        for (i, (g, c)) in generated.lines().zip(committed.lines()).enumerate() {
            assert_eq!(
                g,
                c,
                "spec/stdlib.md is stale at line {} — regenerate with `witchy doc std/*.witchy > spec/stdlib.md`",
                i + 1
            );
        }
        assert_eq!(
            generated.lines().count(),
            committed.lines().count(),
            "spec/stdlib.md length differs — regenerate with `witchy doc std/*.witchy > spec/stdlib.md`"
        );
    }

    /// Every ```witchy code block in the documentation must be a real program:
    /// it parses, links, and type-checks; and when it defines a `main` whose
    /// footprint needs nothing beyond Console, it RUNS on both backends and the
    /// outputs must agree. Docs that drift from the language break the build.
    #[test]
    fn documentation_examples_are_valid() {
        let files = doc_markdown_files();

        let results: Vec<(usize, usize)> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|file| {
                s.spawn(move || {
                    let mut checked = 0usize;
                    let mut ran = 0usize;
                    let Ok(text) = std::fs::read_to_string(file) else { return (0, 0) };
                    for (idx, snippet) in extract_witchy_blocks(&text).into_iter().enumerate() {
                        let context = format!("{}: ```witchy block #{}", file.display(), idx + 1);
                        let proof = witchy::resolve_std_only_checked(&snippet)
                            .unwrap_or_else(|e| {
                                panic!("{context} fails to parse, link, or type-check: {e}\n---\n{snippet}")
                            });
                        let linked = proof.module();
                        checked += 1;

                        let has_main = linked
                            .items
                            .iter()
                            .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
                        let has_actor = false;
                        let reads_argv = linked.items.iter().any(|it| {
                            matches!(it, ast::Item::Function(f) if f.name == "main"
                                && f.params.iter().any(|p| matches!(&p.ty,
                                    Some(ast::Type::Named(n, args)) if n == "List"
                                        && matches!(args.first(),
                                            Some(ast::Type::Named(s, _)) if s == "String"))))
                        });
                        let footprint = crate::capabilities::analyze(linked);
                        let console_only = footprint.total.keys().all(|k| *k == "Console");
                        if has_main && console_only && !has_actor && !reads_argv {
                            let bytes = codegen::compile_checked_module_binary(&proof)
                                .expect_lowered(&format!("{context} compiles to WASM"));
                            let interp = interpreter::run_checked_module(
                                proof,
                                std::path::Path::new("."),
                                Vec::new(),
                            )
                            .unwrap_or_else(|e| panic!("{context} fails on the interpreter: {e}"));
                            let compiled = crate::run_wasm_bytes(&bytes)
                                .unwrap_or_else(|e| panic!("{context} fails on WASM: {e}"));
                            assert_eq!(interp, compiled, "{context}: the backends DIVERGE");
                            ran += 1;
                        }
                    }
                    (checked, ran)
                })
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let checked: usize = results.iter().map(|(c, _)| c).sum();
        let ran: usize = results.iter().map(|(_, r)| r).sum();
        assert!(checked >= 20, "expected the docs to carry many checked examples, found {checked}");
        assert!(ran >= 5, "expected several runnable doc examples, found {ran}");
    }

    /// (RFC-0041 Phase 3) `book/examples.json` — the committed classification manifest — must
    /// match what the classifier produces, so the runnable book can never show a reader an
    /// output the toolchain would not. Freshness-gated exactly like `stdlib_docs_are_current`.
    /// Regenerate with: `BLESS_EXAMPLES=1 cargo test -p witchy book_examples_manifest_is_current`.
    #[test]
    fn book_examples_manifest_is_current() {
        let fresh = generate_examples_manifest();
        let path = std::path::Path::new("book/examples.json");
        if std::env::var("BLESS_EXAMPLES").is_ok() {
            std::fs::write(path, &fresh).expect("write book/examples.json");
            return;
        }
        let committed = std::fs::read_to_string(path).unwrap_or_else(|_| {
            panic!("book/examples.json missing — regenerate: BLESS_EXAMPLES=1 cargo test book_examples_manifest_is_current")
        });
        assert_eq!(
            committed, fresh,
            "book/examples.json is stale — regenerate: BLESS_EXAMPLES=1 cargo test book_examples_manifest_is_current"
        );
    }

    /// The in-language test framework: `witchy test` discovers zero-parameter
    /// `test_*` functions, a test passes by returning and fails by aborting
    /// (std/testing's assertions report a message). Capability-free by
    /// construction.
    #[test]
    fn in_language_test_framework_runs_and_reports() {
        let path = std::env::temp_dir().join(format!("witchy_testfw_{}.witchy", std::process::id()));
        std::fs::write(
            &path,
            "import testing\n\nfn double(n: Int) -> Int:\n    n * 2\n\nfn test_double():\n    testing.assert_int_eq(double(21), 42)\n\nfn test_strings():\n    testing.assert_eq(\"a\" + \"b\", \"ab\")\n    testing.assert_ne(\"a\", \"b\")\n\nfn test_broken():\n    testing.assert(1 > 2, \"deliberately wrong\")\n",
        )
        .unwrap();
        let (passed, failed) = crate::run_tests_in_file(path.to_str().unwrap()).expect("run");
        assert_eq!(passed.len(), 2, "two passing tests: {passed:?}");
        assert_eq!(failed.len(), 1, "one failing test: {failed:?}");
        assert!(failed[0].0.ends_with("test_broken"));
        assert!(
            failed[0].1.contains("deliberately wrong"),
            "failure carries the assertion message: {}",
            failed[0].1
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn every_example_type_checks() {
        let entries = example_entries();
        let failures: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(move || {
                    let p = path.to_str().unwrap();
                    crate::check_file(p).err().map(|e| format!("{p}: {e}"))
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(failures.is_empty(), "examples fail to type-check:\n{}", failures.join("\n"));
    }

    #[test]
    fn every_compilable_example_agrees_on_both_backends() {
        // Differential guard: every example that compiles to WASM must produce
        // identical output on the interpreter and the compiled backend. Examples
        // that are interpreter-only (actors, networking, float/case formatting) or
        // are libraries with no `main` cannot compile and are skipped — only a
        // genuine divergence fails. (This would have caught the trailing-newline
        // print divergence.)
        let entries = example_entries();
        let diverged: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    if let crate::ParityOutcome::Diverge { message, .. } = crate::parity_check(p) {
                        Some(message)
                    } else {
                        None
                    }
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            diverged.is_empty(),
            "examples diverge across backends:\n{}",
            diverged.join("\n")
        );
    }

    #[test]
    fn examples_agree_under_rc_floor() {
        // Metamorphic, NO-ORACLE guard for the RC-floor lever: `WITCHY_OPT=rc-floor` adds the
        // dup/drop refcount discipline + free-at-overwrite/last-use reclamation, which must be
        // OUTPUT-TRANSPARENT — compiling with it on produces byte-identical output to the default.
        // A premature or wrong free (a use-after-free) shows up as a divergence here. This is the
        // check that would have caught the free-at-overwrite alias-init UAF (it corrupted
        // `toml.get_array`); before it, NO test ran the examples under this lever — exactly how a
        // gated-lever memory bug hides. Restricted to console-only, `main`-bearing programs so the
        // run needs no capability grants and the output is deterministic.
        use crate::opt::{self, Opt, OptSet};
        let entries = example_entries();
        let diverged: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    let Ok((linked, _)) = crate::link_file(p) else {
                        return None;
                    };
                    if typeck::check(&linked).is_err() {
                        return None;
                    }
                    let has_main = linked
                        .items
                        .iter()
                        .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
                    let console_only = crate::capabilities::analyze(&linked)
                        .total
                        .keys()
                        .all(|k| *k == "Console");
                    if !has_main || !console_only {
                        return None;
                    }
                    let compile_with_rc = |on: bool| {
                        opt::set_for_tests(if on {
                            Some(OptSet::default_set().with(Opt::RcFloor))
                        } else {
                            None
                        });
                        let bytes = codegen::compile_module_binary(&linked);
                        opt::set_for_tests(None);
                        bytes
                    };
                    if let (
                        codegen::LoweringOutcome::Lowered(def),
                        codegen::LoweringOutcome::Lowered(rc),
                    ) = (compile_with_rc(false), compile_with_rc(true)) {
                        let a = crate::run_wasm_bytes(&def);
                        let b = crate::run_wasm_bytes(&rc);
                        if a != b {
                            return Some(format!("{p}: default {a:?} vs rc-floor {b:?}"));
                        }
                    }
                    None
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            diverged.is_empty(),
            "rc-floor diverges from the default codegen on examples (a reclamation use-after-free):\n{}",
            diverged.join("\n")
        );
    }

    #[test]
    fn every_example_agrees_under_rc_floor() {
        use crate::opt::{Opt, OptSet};
        assert_examples_agree_under(OptSet::default_set().with(Opt::RcFloor), "rc-floor");
    }

    #[test]
    fn every_example_agrees_under_unbox() {
        // Same guard for the other opt-in codegen lever: `unbox` (RFC-0027 packed-by-inference)
        // changes heap LAYOUT, so a missed read/write site would corrupt exactly like a reclamation
        // UAF. It is currently clean, but — like rc-floor — the default sweep never exercised it.
        use crate::opt::{Opt, OptSet};
        assert_examples_agree_under(OptSet::default_set().with(Opt::Unbox), "unbox");
    }

    #[test]
    fn precompiled_wasm_runs_like_the_source() {
        // C Tier 1 (distribution): a program emitted to a `.wasm` and run as a
        // precompiled module — with authority derived from its imports — produces
        // the same output as running the source. This is "ship the .wasm, run it
        // with witchy".
        let tmp = std::env::temp_dir().join(format!("witchy_tier1_precompiled_{}.wasm", std::process::id()));
        let out = tmp.to_str().unwrap();
        crate::emit_wasm_file("examples/calc/src/calc.witchy", out).expect("emit-wasm");
        let (from_wasm, _) =
            crate::run_wasm_file(out, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, Vec::new(), false).expect("run .wasm");
        let from_source = crate::execute_file("examples/calc/src/calc.witchy", Vec::new()).expect("run source");
        assert_eq!(from_wasm, from_source, "precompiled .wasm diverges from the source run");
        let _ = std::fs::remove_file(&tmp);
    }

    /// WIR migration progress meter (not an assertion): reports how many example
    /// programs take the AST→WIR→wasm-binary path vs. still fall back to WAT.
    /// Run with `cargo test --features native binary_path_coverage_report --
    /// --ignored --nocapture`; add `WIRDIAG=1` to also print, per bailing program,
    /// which function(s) didn't lower (the `assemble_wir_module` diagnostic).
    /// Library files (no `main`) can't be a standalone binary, so they're skipped
    /// rather than counted as fallbacks.
    #[test]
    #[ignore]
    fn binary_path_coverage_report() {
        let mut dirs = vec![std::path::PathBuf::from("examples")];
        let mut srcs: Vec<std::path::PathBuf> = vec![];
        while let Some(d) = dirs.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    dirs.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("witchy") {
                    srcs.push(p);
                }
            }
        }
        srcs.sort();
        let (mut ok, mut total) = (0, 0);
        let mut bailed: Vec<String> = vec![];
        for p in srcs {
            let ps = p.to_str().unwrap().to_string();
            if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("serve_")) {
                continue;
            }
            let linked = match crate::link_file(&ps) {
                Ok((m, _)) => m,
                Err(_) => continue,
            };
            if typeck::check(&linked).is_err() {
                continue;
            }
            // Skip library modules (no `main`): they're linked INTO a main program,
            // never compiled standalone, so they aren't real binary-path fallbacks.
            let has_main = linked
                .items
                .iter()
                .any(|it| matches!(it, ast::Item::Function(f) if f.name == "main"));
            if !has_main {
                continue;
            }
            total += 1;
            if matches!(
                codegen::compile_module_binary(&linked),
                codegen::LoweringOutcome::Lowered(_)
            ) {
                ok += 1;
            } else {
                bailed.push(ps);
            }
        }
        eprintln!("\n=== WIR binary-path coverage: {ok}/{total} ===");
        for b in &bailed {
            eprintln!("  fallback: {b}");
        }
    }

    /// Every example must at least compile (parse + link + type-check) and run
    /// to completion through the CLI without an error — whether it prints, just
    /// returns a value, or is a library/actor file with no `main`. Server demos
    /// (`serve_*`) are excluded: they need a `--net` grant and run forever, so
    /// they're covered by the loopback tests instead, not run-to-completion here.
    #[test]
    fn all_examples_run_via_cli() {
        let mut files: Vec<std::path::PathBuf> = example_entries()
            .into_iter()
            .filter(|p| {
                !p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("serve_"))
            })
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no examples found");
        let failures: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    match crate::execute_file(p, Vec::new()) {
                        Ok(_) => None,
                        Err(e) => Some(format!("{p}: {e:?}")),
                    }
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            failures.is_empty(),
            "examples failed:\n{}",
            failures.join("\n")
        );
    }

    /// EVERY example — including the server demos that run forever (and so are
    /// excluded from the run-to-completion test above) — must parse, link, and
    /// type-check. Catches type errors the run test can't reach.
    #[test]
    fn all_examples_type_check() {
        let entries = example_entries();
        assert!(!entries.is_empty(), "no examples found");
        let failures: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = entries.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    match crate::check_file(p) {
                        Ok(_) => None,
                        Err(e) => Some(format!("{p}: {e:?}")),
                    }
                })
            }).collect();
            handles.into_iter().filter_map(|h| h.join().unwrap()).collect()
        });
        assert!(
            failures.is_empty(),
            "type-check failed:\n{}",
            failures.join("\n")
        );
    }

    /// Every example rune's in-language tests (`src/*_test.witchy`) pass. This
    /// keeps the per-example `witchy test` suites green in CI — so an example
    /// whose behavior drifts from its documented tests fails the build, not just
    /// a manual run. (Multi-rune `projects/` are skipped here: their cross-rune
    /// path dependencies are exercised by the package-manager tests instead.)
    #[test]
    fn all_example_rune_tests_pass() {
        let mut test_files: Vec<std::path::PathBuf> = Vec::new();
        for dir in std::fs::read_dir("examples").expect("examples directory") {
            let dir = dir.unwrap().path();
            if !dir.is_dir() {
                continue;
            }
            if dir.file_name().and_then(|s| s.to_str()) == Some("projects") {
                continue;
            }
            let Ok(rd) = std::fs::read_dir(dir.join("src")) else {
                continue;
            };
            let mut files: Vec<std::path::PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with("_test.witchy"))
                })
                .collect();
            files.sort();
            test_files.extend(files);
        }
        assert!(!test_files.is_empty(), "no example rune test files found");
        let ran: usize = std::thread::scope(|s| {
            let handles: Vec<_> = test_files.iter().map(|tf| {
                s.spawn(move || {
                    let p = tf.to_str().unwrap();
                    let (passed, failed) =
                        crate::run_tests_in_file(p).unwrap_or_else(|e| panic!("{p}: {e}"));
                    assert!(failed.is_empty(), "{p}: test failures: {failed:?}");
                    assert!(!passed.is_empty(), "{p}: a `*_test.witchy` with no `test_*` functions");
                    passed.len()
                })
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect::<Vec<_>>().into_iter().sum()
        });
        assert!(ran > 0, "no example rune tests ran");
    }

    /// Every bundled std module must type-check on its own (linked with its
    /// imports). The interpreter doesn't type-check, so without this a latent
    /// type error in a module no example imports would go unnoticed.
    #[test]
    fn all_std_modules_type_check() {
        let mut any = false;
        for entry in std::fs::read_dir("std").expect("std directory") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("witchy") {
                continue;
            }
            any = true;
            let p = path.to_str().unwrap();
            assert!(
                crate::check_file(p).is_ok(),
                "type-check failed for `{p}`: {:?}",
                crate::check_file(p)
            );
        }
        assert!(any, "no std modules found");
    }

    // The linker treats the bundled std as a built-in search path: a program can
    // `import list` without the caller listing the module's source. This unblocks
    // composable std modules (one std module importing another). Verified on both
    // backends with only `main` provided.
    #[test]
    fn linker_auto_resolves_std_imports() {
        let client = r#"
import list

fn main(console: Console):
    console.print("${list.sum(list.range(5))}")
"#;
        let sources = [("main", client)];
        let interpreted = interpreter::run_program(&sources, "main").expect("interp");
        let compiled = run_linked_on_wasm(&sources, "main");
        assert_eq!(interpreted, compiled, "auto-resolved std import diverged");
        assert_eq!(compiled, vec!["10"]);
    }

    // std/json get_path: follow a dotted key path into a decoded value (and a
    // missing path -> None). Pure, so both backends agree.
    // Dead-code elimination: a program importing `list` but using only `map`
    // and `sum` must not compile the rest of the list API (or `option`, which
    // `list` imports) into the WASM — only functions reachable from `main`.
    #[test]
    fn dce_drops_unused_stdlib_functions() {
        let client = r#"
import list

fn main(console: Console):
    let xs = list.map([1, 2, 3], fn(x: Int): (x * 2))
    console.print("${list.sum(xs)}")
"#;
        let mods = vec![("main".to_string(), parser::parse_module(client).expect("parse"))];
        let linked = crate::pipeline::link(mods, "main").expect("link");
        // Reachable functions are present and the unused ones are dropped: the
        // binary path's `assemble_wir_module` runs the same `reachable_functions`
        // DCE, so inspect the assembled WIR func names directly.
        let wir = codegen::assemble_wir_module(&linked)
            .expect_lowered("the binary path lowers this program");
        // The binary path monomorphizes generics, so the method implementation
        // appears as `List__map__Int__Int`; match on the generated-method prefix.
        let names: Vec<&str> = wir.funcs.iter().map(|f| f.name.as_str()).collect();
        let has = |fn_name: &str| names.iter().any(|n| *n == fn_name || n.starts_with(&format!("{fn_name}__")));
        assert!(has("List__map"), "map should be compiled: {names:?}");
        assert!(has("List__sum"), "sum should be compiled: {names:?}");
        assert!(!has("List__partition"), "partition should be eliminated: {names:?}");
        assert!(!has("List__windows"), "windows should be eliminated: {names:?}");
        assert!(!has("List__sort_by"), "sort_by should be eliminated: {names:?}");
        assert!(!names.iter().any(|n| n.starts_with("option.")), "unused option fns should be eliminated: {names:?}");
        // And it still runs correctly.
        assert_eq!(run_linked_on_wasm(&[("main", client)], "main"), vec!["12"]);
    }
