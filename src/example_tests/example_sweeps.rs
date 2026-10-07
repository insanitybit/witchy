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

    /// Every ```` ```witchy ```` block in the root docs and the language specification
    /// parses, checks, and (when it is a Console-only program) runs on admitted Wasm.
    #[test]
    fn documentation_examples_are_valid() {
        let fresh = generate_examples_manifest();
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(&fresh).expect("generated examples manifest is valid JSON");
        assert!(!entries.is_empty(), "expected checked documentation examples");
    }

    #[test]
    fn portable_wasm_is_readmitted_from_its_source_while_detached_bytes_refuse() {
        // `emit-wasm` output carries its closed source; the runner re-admits
        // it and runs the fresh artifact. The same module with that section
        // stripped carries no admission and must refuse before any grant.
        let tmp = std::env::temp_dir().join(format!("witchy_tier1_precompiled_{}.wasm", std::process::id()));
        let out = tmp.to_str().unwrap();
        crate::emit_wasm_file("examples/calc/src/calc.witchy", out).expect("emit-wasm");
        let run = |path: &str| crate::run_wasm_file(path, Vec::new(), Vec::new(), Vec::new(), Vec::new(), None, None, Vec::new(), None, Vec::new(), false, witchy_confinement::EnforcementMode::Disabled);
        let expected = [
            "2 + 3 * 4       => 14",
            "(2 + 3) * 4     => 20",
            "100 - 2 - 3     => 95",
            "2 * (10 - 1)    => 18",
            "8 / (4 - 4)     => error: division by zero",
            "2 * (3 +        => error: unexpected end of input",
        ].map(str::to_owned);
        assert_eq!(run(out).expect("run portable calc").0, expected);

        let sealed = std::fs::read(&tmp).unwrap();
        let module = witchy::portable_artifact::probe(&sealed).unwrap().expect("embedded source").module;
        std::fs::write(&tmp, module).unwrap();
        let error = run(out).expect_err("detached Wasm must not recover admission authority");
        assert!(error.contains("without embedded source cannot be admitted"), "{error}");
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
