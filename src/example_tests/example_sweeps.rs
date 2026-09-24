use super::*;
use crate::{codegen, interpreter, parser};

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
            generated.push_str(&witchy_syntax::doc::render(stem, &src).expect("render"));
        }
        let generated = crate::commands::frontend::canonical_document_eof(generated);
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

    /// (RFC-0041 Phase 3) `book/examples.json` — the committed classification manifest — must
    /// match what the classifier produces, so the runnable book can never show a reader an
    /// output the toolchain would not. Freshness-gated exactly like `stdlib_docs_are_current`.
    /// Regenerate with: `BLESS_EXAMPLES=1 cargo test -p witchy documentation_examples_are_valid`.
    #[test]
    fn documentation_examples_are_valid() {
        let full_sweep = std::env::var_os("BLESS_EXAMPLES").is_some()
            || std::env::var_os("WITCHY_FULL_DOCS_SWEEP").is_some_and(|v| v == "1")
            || std::env::var_os("WITCHY_NIGHTLY").is_some_and(|v| v == "1");
        let path = std::path::Path::new("book/examples.json");
        let fresh = generate_examples_manifest();
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&fresh).expect("generated examples manifest is valid JSON");
        assert!(!entries.is_empty(), "expected checked documentation examples");

        if std::env::var("BLESS_EXAMPLES").is_ok() {
            std::fs::write(path, &fresh).expect("write book/examples.json");
            return;
        }
        let committed_str = std::fs::read_to_string(path).unwrap_or_else(|_| {
            panic!("book/examples.json missing — regenerate: BLESS_EXAMPLES=1 cargo test documentation_examples_are_valid")
        });
        let committed_entries: Vec<serde_json::Value> = serde_json::from_str(&committed_str)
            .expect("committed book/examples.json is valid JSON");

        if full_sweep {
            assert_eq!(
                committed_str, fresh,
                "book/examples.json is stale — regenerate: BLESS_EXAMPLES=1 cargo test documentation_examples_are_valid"
            );
        } else {
            // Verify that all generated entries in the representative set match committed entries exactly.
            for fresh_entry in &entries {
                let file = fresh_entry["file"].as_str().unwrap();
                let block = fresh_entry["block"].as_i64().unwrap();
                let matching = committed_entries.iter().find(|e| {
                    e["file"].as_str() == Some(file) && e["block"].as_i64() == Some(block)
                });
                assert!(
                    matching.is_some(),
                    "missing entry in committed book/examples.json for {file} block {block}"
                );
                assert_eq!(
                    matching.unwrap(), fresh_entry,
                    "entry mismatch for {file} block {block}"
                );
            }
        }
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
    fn detached_wasm_requires_admission_while_the_admitted_artifact_runs() {
        // Detached Wasm bytes no longer carry canonical admission authority.
        // Preserve both sides of that boundary: raw distribution fails closed,
        // while the authenticated artifact produced in-process executes and
        // matches an independent semantic expectation.
        let tmp = std::env::temp_dir().join(format!("witchy_tier1_precompiled_{}.wasm", std::process::id()));
        let out = tmp.to_str().unwrap();
        crate::emit_wasm_file("examples/calc/src/calc.witchy", out).expect("emit-wasm");
        let error = crate::run_wasm_file(out, Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, Vec::new(), false, witchy_confinement::EnforcementMode::Disabled)
            .expect_err("detached Wasm must not recover admission authority");
        assert!(error.contains("raw Wasm cannot claim canonical admission"), "{error}");

        let (checked, _) = crate::link_file_checked("examples/calc/src/calc.witchy")
            .expect("checked calc source");
        let artifact = codegen::compile_checked_module_artifact(&checked)
            .expect_lowered("canonical calc admission");
        let expected = [
            "2 + 3 * 4       => 14",
            "(2 + 3) * 4     => 20",
            "100 - 2 - 3     => 95",
            "2 * (10 - 1)    => 18",
            "8 / (4 - 4)     => error: division by zero",
            "2 * (3 +        => error: unexpected end of input",
        ].map(str::to_owned);
        assert_eq!(
            crate::run_admitted_artifact(&artifact).expect("run admitted calc"),
            expected,
        );
        let _ = std::fs::remove_file(&tmp);
    }

    /// Every example must validate through its usable CLI path. Finite examples
    /// execute to completion; server demos type-check because running them would
    /// require a network grant and never terminate.
    #[test]
    fn all_examples_validate_via_cli() {
        let mut files = example_entries();
        files.sort();
        assert!(!files.is_empty(), "no examples found");
        let failures: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = files.iter().map(|path| {
                s.spawn(|| {
                    let p = path.to_str().unwrap();
                    let server = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("serve_"));
                    if server {
                        crate::check_file(p).err().map(|error| format!("{p}: {error:?}"))
                    } else {
                        crate::execute_file(p, Vec::new())
                            .err()
                            .map(|error| format!("{p}: {error:?}"))
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

    /// Every bundled std module must type-check in the authenticated complete
    /// graph and stay free of O(n²) accumulation cliffs. Checking every module
    /// as a separate root repeats the same prelude parse, link, and type-check
    /// work, while a second cliff test would repeat the complete graph again.
    #[test]
    fn all_std_modules_type_check() {
        let mut files = std::fs::read_dir("std")
            .expect("std directory")
            .filter_map(|entry| {
                let path = entry.expect("std directory entry").path();
                (path.extension().and_then(|extension| extension.to_str()) == Some("witchy"))
                    .then(|| {
                        path.file_stem()
                            .expect("std module stem")
                            .to_string_lossy()
                            .into_owned()
                    })
            })
            .collect::<Vec<_>>();
        files.sort();
        let mut registry = witchy_syntax::linker::STD_MODULES
            .iter()
            .map(|module| (*module).to_owned())
            .collect::<Vec<_>>();
        registry.sort();
        assert_eq!(
            files, registry,
            "bundled std registry must name every std source exactly once"
        );

        let source = registry
            .iter()
            .map(|module| format!("import {module}\n"))
            .collect::<String>();
        let checked = witchy::resolve_std_only_checked(&source)
            .expect("the complete bundled std graph must type-check");

        let offenders = witchy_lower::analysis::module_cliffs(checked.module())
            .into_iter()
            .map(|(function, cliff)| {
                format!(
                    "{function} (line {}): `{}` is rebuilt by copy each iteration — {}",
                    cliff.line, cliff.var, cliff.reason
                )
            })
            .collect::<Vec<_>>();
        assert!(
            offenders.is_empty(),
            "stdlib performance cliffs (O(n²) accumulation) found:\n  {}",
            offenders.join("\n  ")
        );
    }

