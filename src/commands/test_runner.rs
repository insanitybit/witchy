//! The `witchy test` runner: test discovery, integration-test grant
//! validation, dependency-root resolution, and execution on the compiled
//! backend. Extracted from the composition root.

use crate::commands::execution::run_linked_compiled;
use crate::{
    ast, codegen, enforce_performance_modes, is_entry_function, link_file_with_mode, linker, parser,
    run_wasm_test_bytes, typeck,
};

/// Run a program on BOTH backends — the tree-walking interpreter and compiled
/// WebAssembly — and confirm they produce identical output. Witchy's
/// dual-backend equivalence is normally an internal test invariant; `witchy
/// verify` surfaces it as a guarantee you can check on your own code.
/// A failed in-language test: its (qualified) name and the abort message.
pub(crate) type TestFailure = (String, String);

pub(crate) const TEST_USAGE: &str =
    "usage: witchy test [--integration] [--dir <root>]... [--net <addr>]... <file.witchy|dir>";

#[derive(Clone, Debug, Default)]
struct TestGrants {
    dir_roots: Vec<std::path::PathBuf>,
    net_allow: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct TestOptions {
    path: String,
    integration: bool,
    grants: TestGrants,
}

impl TestOptions {
    pub(crate) fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let mut path = None;
        let mut integration = false;
        let mut grants = TestGrants::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--integration" => {
                    if integration {
                        return Err("`--integration` may be specified only once".to_string());
                    }
                    integration = true;
                }
                "--dir" => {
                    let root = args
                        .next()
                        .ok_or_else(|| "`--dir` requires a filesystem root".to_string())?;
                    grants.dir_roots.push(std::path::PathBuf::from(root));
                }
                "--net" => {
                    let addr = args
                        .next()
                        .ok_or_else(|| "`--net` requires a host or host:port".to_string())?;
                    grants.net_allow.push(addr);
                }
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown `witchy test` option `{flag}`"));
                }
                value => {
                    if path.replace(value.to_string()).is_some() {
                        return Err("`witchy test` accepts exactly one file or directory".to_string());
                    }
                }
            }
        }
        let path = path.ok_or_else(|| "`witchy test` requires a file or directory".to_string())?;
        if !integration && (!grants.dir_roots.is_empty() || !grants.net_allow.is_empty()) {
            return Err("real `--dir`/`--net` grants require `witchy test --integration`".to_string());
        }
        Ok(Self { path, integration, grants })
    }
}

#[derive(Clone, Copy)]
struct TestRunPolicy<'a> {
    integration: bool,
    real_grants: bool,
    grants: &'a TestGrants,
}

/// Rewrite the placeholder call `witchy_test_target()` in a synthesized test-driver
/// expression to the real (linker-qualified) test name — so the parser never has to
/// re-read `mod.fn` as a method call. The placeholder may sit anywhere in the driver
/// body: bare (`witchy_test_target()`), or as an argument (`task.run(
/// witchy_test_target())`, the async driver), so this recurses through calls,
/// method calls, and unary ops.
fn patch_test_target(expr: &mut ast::Expr, name: &str, params: &[ast::Param]) {
    match expr {
        ast::Expr::Call { name: n, args } => {
            if n == "witchy_test_target" {
                *n = name.to_string();
                *args = params
                    .iter()
                    .map(|param| ast::Expr::Var(param.name.clone()))
                    .collect();
            } else {
                for a in args {
                    patch_test_target(a, name, params);
                }
            }
        }
        ast::Expr::MethodCall { receiver, args, .. } => {
            patch_test_target(receiver, name, params);
            for a in args {
                patch_test_target(a, name, params);
            }
        }
        ast::Expr::Unary { expr, .. } => patch_test_target(expr, name, params),
        _ => {}
    }
}

/// The bare names of every `test_*` function in the UNLOWERED source,
/// split into `(async, gen)` sets. Async lowering (`generators` too) runs during
/// `link`, erasing `is_async`/`is_gen` and rewriting the bodies, so the linked module
/// can no longer tell an async or generator test from a plain one — this recovers
/// that shape from the raw parse. A parse/read failure yields empty sets (the linked
/// module still fails to compile and is reported separately).
fn raw_test_shapes(path: &str) -> (std::collections::HashSet<String>, std::collections::HashSet<String>) {
    let mut async_tests = std::collections::HashSet::new();
    let mut gen_tests = std::collections::HashSet::new();
    if let Ok(src) = std::fs::read_to_string(path) {
        if let Ok(module) = parser::parse_module(&src) {
            for it in &module.items {
                if let ast::Item::Function(f) = it {
                    if f.name.starts_with("test_") {
                        if f.is_async {
                            async_tests.insert(f.name.clone());
                        } else if f.is_gen {
                            gen_tests.insert(f.name.clone());
                        }
                    }
                }
            }
        }
    }
    (async_tests, gen_tests)
}

fn validate_integration_test_params(
    test: &str,
    params: &[ast::Param],
    policy: TestRunPolicy<'_>,
) -> Result<(), String> {
    if !policy.integration && !params.is_empty() {
        return Err(format!(
            "test `{test}` declares capability parameter(s); run it with `witchy test --integration` and explicit grants"
        ));
    }
    if !policy.integration {
        return Ok(());
    }

    let mut dir_count = 0usize;
    let mut needs_net = false;
    for param in params {
        let Some(ty) = param.ty.as_ref() else {
            return Err(format!(
                "integration test `{test}` parameter `{}` needs an explicit capability type (`Console`, `Dir`, or `Net`)",
                param.name
            ));
        };
        let ast::Type::Named(name, _) = ty.unqualified() else {
            return Err(format!(
                "integration test `{test}` parameter `{}` must be a `Console`, `Dir`, or `Net` capability",
                param.name
            ));
        };
        match name.as_str() {
            "Console" => {}
            "Dir" => dir_count += 1,
            "Net" => needs_net = true,
            other => {
                return Err(format!(
                    "integration test `{test}` parameter `{}` has unsupported capability type `{other}`; this tier currently accepts `Console`, `Dir`, and `Net`",
                    param.name
                ));
            }
        }
    }

    if !policy.real_grants && (dir_count > 0 || needs_net) {
        return Err(format!(
            "dependency test `{test}` requests real authority, but dependency tests receive zero real grants even under `--integration`"
        ));
    }
    if policy.grants.dir_roots.len() < dir_count {
        return Err(format!(
            "integration test `{test}` requires {dir_count} `Dir` grant(s), but {} were provided; repeat `--dir <root>`",
            policy.grants.dir_roots.len()
        ));
    }
    if needs_net && policy.grants.net_allow.is_empty() {
        return Err(format!(
            "integration test `{test}` requires a `Net` grant; provide at least one `--net <addr>`"
        ));
    }
    Ok(())
}

/// Discover and run the tests in an already-linked module (`stem` = the entry file's
/// stem). Every function named `test_*` that the ENTRY file itself declares is
/// invoked through a synthesized `main` on compiled WASM. Plain tests take no
/// parameters and receive no real authority; integration tests forward only their
/// declared capability parameters under the caller's explicit grant policy.
/// `async_tests`/`gen_tests` are the bare names of the entry file's async/gen tests
/// (from `raw_test_shapes`, since lowering erased the AST flags). Returns
/// `(passed, failures)` where each failure is `(name, message)`.
fn run_tests_in_module(
    linked: &ast::Module,
    stem: &str,
    async_tests: &std::collections::HashSet<String>,
    gen_tests: &std::collections::HashSet<String>,
    policy: TestRunPolicy<'_>,
) -> Result<(Vec<String>, Vec<TestFailure>), String> {
    typeck::check(linked).map_err(|e| e.to_string())?;
    // BUG-177: a test run honors `mode opt` like `check`/`run` — a copy-cliff or a
    // missing ownership convention fails the run, it is not silently ignored.
    enforce_performance_modes(linked, stem)?;
    // Post-link names are module-qualified (`suite.test_x`); match on the bare name.
    // BUG-185: run only the ENTRY file's OWN tests. Linking pulls an imported
    // module's `test_*` functions into `linked` too (as `othermod.test_x`); running
    // them here would DOUBLE-count them — they run again when that module's own file
    // is swept. `is_entry_function` keeps just `main` + the `{stem}.`-prefixed items.
    let tests: Vec<(String, bool, bool, Vec<ast::Param>)> = linked
        .items
        .iter()
        .filter_map(|it| match it {
            ast::Item::Function(f)
                if is_entry_function(&f.name, stem)
                    && f.name.rsplit('.').next().unwrap_or(&f.name).starts_with("test_") =>
            {
                let bare = f.name.rsplit('.').next().unwrap_or(&f.name);
                Some((
                    f.name.clone(),
                    async_tests.contains(bare),
                    gen_tests.contains(bare),
                    f.params.clone(),
                ))
            }
            _ => None,
        })
        .collect();
    let mut passed = Vec::new();
    let mut failed = Vec::new();
    for (test, is_async, is_gen, params) in tests {
        // BUG-184: an async/gen test's body does NOT run when the function is merely
        // CALLED — calling an `async fn` yields a `Task` and a `gen fn` yields an
        // iterator, both discarded, so a `fail_with` inside never fires and the test
        // FALSELY passes. An `async fn test_*()` is already lowered (when the file was
        // linked) to a `Task(Nil)`-returning function, so DRIVE it to completion with
        // `task.run` — which surfaces the abort. A `gen fn` yields a sequence rather
        // than running to completion, so it cannot be a test; report it as a failure
        // rather than a silent pass.
        if is_gen {
            failed.push((
                test,
                "a `gen fn` cannot be run as a test — it yields a sequence instead of running to completion".to_string(),
            ));
            continue;
        }
        if let Err(e) = validate_integration_test_params(&test, &params, policy) {
            failed.push((test, e));
            continue;
        }
        // Synthesize a `main` (replacing any real one) that runs the test, and run it.
        // The test name is linker-qualified (`suite.test_x`), which the parser would
        // read as a method call — so parse a placeholder and patch the call in the AST.
        // `task.run` is in scope: async lowering imported `task` into a file with any
        // `async fn`, which is exactly the case an async test needs it.
        let mut m = linked.clone();
        m.items
            .retain(|it| !matches!(it, ast::Item::Function(f) if f.name == "main"));
        let driver_src = if is_async {
            "fn main():\n    task.run(witchy_test_target())\n"
        } else {
            "fn main():\n    witchy_test_target()\n"
        };
        let mut driver = parser::parse_module(driver_src).map_err(|e| e.to_string())?;
        for it in &mut driver.items {
            if let ast::Item::Function(f) = it {
                f.params = params.clone();
                if let Some(ast::Stmt::Expr(e)) = f.body.stmts.first_mut() {
                    patch_test_target(e, &test, &params);
                }
            }
        }
        m.items.extend(driver.items);
        // Run the test on the COMPILED WASM tier — the tier users ship — not the
        // interpreter oracle: a `witchy test` that passes must reflect the backend
        // that actually runs in production. A `testing.assert` / `fail_with` lowers
        // to `__witchy_abort`, which is authority-free and always linked by the
        // runtime. Plain tests run under zero real host capability grants;
        // integration tests use the same explicit runtime-grant path as sandbox/run.
        // The synthesized `main` plus codegen's reachability pruning keep unused
        // effectful production functions out of the test artifact. A module that
        // does not lower is itself a failure: the test cannot run where it ships.
        let outcome = if policy.integration {
            run_linked_compiled(
                &m,
                policy.grants.dir_roots.clone(),
                Vec::new(),
                policy.grants.net_allow.clone(),
                Vec::new(),
                None,
                Vec::new(),
                None,
                Vec::new(),
                Vec::new(),
                true,
                true,
            )
            .map(|_| ())
        } else {
            match codegen::compile_module_binary(&m) {
                codegen::LoweringOutcome::Lowered(bytes) => {
                    run_wasm_test_bytes(&bytes).map(|_| ())
                }
                codegen::LoweringOutcome::Unsupported(reason) => Err(reason.to_string()),
                codegen::LoweringOutcome::Rejected(error) => Err(error.to_string()),
            }
        };
        match outcome {
            Ok(()) => passed.push(test),
            Err(msg) => failed.push((test, msg)),
        }
    }
    Ok((passed, failed))
}

/// Link `path` and run its own tests — the single-file convenience the test suite
/// drives. Mirrors what `run_tests` does per file (link, recover async/gen shapes,
/// dispatch to `run_tests_in_module`).
#[cfg(test)]
pub(crate) fn run_tests_in_file(path: &str) -> Result<(Vec<String>, Vec<TestFailure>), String> {
    let (linked, stem) = link_file_with_mode(path, linker::LinkMode::Test)?;
    let (async_tests, gen_tests) = raw_test_shapes(path);
    let grants = TestGrants::default();
    run_tests_in_module(
        &linked,
        &stem,
        &async_tests,
        &gen_tests,
        TestRunPolicy { integration: false, real_grants: false, grants: &grants },
    )
}

#[cfg(test)]
mod test_mode_link_tests {
    use super::run_tests_in_file;
    use crate::link_file;

    fn unique_dir(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("witchy_{name}_{}_{}", std::process::id(), nanos))
    }

    #[test]
    fn witchy_test_allows_entry_to_construct_foreign_sealed_data() {
        let dir = unique_dir("sealed_test_mode");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sealed_lib.witchy"),
            "sealed type Version:\n    Version(Int, Int, Int)\n\n\
             pub fn major(v: Version) -> Int:\n    \
             match v:\n        Version(n, _, _) -> n\n",
        )
        .unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import sealed_lib\nimport testing\n\n\
             fn test_constructs_domain_edge_case():\n    \
             let v = sealed_lib.Version(99, 0, 0)\n    \
             testing.assert_int_eq(sealed_lib.major(v), 99)\n",
        )
        .unwrap();

        let prod = link_file(suite.to_str().unwrap()).expect_err("production link must reject");
        assert!(prod.contains("sealed type") && prod.contains("Version"), "{prod}");

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap()).expect("test mode links");
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(passed, vec!["suite.test_constructs_domain_edge_case".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn witchy_test_prunes_unused_effectful_main_under_zero_grant() {
        let dir = unique_dir("zero_grant_prunes_unused_effects");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn main(console: Console, root: Dir[Read]):\n    \
             console.print(root.read(\"secret.txt\"))\n\n\
             fn test_pure_logic():\n    \
             testing.assert_int_eq(2 + 2, 4)\n",
        )
        .unwrap();

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap())
            .expect("unused effectful main is replaced by the test driver");
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(passed, vec!["suite.test_pure_logic".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn witchy_test_zero_grant_keeps_abort_diagnostics() {
        let dir = unique_dir("zero_grant_abort");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn test_failure_message():\n    testing.fail_with(\"boom\")\n",
        )
        .unwrap();

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap())
            .expect("abort host import is authority-free under zero grant");
        assert!(passed.is_empty(), "{passed:?}");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "suite.test_failure_message");
        assert!(failed[0].1.contains("boom"), "{failed:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn testing_mock_dir_is_test_mode_only() {
        let dir = unique_dir("mock_dir_test_only");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn main():\n    \
             let root = testing.mock_dir([(\"config.txt\", \"ok\")])\n    \
             testing.assert_eq(root.read(\"config.txt\"), \"ok\")\n",
        )
        .unwrap();

        let err = link_file(suite.to_str().unwrap()).expect_err("production link must reject");
        assert!(err.contains("testing.mock_dir") && err.contains("witchy test"), "{err}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn witchy_test_mock_dir_reads_in_memory_tree_under_zero_grant() {
        let dir = unique_dir("mock_dir_zero_grant");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let suite = dir.join("suite.witchy");
        std::fs::write(
            &suite,
            "import testing\n\n\
             fn read_config(root: Dir[Read]) -> String:\n    \
             root.read(\"app/config.txt\")\n\n\
             fn test_mock_dir_read_surface():\n    \
             let root = testing.mock_dir([\n        \
             (\"app/config.txt\", \"ok\"),\n        \
             (\"app/nested/name.txt\", \"Ada\"),\n        \
             (\"README.md\", \"top\")\n    \
             ])\n    \
             testing.assert_eq(read_config(root), \"ok\")\n    \
             testing.assert(root.exists(\"app/config.txt\"), \"file exists\")\n    \
             testing.assert(root.is_dir(\"app\"), \"directory exists\")\n    \
             testing.assert(!root.exists(\"missing.txt\"), \"missing path is false\")\n    \
             let app = root.subtree(\"app\")\n    \
             testing.assert_value_eq(app.list(), [\"config.txt\", \"nested\"])\n    \
             let file = app.read_file(\"nested/name.txt\")\n    \
             testing.assert_eq(file.read(), \"Ada\")\n",
        )
        .unwrap();

        let (passed, failed) = run_tests_in_file(suite.to_str().unwrap())
            .expect("mock Dir runs under zero real grants");
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(passed, vec!["suite.test_mock_dir_read_surface".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[derive(Debug, Default)]
struct TestPackageOwnership {
    dependency_roots: Vec<std::path::PathBuf>,
}

impl TestPackageOwnership {
    fn resolve(target: &str) -> Result<Self, String> {
        let target = std::fs::canonicalize(target)
            .map_err(|e| format!("cannot resolve test target `{target}`: {e}"))?;
        let start = if target.is_dir() {
            target.as_path()
        } else {
            target.parent().unwrap_or_else(|| std::path::Path::new("."))
        };
        let Some(package_root) = package_root_for(start) else {
            return Ok(Self::default());
        };
        let mut roots = std::collections::BTreeSet::new();
        let mut visited = std::collections::HashSet::new();
        collect_resolved_dependency_roots(&package_root, &mut roots, &mut visited)?;
        Ok(Self { dependency_roots: roots.into_iter().collect() })
    }

    fn owns(&self, file: &str) -> Result<bool, String> {
        let file = std::fs::canonicalize(file)
            .map_err(|e| format!("cannot resolve test file `{file}`: {e}"))?;
        Ok(!self.dependency_roots.iter().any(|root| file.starts_with(root)))
    }
}

fn package_root_for(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut at = Some(start);
    while let Some(dir) = at {
        if dir.join("witchy.toml").is_file() {
            return std::fs::canonicalize(dir).ok();
        }
        at = dir.parent();
    }
    None
}

fn dependency_alias(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

fn locked_registry_aliases(package_root: &std::path::Path) -> Result<std::collections::HashSet<String>, String> {
    let path = package_root.join("witchy.lock");
    if !path.is_file() {
        return Ok(std::collections::HashSet::new());
    }
    let source = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read `{}`: {e}", path.display()))?;
    let lock: toml::Value = toml::from_str(&source)
        .map_err(|e| format!("cannot parse `{}`: {e}", path.display()))?;
    let mut aliases = std::collections::HashSet::new();
    for entry in lock.get("rune").and_then(toml::Value::as_array).into_iter().flatten() {
        if entry.get("source").and_then(toml::Value::as_str) != Some("coven") {
            continue;
        }
        let alias = entry
            .get("alias")
            .and_then(toml::Value::as_str)
            .or_else(|| entry.get("name").and_then(toml::Value::as_str).map(dependency_alias));
        if let Some(alias) = alias {
            aliases.insert(alias.to_string());
        }
    }
    Ok(aliases)
}

fn resolved_dependency_dirs(package_root: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let manifest_path = package_root.join("witchy.toml");
    let source = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read `{}`: {e}", manifest_path.display()))?;
    let manifest: toml::Value = toml::from_str(&source)
        .map_err(|e| format!("cannot parse `{}`: {e}", manifest_path.display()))?;
    let Some(dependencies) = manifest.get("dependencies").and_then(toml::Value::as_table) else {
        return Ok(Vec::new());
    };
    let registry_aliases = locked_registry_aliases(package_root)?;
    let mut dirs = Vec::new();
    for (name, declaration) in dependencies {
        let resolved = declaration
            .as_table()
            .and_then(|inline| inline.get("path"))
            .and_then(toml::Value::as_str)
            .map(|path| package_root.join(path))
            .or_else(|| {
                let alias = dependency_alias(name);
                registry_aliases
                    .contains(alias)
                    .then(|| package_root.join("vendor").join(alias))
            });
        if let Some(dir) = resolved
            && dir.is_dir()
        {
            dirs.push(
                std::fs::canonicalize(&dir)
                    .map_err(|e| format!("cannot resolve dependency `{}`: {e}", dir.display()))?,
            );
        }
    }
    Ok(dirs)
}

fn collect_resolved_dependency_roots(
    package_root: &std::path::Path,
    roots: &mut std::collections::BTreeSet<std::path::PathBuf>,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) -> Result<(), String> {
    let package_root = std::fs::canonicalize(package_root)
        .map_err(|e| format!("cannot resolve package `{}`: {e}", package_root.display()))?;
    if !visited.insert(package_root.clone()) {
        return Ok(());
    }
    for dependency in resolved_dependency_dirs(&package_root)? {
        roots.insert(dependency.clone());
        collect_resolved_dependency_roots(&dependency, roots, visited)?;
    }
    Ok(())
}

/// `witchy test <file|dir>`: run in-language tests, print a cargo-style
/// report, and return whether everything passed.
pub(crate) fn run_tests(options: &TestOptions) -> Result<bool, String> {
    let path = &options.path;
    let mut files: Vec<String> = Vec::new();
    let meta = std::fs::metadata(path).map_err(|e| format!("cannot read `{path}`: {e}"))?;
    let ownership = TestPackageOwnership::resolve(path)?;
    if meta.is_dir() {
        // Collect every `.witchy` under the directory recursively, so a rune's
        // `src/` modules (and the nested runes of a multi-rune project) are all
        // discovered — `witchy test <rune-dir>` runs the whole package's tests.
        fn collect(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
            let mut entries: Vec<_> =
                std::fs::read_dir(dir)?.filter_map(|e| e.ok()).map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                if p.is_dir() {
                    collect(&p, out)?;
                } else if p.extension().and_then(|s| s.to_str()) == Some("witchy") {
                    out.push(p);
                }
            }
            Ok(())
        }
        let mut paths = Vec::new();
        collect(std::path::Path::new(path), &mut paths)
            .map_err(|e| format!("cannot read `{path}`: {e}"))?;
        files.extend(paths.into_iter().filter_map(|p| p.to_str().map(String::from)));
    } else {
        files.push(path.to_string());
    }
    let mut total_pass = 0usize;
    let mut total_fail = 0usize;
    for file in &files {
        // Distinguish a LINK failure from a post-link (compile) failure (BUG-120).
        // In a directory sweep, a file that can't LINK standalone — a module of a
        // multi-rune project that imports a sibling rune via a path dependency, which
        // resolves no local `<import>.witchy` — is skipped, not fatal. But a file
        // that links yet fails to TYPE-CHECK (or violates `mode opt`) is a genuinely
        // BROKEN test file: it must FAIL the run, never be silently skipped as
        // "ok. 0 passed". An explicit single file surfaces even a link error.
        let (linked, stem) = match link_file_with_mode(file, linker::LinkMode::Test) {
            Ok(v) => v,
            Err(e) if meta.is_dir() => {
                eprintln!("  skipped {file}: {e}");
                continue;
            }
            Err(e) => return Err(e),
        };
        let (async_tests, gen_tests) = raw_test_shapes(file);
        let owns_test = ownership.owns(file)?;
        let no_real_grants = TestGrants::default();
        let grants = if owns_test { &options.grants } else { &no_real_grants };
        let policy = TestRunPolicy {
            integration: options.integration,
            real_grants: options.integration && owns_test,
            grants,
        };
        let (passed, failed) = match run_tests_in_module(
            &linked,
            &stem,
            &async_tests,
            &gen_tests,
            policy,
        ) {
            Ok(r) => r,
            Err(e) => {
                // Linked OK but broken (a type error or a `mode opt` violation): count
                // it as a failure so the run exits non-zero (BUG-120).
                println!("running test(s) in {file}");
                println!("test {file} ... FAILED to compile: {e}");
                total_fail += 1;
                continue;
            }
        };
        if passed.is_empty() && failed.is_empty() {
            continue;
        }
        println!("running {} test(s) in {file}", passed.len() + failed.len());
        for name in &passed {
            println!("test {name} ... ok");
        }
        for (name, msg) in &failed {
            println!("test {name} ... FAILED: {msg}");
        }
        total_pass += passed.len();
        total_fail += failed.len();
    }
    println!(
        "\ntest result: {}. {total_pass} passed; {total_fail} failed",
        if total_fail == 0 { "ok" } else { "FAILED" }
    );
    Ok(total_fail == 0)
}
