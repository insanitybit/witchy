use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn documentation_edits_do_not_run_the_deferred_source_interpreter_sweep() {
    for path in ["README.md", "spec/language.md"] {
        let commands = selected(&[path]);
        assert!(
            !commands
                .iter()
                .any(|command| command.contains("documentation_examples_are_valid")),
            "{path} selected the frozen source-interpreter documentation sweep: {commands:?}",
        );
    }
}

fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for tests"),
    );
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("merge-queue-tests lives under tools/")
        .to_path_buf()
}

struct TempRepo(PathBuf);

impl TempRepo {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "witchy-test-for-paths-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(path.join("scripts")).expect("create fixture scripts");
        fs::copy(
            workspace_root().join("scripts/test-for-paths.sh"),
            path.join("scripts/test-for-paths.sh"),
        )
        .expect("copy path router");
        let impact = workspace_root().join("scripts/test-impact.py");
        if impact.exists() {
            fs::copy(impact, path.join("scripts/test-impact.py")).expect("copy test-impact");
        }
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git").current_dir(root).args(args).output().expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn route(paths: &[&str]) -> String {
    let script = workspace_root().join("scripts/test-for-paths.sh");
    let output = Command::new("bash")
        .arg(script)
        .args(paths)
        .output()
        .expect("run test-for-paths.sh");
    assert!(
        output.status.success(),
        "test router failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("router output is UTF-8")
}

fn selected(paths: &[&str]) -> Vec<String> {
    route(paths)
        .lines()
        .filter_map(|line| line.strip_prefix("  ").map(str::to_owned))
        .collect()
}

fn route_focused(paths: &[&str]) -> String {
    let script = workspace_root().join("scripts/test-for-paths.sh");
    let output = Command::new("bash")
        .arg(script)
        .arg("--focused")
        .args(paths)
        .output()
        .expect("run test-for-paths.sh --focused");
    assert!(
        output.status.success(),
        "focused test router failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("router output is UTF-8")
}

fn selected_focused(paths: &[&str]) -> Vec<String> {
    route_focused(paths)
        .lines()
        .filter_map(|line| line.strip_prefix("  ").map(str::to_owned))
        .collect()
}

#[test]
fn whole_diff_runs_record_only_clean_successful_receipts() {
    let output = Command::new("bash")
        .current_dir(workspace_root())
        .arg("scripts/test-path-receipts.sh")
        .output()
        .expect("run path receipt fixture");
    assert!(
        output.status.success(),
        "path receipt fixture failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn gate_nextest(paths: &[&str]) -> String {
    let script = workspace_root().join("scripts/test-for-paths.sh");
    let output = Command::new("bash")
        .arg(script)
        .arg("--gate-nextest")
        .args(paths)
        .output()
        .expect("run test-for-paths.sh --gate-nextest");
    assert!(
        output.status.success(),
        "gate-nextest router failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("gate-nextest output is UTF-8")
}

#[test]
fn gate_nextest_example_tests_only_is_not_workspace() {
    let output = gate_nextest(&["src/example_tests/rfc0167_views.rs"]);
    assert!(
        output.contains("example_tests::rfc0167_views"),
        "example_tests-only mapping did not partition to the touched module: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)") || output.contains("rfc0167_views"),
        "example_tests-only mapping kept the whole matrix: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "example_tests-only mapping fell back to the full workspace: {output}",
    );
}

#[test]
fn compiler_edit_loop_runs_one_example_slice_and_landing_runs_all() {
    let sliced = selected(&["--edit-loop", "crates/witchy-types/src/typeck.rs"]);
    assert!(
        sliced.iter().any(|c| c.contains("--partition hash:1/8")
            && c.contains("test(/^example_tests::/)")),
        "compiler edit loop did not slice the example matrix: {sliced:?}",
    );
    let output = Command::new("bash")
        .arg(workspace_root().join("scripts/test-for-paths.sh"))
        .args(["--edit-loop", "crates/witchy-types/src/typeck.rs"])
        .env("WITCHY_EDIT_EXAMPLES", "full")
        .output()
        .expect("run test-for-paths.sh");
    let full = String::from_utf8(output.stdout).expect("router output is UTF-8");
    assert!(
        full.contains("test(/^example_tests::/)") && !full.contains("--partition"),
        "WITCHY_EDIT_EXAMPLES=full did not restore the complete matrix: {full}",
    );
    let landing = gate_nextest(&["crates/witchy-types/src/typeck.rs"]);
    assert!(!landing.contains("--partition"), "landing gate sliced the examples: {landing}");
}

#[test]
fn root_example_tests_edit_builds_only_the_main_bin() {
    let commands = selected(&["--edit-loop", "src/example_tests.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].contains("--bin witchy")
            && commands[0].contains("test(/^example_tests::/)"),
        "root example-test edit rebuilt unrelated package targets: {commands:?}",
    );
}

#[test]
fn network_example_edits_run_only_the_network_module() {
    let commands = selected(&["--edit-loop", "src/example_tests/network.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].contains("test(/^example_tests::network::/)")
            && commands[0].contains("--bin witchy"),
        "network edit did not select the focused bin probe: {commands:?}"
    );
    assert!(
        !commands[0].contains("test(/^example_tests::/)")
            || commands[0].contains("test(/^example_tests::network::/)"),
        "network edit expanded to the full example matrix: {commands:?}"
    );
}

/// `-p witchy` makes cargo BUILD all 24 of the package's test binaries even
/// when the run is one test from one of them. Measured 2026-08-27: a docs gate
/// spent 7m14s compiling to reach "Starting 1 test across 24 binaries" while
/// holding the serialized gate lock; `--bin witchy` builds that binary alone
/// (41s -> 12s warm). Safe only because `mod example_tests;` lives in
/// src/main.rs, which IS the bin target.
#[test]
fn gate_nextest_docs_only_builds_the_bin_target() {
    for path in ["book/src/ch01.md", "std/list.witchy", "README.md"] {
        let output = gate_nextest(&[path]);
        assert!(
            output.contains("--bin witchy"),
            "{path} did not narrow the build to the bin target: {output}",
        );
    }
}

#[test]
fn gate_nextest_docs_plus_integration_test_keeps_both_target_kinds() {
    let output = gate_nextest(&[
        "spec/architecture-ledger.md",
        "tools/repo-tests/tests/repo_hygiene/architecture.rs",
        "scripts/test-for-paths.sh",
    ]);
    let args = output.lines().next().unwrap_or_default();
    assert!(args.contains("-p witchy"), "root package was omitted: {output}");
    assert!(args.contains("-p repo-tests"), "repository contracts were omitted: {output}");
    for required in ["--lib", "--test repo_hygiene", "--bin witchy"] {
        assert!(
            args.contains(required),
            "explicit target union omitted {required}: {output}",
        );
    }
    assert!(
        output.contains("package(repo-tests)")
            && output.contains("documentation_examples_are_valid"),
        "mixed selection omitted a required target: {output}",
    );
}

/// The narrowing must stay off wherever the selection can name a target outside
/// the bin. `--bin` ANDs with `-p` instead of unioning, so a wrong narrowing
/// does not error -- it silently runs a subset and reports green. Main-only
/// modules, however, should never pay to build the other root test targets.
#[test]
fn gate_nextest_does_not_narrow_past_the_bin_target() {
    // A crate package plus the rfc0* conformance binaries.
    // It may name `--bin witchy` (for `example_tests::rfc0*`) only inside the
    // explicit union of every target: `--lib` plus each test binary.
    let compiler = gate_nextest(&["crates/witchy-lower/src/codegen/mod.rs"]);
    assert!(
        !compiler.contains("--bin witchy")
            || (compiler.contains(" --lib") && compiler.contains("--test misc")),
        "a compiler-stage change narrowed away its crate and conformance tests: {compiler}",
    );
    for path in ["src/lsp.rs", "src/diagnostic_golden_tests.rs", "src/commands/test.rs"] {
        let output = gate_nextest(&[path]);
        assert!(
            output.contains("--bin witchy"),
            "main-only tests still build every root target for {path}: {output}",
        );
    }
}

/// The narrowing above is only sound while these modules are compiled into the
/// bin target. Assert that structural fact rather than trusting it to stay true.
#[test]
fn focused_main_modules_are_declared_in_the_bin_target() {
    let main = std::fs::read_to_string(
        workspace_root().join("src/main.rs"),
    )
    .expect("read src/main.rs");
    for module in ["commands", "lsp", "diagnostic_golden_tests", "example_tests"] {
        assert!(
            main.contains(&format!("mod {module};")),
            "{module} is no longer declared in the bin target; \
             scripts/test-for-paths.sh narrows its gates to `--bin witchy` on that basis",
        );
    }
}

#[test]
fn focused_main_routes_build_only_the_bin_target() {
    for path in [
        "src/lsp.rs",
        "src/diagnostic_golden_tests.rs",
        "std/list.witchy",
        "spec/stdlib.md",
    ] {
        let commands = selected_focused(&[path]);
        let main_only = commands
            .iter()
            .filter(|command| {
                command.contains("lsp")
                    || command.contains("diagnostic_golden_tests")
                    || command.contains("stdlib_docs_are_current")
            })
            .collect::<Vec<_>>();
        assert!(!main_only.is_empty(), "{path} selected no main-only test: {commands:?}");
        for command in main_only {
            assert!(
                command.contains("--bin witchy"),
                "{path} left a main-only test route broad: {commands:?}",
            );
        }
    }
}

#[test]
fn gate_nextest_crate_plus_example_tests_unions_without_workspace() {
    let output = gate_nextest(&[
        "crates/witchy-interp/src/lib.rs",
        "src/example_tests/rfc0167_views.rs",
    ]);
    let lines: Vec<_> = output.lines().collect();
    assert!(
        output.contains("-p witchy-interp"),
        "crate+example_tests mapping dropped the crate: {output}",
    );
    assert!(
        output.contains("example_tests::rfc0167_views"),
        "crate+example_tests mapping did not partition to the touched module: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)"),
        "crate+file mapping still selected the whole example_tests matrix: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "crate+example_tests mapping fell back to the full workspace: {output}",
    );
    assert!(
        lines.len() >= 3
            && lines[2].contains("-p witchy-interp")
            && !lines[2].contains("-p witchy "),
        "compile-check package line should be mapped crates only: {output}",
    );
}

#[test]
fn gate_nextest_queue_scripts_are_not_workspace() {
    let output = gate_nextest(&["scripts/check.sh", "scripts/test-for-paths.sh"]);
    assert!(
        output.contains("--test merge_queue") || output.contains("--test test_for_paths"),
        "queue-script mapping dropped the focused tests: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "queue-script mapping fell back to the full workspace: {output}",
    );
}

#[test]
fn gate_nextest_types_only_is_not_unfiltered_workspace() {
    let output = gate_nextest(&["crates/witchy-types/src/lib.rs"]);
    assert!(
        output.contains("-p witchy-types"),
        "types-only gate mapping dropped the crate filter: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)"),
        "types-only crate mapping still auto-attached the example_tests matrix: {output}",
    );
    assert!(
        !output.contains("--workspace") && !output.contains("WORKSPACE"),
        "types-only gate mapping fell back to the full workspace: {output}",
    );
}

#[test]
fn gate_nextest_crate_only_interp_does_not_attach_example_tests_matrix() {
    let output = gate_nextest(&["crates/witchy-interp/src/lib.rs"]);
    assert!(
        output.contains("-p witchy-interp"),
        "interp-only mapping dropped the crate: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)"),
        "interp-only crate mapping still auto-attached the example_tests matrix: {output}",
    );
    assert!(
        !output.contains("WORKSPACE"),
        "interp-only mapping fell back to WORKSPACE: {output}",
    );
}

#[test]
fn gate_nextest_compiler_crate_edits_run_the_std_pruning_root_census() {
    // d92c12c45 added a std name literal in witchy-interp; the crate-scoped
    // gate never reached witchy-types' census of those literals and master
    // went red on it.
    let census = "test(=std_pruning::tests::pruning_roots_cover_every_compiler_std_literal)";
    for path in [
        "crates/witchy-interp/src/interpreter/reflection.rs",
        "crates/witchy-lower/src/codegen/expr_lower.rs",
        "crates/witchy-wir/src/lib.rs",
        "src/lib.rs",
    ] {
        let output = gate_nextest(&[path]);
        assert!(
            output.contains("-p witchy-types") && output.contains(census),
            "{path} did not select the std-pruning root census: {output}",
        );
        // witchy-types' lib target is `test = false`; the census lives in its
        // `typeck` integration binary, which a narrowed `--lib` selection
        // must name explicitly or the expression silently matches nothing.
        let selection = output.lines().next().unwrap_or_default();
        let words: Vec<&str> = selection.split_whitespace().collect();
        assert!(
            !words.contains(&"--lib") || words.windows(2).any(|pair| pair == ["--test", "typeck"]),
            "{path} narrowed targets without the census binary: {output}",
        );
    }
    let output = gate_nextest(&["std/meta.witchy"]);
    assert!(!output.contains(census), "a std-only edit selected the census: {output}");
}

#[test]
fn gate_nextest_lower_only_does_not_attach_wir_or_runtime() {
    let output = gate_nextest(&["crates/witchy-lower/src/codegen/expr_lower.rs"]);
    assert!(
        output.contains("-p witchy-lower"),
        "lower-only mapping dropped the crate: {output}",
    );
    assert!(
        !output.contains("witchy-wir") && !output.contains("witchy-runtime"),
        "lower-only mapping still attached wir/runtime: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)") && !output.contains("WORKSPACE"),
        "lower-only mapping was not crate-scoped: {output}",
    );
}

#[test]
fn gate_nextest_crate_plus_integration_test_unions_without_workspace() {
    let output = gate_nextest(&[
        "crates/witchy-lower/src/codegen/builtins.rs",
        "tests/rfc0122.rs",
    ]);
    assert!(
        output.contains("-p witchy-lower"),
        "crate+integration mapping dropped the crate: {output}",
    );
    assert!(
        output.contains("binary(rfc0122)") || output.contains("--test rfc0122"),
        "crate+integration mapping dropped the integration test: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "crate+integration mapping fell back to the full workspace: {output}",
    );
}

#[test]
fn gate_nextest_nested_integration_modules_use_root_targets() {
    let output = gate_nextest(&[
        "crates/witchy-types/src/typeck.rs",
        "tests/rfc0082/dynamic_methods.rs",
    ]);
    assert!(output.contains("binary(rfc0082)"), "missing rfc0082 root target: {output}");
    assert!(!output.contains("rfc0082/dynamic_methods"), "nested path became a binary selector: {output}");
}

#[test]
fn gate_nextest_spec_language_is_not_workspace() {
    let output = gate_nextest(&["spec/language.md"]);
    assert!(
        output.contains("documentation_examples_are_valid"),
        "spec/language.md mapping did not select the documentation sweep: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "spec/language.md mapping fell back to the full workspace: {output}",
    );
}

#[test]
fn gate_nextest_compiler_std_web_and_rfc0122_is_not_workspace() {
    let output = gate_nextest(&[
        "crates/witchy-lower/src/codegen/builtins.rs",
        "crates/witchy-types/src/typeck.rs",
        "std/list.witchy",
        "src/commands/web/tests.rs",
        "tests/rfc0122.rs",
        "web/witchy-runtime/glamour-docs-bundle.test.mjs",
        "spec/stdlib.md",
    ]);
    assert!(
        output.contains("-p witchy-lower") && output.contains("-p witchy-types"),
        "multi-surface mapping dropped compiler crates: {output}",
    );
    assert!(
        output.contains("binary(rfc0122)") || output.contains("binary(glamour)"),
        "multi-surface mapping dropped integration tests: {output}",
    );
    assert!(
        output.contains("example_tests") || output.contains("stdlib_docs_are_current"),
        "multi-surface mapping dropped std/spec tests: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "multi-surface mapping fell back to the full workspace: {output}",
    );
}

#[test]
fn gate_nextest_std_keeps_the_example_tests_matrix() {
    let output = gate_nextest(&["std/json.witchy"]);
    assert!(
        output.contains("test(/^example_tests::/)"),
        "std/json.witchy must keep the full example_tests matrix: {output}",
    );
    assert!(
        !output.contains("WORKSPACE"),
        "std mapping fell back to WORKSPACE: {output}",
    );
}

#[test]
fn gate_nextest_book_is_documentation_examples_not_the_matrix() {
    let output = gate_nextest(&["book/src/tour-values.md"]);
    assert!(
        output.contains("documentation_examples_are_valid"),
        "book change did not select documentation_examples_are_valid: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)"),
        "book change still selected the full example_tests matrix: {output}",
    );
    assert!(!output.contains("WORKSPACE"), "book mapping was WORKSPACE: {output}");
}

#[test]
fn gate_nextest_readme_is_documentation_examples_not_the_matrix() {
    let output = gate_nextest(&["README.md"]);
    assert!(
        output.contains("documentation_examples_are_valid"),
        "README.md did not select documentation_examples_are_valid: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)"),
        "README.md still selected the full example_tests matrix: {output}",
    );
}

#[test]
fn gate_nextest_examples_are_not_the_full_matrix() {
    let output = gate_nextest(&["examples/calc/src/calc.witchy"]);
    assert!(
        output.contains("examples_programs") || output.contains("all_examples_validate"),
        "examples/ change dropped the example corpus tests: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)"),
        "examples/ change still selected the full example_tests matrix: {output}",
    );
    assert!(!output.contains("WORKSPACE"), "examples mapping was WORKSPACE: {output}");
}

#[test]
fn rust_integration_tests_use_fast_without_redundant_binary_run() {
    let output = route(&["tests/differential_fuzz.rs"]);
    assert!(output.contains("./scripts/check.sh --fast"));
    assert!(!output.contains("cargo nextest run --test differential_fuzz"));
    assert!(!output.contains("prose-only"));
}

#[test]
fn new_root_integration_driver_checks_the_binary_budget_first() {
    let commands = selected(&["tests/new_velocity_probe_driver.rs"]);
    assert_eq!(
        commands.first().map(String::as_str),
        Some("cargo nextest run -p repo-tests --test repo_hygiene -E 'test(architecture::root_integration_test_binaries_stay_within_budget)'")
    );
    assert!(commands.contains(&"./scripts/check.sh --fast".to_owned()), "{commands:?}");

    let existing = selected(&["tests/differential_fuzz.rs"]);
    assert!(
        !existing.iter().any(|command| command.contains("root_integration_test_binaries_stay_within_budget")),
        "an existing driver edit cannot increase the binary count: {existing:?}"
    );
}

#[test]
fn authenticated_owner_surfaces_keep_the_cli_contract_probe() {
    for path in [
        "src/commands/compile.rs",
        "src/commands/project_resolver_tests.rs",
        "src/source.rs",
        "src/source/package_inputs.rs",
    ] {
        let commands = selected(&["--edit-loop", path]);
        let owner_probe = if path == "src/source.rs" || path == "src/source/package_inputs.rs" {
            &commands[1]
        } else {
            &commands[0]
        };
        assert_eq!(
            owner_probe,
            "cargo nextest run --test cli_subcommands -E 'test(=compile_accepts_complete_authenticated_package_owners)'",
            "{path}: {commands:?}"
        );
        if path != "src/source/package_inputs.rs" {
            assert!(
                commands
                    .iter()
                    .skip(1)
                    .any(|command| command.contains("nextest-admitted.sh")),
                "{path}: {commands:?}"
            );
        }
    }
}

#[test]
fn packaged_source_surfaces_run_the_unsafe_input_probe_first() {
    let commands = selected(&[
        "--edit-loop",
        "rfcs/0165-acceptance-ledger.md",
        "src/source.rs",
        "src/source/package_inputs.rs",
        "src/source/package_inputs/registry.rs",
        "src/source/package_inputs/registry_replay_tests.rs",
        "src/source/package_inputs/registry_verify.witchy",
        "src/trusted_publisher/source_inputs.rs",
    ]);
    assert_eq!(
        commands[0],
        "cargo nextest run --test misc -E 'test(=rfc0165_packaged_source::packaged_source_rejects_custom_owner_and_unsafe_input_paths)'",
        "{commands:?}"
    );
    assert!(commands.iter().any(|command| command.contains("source::tests")), "{commands:?}");
    assert!(commands.iter().any(|command| command.contains("cli_subcommands")), "{commands:?}");
}

#[test]
fn canonical_rewrite_edit_loop_stays_bounded_across_stage_files() {
    let commands = selected(&[
        "--edit-loop",
        "crates/witchy-types/src/typeck.rs",
        "crates/witchy-lower/src/canonical_pipeline.rs",
        "crates/witchy-wir/src/canonical_safewir.rs",
        "crates/witchy-runtime/src/runtime/host/console.rs",
        "src/commands/execution.rs",
        "src/commands/embedded_pm.rs",
        "tests/misc/rfc0165_oracle.rs",
        "spec/wasm-abi.md",
        "rfcs/0165-rule-manifest.md",
        "security-eval/findings/canonical-pop.md",
    ]);
    assert_eq!(commands.len(), 3, "cross-stage edit loop was not bounded: {commands:?}");
    assert!(
        !commands.iter().any(|command| {
            command.contains("test(/^example_tests::/)")
                || command == "cargo nextest run --test misc"
                || command.contains("check.sh --wasm")
        }),
        "checkpoint work leaked into the edit loop: {commands:?}",
    );
    assert!(
        commands[0].contains("--test rfc0165_route_gate")
            && commands[0].contains("test(boolean_reaches::)")
            && commands[0]
                .contains("canonical_wasm_encoding_is_private_and_requires_final_authority"),
        "the focused route omitted the final-authority admission boundary: {commands:?}",
    );
    assert!(
        commands[2].contains("rfc0165_enum_equality::"),
        "the focused route omitted the enum equality semantic regression: {commands:?}",
    );
    assert!(
        !commands[0].contains("outlined_address_predicate_mutation_fails_independent_admission"),
        "the expensive WasmIR tail probe leaked into unrelated canonical edits: {commands:?}",
    );
    assert!(
        commands[2].starts_with("WITCHY_ORACLE_EXAMPLE=calc ")
            && commands[2].contains(
                "test(=rfc0165_oracle::oracle_agrees_on_the_admitted_examples_shard_3)",
            ),
        "the focused route omitted the filtered production oracle: {commands:?}",
    );
}

#[test]
fn canonical_wasmir_landing_keeps_the_deferred_mutation_tests() {
    let output = gate_nextest(&["crates/witchy-wir/src/canonical_wasmir.rs"]);
    assert!(
        output.lines().next().is_some_and(|line| line.contains("-p witchy-wir")),
        "canonical WasmIR landing did not retain the full WIR package: {output}",
    );
    // Package-wide means EVERY witchy-wir target, not "no target flags". The
    // gate now names every target of every selected package (target filters
    // union, so that keeps the exact test set while skipping unneeded root
    // binaries). What must never happen is naming a SUBSET: either no target
    // flags at all, or `--lib` plus every integration target on disk.
    let selection = output.lines().next().unwrap_or_default();
    if selection.contains("--lib") || selection.contains("--test ") {
        assert!(selection.contains("--lib"), "WIR unit tests dropped: {output}");
        let tests = workspace_root().join("crates/witchy-wir/tests");
        for entry in std::fs::read_dir(tests).expect("list witchy-wir tests") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_some_and(|extension| extension == "rs") {
                let name = path.file_stem().unwrap().to_string_lossy().into_owned();
                assert!(
                    selection.split_whitespace().collect::<Vec<_>>().windows(2)
                        .any(|pair| pair == ["--test", name.as_str()]),
                    "canonical WasmIR landing was narrowed away from `{name}`: {output}",
                );
            }
        }
    }
}

#[test]
fn canonical_console_fixture_edit_loop_stays_on_its_three_contracts() {
    let commands = selected(&[
        "--edit-loop",
        "crates/witchy-interp/src/interpreter/runners.rs",
        "crates/witchy-runtime/src/runtime/console_fixtures.rs",
        "crates/witchy-test-host/src/lib.rs",
        "crates/witchy-types/src/pipeline/test_driver/console_fixture.rs",
        "crates/witchy-types/src/pipeline/test_driver/console_fixture/arguments.rs",
        "scripts/rfc0165-deferred-tests.txt",
        "src/commands/test_runner/canonical_console_fixture.rs",
    ]);
    assert_eq!(commands.len(), 3, "fixture edit loop was not bounded: {commands:?}");
    assert!(
        commands[0].contains("native_host_links_every_catalog_import_with_the_declared_signature"),
        "fixture edit loop omitted the ABI linkage contract: {commands:?}",
    );
    assert!(
        commands[1].contains("-p witchy-test-host -p witchy-runtime")
            && commands[1].contains("runtime::console_fixtures::tests::"),
        "fixture edit loop omitted host/runtime ownership and bounds contracts: {commands:?}",
    );
    assert!(
        commands[2].starts_with("./scripts/nextest-admitted.sh witchy -- ")
            && commands[2].contains("--bin witchy")
            && commands[2].contains("canonical_console_fixture_tests::"),
        "fixture edit loop omitted admitted source/native semantics: {commands:?}",
    );
    assert!(
        !commands.iter().any(|command| {
            command.contains("test(/^example_tests::/)")
                || command.contains("test(/^commands::/)")
                || command.contains("check.sh --wasm")
        }),
        "checkpoint work leaked into the fixture edit loop: {commands:?}",
    );
}

#[test]
fn broad_rfc0165_edit_checks_share_the_landing_admission_policy() {
    // Representative paths from impl/rfc0165-project-crypto-admission. Its
    // unfiltered edit check failed only the ten legacy lowerer tests that the
    // landing gate deliberately defers; the filtered suite passed 454 tests
    // and skipped those ten in 25.3s.
    let commands = selected(&[
        "--edit-loop",
        "crates/witchy-lower/src/canonical_crypto.rs",
        "crates/witchy-runtime/src/runtime/host/crypto.rs",
        "crates/witchy-wir/src/canonical_safewir/crypto.rs",
        "src/commands/compile.rs",
        "tests/misc/rfc0165_crypto.rs",
        "projects/grimoire/src/grimoire.witchy",
        "Cargo.lock",
    ]);
    for command in &commands {
        if command.contains("-p witchy-lower --features witchy-wir/native")
            || command.contains("test(/^example_tests::/)")
            || command.contains("test(/^commands::/)")
            || command.ends_with("cargo nextest run --test misc")
        {
            assert!(
                command.starts_with("./scripts/nextest-admitted.sh "),
                "broad command bypassed the landing admission policy: {command}"
            );
        }
    }

    // Named authority/ABI/oracle probes must remain mandatory even when their
    // package also has deferred legacy families.
    let security = selected(&[
        "--edit-loop",
        "crates/witchy-wir/src/wir_prelude.rs",
        "tests/misc/rfc0165_oracle.rs",
    ]);
    assert!(
        security
            .iter()
            .all(|command| !command.starts_with("./scripts/nextest-admitted.sh ")),
        "exact security probes were filtered: {security:?}"
    );
    assert!(
        security
            .iter()
            .any(|command| command.contains("--ignore-default-filter")),
        "oracle probe lost its mandatory override: {security:?}"
    );
}

#[test]
fn canonical_crypto_edit_loop_keeps_rsa_contracts() {
    let commands = selected(&[
        "--edit-loop",
        "crates/witchy-lower/src/canonical_crypto.rs",
        "crates/witchy-runtime/src/runtime/host/crypto_rsa_slices_tests.rs",
        "crates/witchy-wir/src/canonical_interpreter/crypto_rsa_tests.rs",
        "tests/misc/rfc0165_crypto_rsa.rs",
        "spec/wasm-abi.md",
    ]);
    assert_eq!(commands.len(), 4, "RSA edit route changed shape: {commands:?}");
    assert!(
        commands[0].contains("canonical_interpreter::crypto_rsa_tests::"),
        "RSA interpreter coverage was omitted: {commands:?}",
    );
    assert!(
        commands[2].contains("rfc0165_crypto(_rsa)?::"),
        "RSA admitted misc coverage was omitted: {commands:?}",
    );
    assert!(
        commands[3].contains("runtime::host::crypto_rsa_slices_tests::"),
        "RSA host-slice coverage was omitted: {commands:?}",
    );
}

#[test]
fn oracle_harness_edit_runs_only_the_filtered_production_oracle() {
    let commands = selected(&["--edit-loop", "tests/misc/rfc0165_oracle.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(commands[0].starts_with("WITCHY_ORACLE_EXAMPLE=calc "), "{commands:?}");
    assert!(
        commands[0]
            .contains("test(=rfc0165_oracle::oracle_agrees_on_the_admitted_examples_shard_3)"),
        "{commands:?}",
    );
}

#[test]
fn server_pool_oracle_edit_keeps_its_authority_and_execution_probes() {
    let commands = selected(&[
        "--edit-loop",
        "tests/misc/rfc0165_server_pool_oracle.rs",
    ]);
    assert_eq!(commands.len(), 3, "{commands:?}");
    assert!(
        commands[2].contains("test(rfc0165_owner_adapters::server_pool_oracle::/)"),
        "{commands:?}",
    );
}

#[test]
fn owner_adapter_edit_replays_the_raw_wasm_admission_probe_early() {
    let commands = selected(&[
        "--edit-loop",
        "tests/misc/rfc0165_owner_adapters.rs",
    ]);
    assert_eq!(commands.len(), 1, "owner adapter edit route expanded: {commands:?}");
    assert!(
        commands[0].contains(
            "rfc0165_owner_adapters::server_workers_reject_raw_wasm_even_with_genuine_entry_metadata",
        ),
        "raw-Wasm admission probe was omitted: {commands:?}",
    );
}

#[test]
fn integration_driver_edits_use_the_bounded_semantic_contracts() {
    for path in [
        "crates/witchy-types/src/pipeline/test_driver/integration.rs",
        "src/commands/test_runner/canonical_integration.rs",
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 1, "{path}: {commands:?}");
        assert!(commands[0].contains("--bin witchy --test misc"), "{path}: {commands:?}");
        assert!(
            commands[0].contains(
                "source_integration_recipe_runs_selected_console_capture_and_reports_abort",
            ),
            "{path}: {commands:?}",
        );
        assert!(
            commands[0].contains("commands::test_runner::canonical_integration_tests::"),
            "{path}: {commands:?}",
        );
        assert!(
            commands[0].contains(
                "wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature",
            ),
            "{path}: {commands:?}",
        );
    }
}

#[test]
fn build_recipe_test_edits_run_their_executable_module() {
    for (path, module) in [
        ("src/source/build_recipe_tests.rs", "build_recipe_tests"),
        (
            "src/source/build_net_recipe_tests.rs",
            "build_recipe_tests::build_net_recipe_tests",
        ),
        (
            "src/source/build_env_recipe_tests.rs",
            "build_recipe_tests::build_env_recipe_tests",
        ),
        (
            "src/source/build_exec_recipe_tests.rs",
            "build_recipe_tests::build_exec_recipe_tests",
        ),
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 1, "{path}: {commands:?}");
        assert_eq!(
            commands[0],
            format!(
                "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^source::{module}::/)'"
            ),
        );
    }
}

#[test]
fn build_read_integration_edits_run_only_their_own_modules() {
    for (path, module) in [
        (
            "tests/misc/rfc0103_build_step_confinement.rs",
            "rfc0103_build_step_confinement",
        ),
        (
            "tests/misc/rfc0165_build_read_project.rs",
            "rfc0165_build_read_project",
        ),
        (
            "tests/misc/rfc0165_build_env_project.rs",
            "rfc0165_build_env_project",
        ),
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 1, "{path}: {commands:?}");
        assert!(
            commands[0].starts_with("./scripts/nextest-admitted.sh witchy -- ")
                && commands[0].contains("cargo nextest run --test misc")
                && commands[0].contains(&format!("test(/^{}::/)", module)),
            "{path}: {commands:?}",
        );
        assert_ne!(
            commands[0],
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc",
            "{path} expanded to every misc test: {commands:?}",
        );
    }
}

#[test]
fn url_component_edits_run_only_the_url_component_module() {
    let commands = selected(&["--edit-loop", "tests/misc/url_components.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].contains("cargo nextest run --test misc")
            && commands[0].contains("test(/^url_components::/)")
            && commands[0].starts_with("./scripts/nextest-admitted.sh witchy -- "),
        "{commands:?}"
    );
    assert_ne!(
        commands[0],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc",
        "URL component edit expanded to every misc test: {commands:?}"
    );
}

#[test]
fn cli_subcommand_edits_replay_output_authority_and_admission_guards() {
    let commands = selected(&["--edit-loop", "tests/cli_subcommands.rs"]);
    assert_eq!(commands.len(), 1, "CLI edit route expanded: {commands:?}");
    for probe in [
        "source_nil_main_int_output_is_not_eaten_as_exit_code",
        "sandbox_wasm_requires_explicit_dir_grant",
        "fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact",
    ] {
        assert!(
            commands[0].contains(probe),
            "missing CLI contract probe {probe}: {commands:?}"
        );
    }
    assert!(commands[0].contains("--test cli_subcommands -E 'test(="), "broad CLI route: {commands:?}");
}

#[test]
fn lower_codegen_edits_run_the_known_fast_failure_probe_first() {
    let commands = selected(&["--edit-loop", "crates/witchy-lower/tests/codegen.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(
        commands[0],
        "cargo nextest run -p witchy-lower --test codegen -E 'test(=tests::grantful_build_primitives_compile_to_build_imports_only)'"
    );
}

#[test]
fn semantic_conformance_edits_run_the_known_independent_expectation_probe_first() {
    let commands = selected(&["--edit-loop", "tests/misc/semantic_conformance.rs"]);
    assert_eq!(
        commands,
        ["cargo nextest run --test misc -E 'test(=semantic_conformance::independent_expectation_rejects_a_shared_semantic_mutation)'"],
        "{commands:?}"
    );
}

#[test]
fn owned_slot_child_storage_edits_run_the_known_admission_probe_first() {
    let commands = selected(&[
        "--edit-loop",
        "crates/witchy-wir/tests/owned_slot_child_storage.rs",
    ]);
    assert_eq!(
        commands,
        ["cargo nextest run -p witchy-wir --features native --test owned_slot_child_storage -E 'test(=owned_slots_require_managed_child_storage_before_consuming_move)'"],
        "{commands:?}"
    );
}

#[test]
fn dispatch_edits_probe_fetch_origin_admission_before_commands_module() {
    let commands = selected(&["--edit-loop", "src/commands/dispatch.rs"]);
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert_eq!(
        commands[0],
        "cargo nextest run --test cli_subcommands -E 'test(=fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact)'"
    );
    assert!(
        commands[1].contains("test(/^commands::/)")
            && commands[1].starts_with("./scripts/nextest-admitted.sh witchy -- "),
        "{commands:?}"
    );
}

#[test]
fn network_misc_edits_run_only_their_modules() {
    for (path, module) in [
        ("tests/misc/canonical_net_fixture.rs", "canonical_net_fixture"),
        ("tests/misc/http_url_fallible.rs", "http_url_fallible"),
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 1, "{path}: {commands:?}");
        assert!(
            commands[0].contains(&format!("test(/^{}::/)", module)),
            "{path}: {commands:?}"
        );
        assert_ne!(
            commands[0],
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc",
            "{path} expanded to every misc test: {commands:?}"
        );
    }
}

#[test]
fn uniform_var_edits_run_the_complete_module_only() {
    let commands = selected(&["--edit-loop", "tests/misc/rfc0087_uniform_var.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].contains("test(/^rfc0087_uniform_var::/)")
            && commands[0].contains("cargo nextest run --test misc"),
        "{commands:?}"
    );
    assert_ne!(
        commands[0],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc",
        "uniform-var edit expanded to every misc test: {commands:?}"
    );
}

#[test]
fn migration_census_edits_run_only_the_census_module() {
    let commands = selected(&["--edit-loop", "tests/misc/rfc0087_migration_census.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].contains("test(/^rfc0087_migration_census::/)")
            && commands[0].contains("cargo nextest run --test misc"),
        "{commands:?}"
    );
    assert_ne!(
        commands[0],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc",
        "migration census edit expanded to every misc test: {commands:?}"
    );
}

#[test]
fn broad_misc_run_subsumes_the_same_filtered_binary_run() {
    let commands = selected(&[
        "--focused",
        "src/bin/rfc0087-census.rs",
        "tests/misc/rfc0087_migration_census.rs",
    ]);
    assert_eq!(
        commands,
        ["./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc"],
        "the complete misc run should replace its redundant census-only subset"
    );
}

#[test]
fn exact_filters_for_the_same_test_target_share_one_build() {
    let commands = selected(&[
        "--edit-loop",
        "src/commands/project_resolver_tests.rs",
        "tests/cli_subcommands.rs",
    ]);
    let cli_probes: Vec<_> = commands
        .iter()
        .filter(|command| command.starts_with("cargo nextest run --test cli_subcommands -E"))
        .collect();
    assert_eq!(cli_probes.len(), 1, "duplicate CLI target builds: {commands:?}");
    for probe in [
        "compile_accepts_complete_authenticated_package_owners",
        "source_nil_main_int_output_is_not_eaten_as_exit_code",
        "sandbox_wasm_requires_explicit_dir_grant",
        "fetch_root_requires_an_explicit_valid_origin_and_admitted_artifact",
    ] {
        assert!(cli_probes[0].contains(probe), "missing {probe}: {commands:?}");
    }
}

#[test]
fn compiler_feature_variants_share_one_example_matrix() {
    let commands = selected(&[
        "--edit-loop",
        "crates/witchy-lower/Cargo.toml",
        "crates/witchy-runtime/Cargo.toml",
        "crates/witchy-wir/Cargo.toml",
    ]);
    let example_runs: Vec<_> = commands
        .iter()
        .filter(|command| command.contains("test(/^example_tests::/)"))
        .collect();
    assert_eq!(example_runs.len(), 1, "duplicate example matrices: {commands:?}");
    assert!(
        !example_runs[0].contains("--features witchy-wir/native"),
        "the root test graph already enables witchy-wir/native: {commands:?}"
    );
}

#[test]
fn async_region_edits_run_complete_rfc0152_modules() {
    for (path, module) in [
        (
            "tests/misc/rfc0152_async_region_formation.rs",
            "rfc0152_async_region_formation",
        ),
        ("tests/misc/rfc0152_semantic_matrix.rs", "rfc0152_semantic_matrix"),
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 1, "{path}: {commands:?}");
        assert!(
            commands[0].contains(&format!("test(/^{}::/)", module))
                && commands[0].contains("cargo nextest run --test misc")
                && commands[0].contains("nextest-admitted.sh")
                && !commands[0].contains("--ignore-default-filter"),
            "{path}: {commands:?}"
        );
        assert_ne!(
            commands[0],
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc",
            "{path} expanded to every misc test: {commands:?}"
        );
    }
}

#[test]
fn mode_interop_edits_run_only_the_known_reference_free_probe() {
    let commands = selected(&["--edit-loop", "tests/misc/rfc0167_mode_interop.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(
        commands[0],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc -E 'test(=rfc0167_mode_interop::reference_free_normal_opt_chain_preserves_modes_and_aliased_snapshot_without_opts)'"
    );
    assert_ne!(
        commands[0],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc",
        "mode interop edit expanded to every misc test: {commands:?}"
    );
}

#[test]
fn direct_fetch_fixture_edits_run_the_complete_fetch_module() {
    let commands = selected(&["--edit-loop", "tests/misc/rfc0165_fetch.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].contains("test(/^rfc0165_fetch::/)")
            && commands[0].contains("cargo nextest run --test misc"),
        "{commands:?}"
    );
    assert!(
        !commands[0].contains("rfc0165_route_gate")
            && !commands[0].contains("test(=rfc0165_oracle::oracle_agrees_on_the_admitted_examples_shard_3)"),
        "direct Fetch fixture edit retained the canonical checkpoint: {commands:?}"
    );
}

#[test]
fn direct_rfc0165_fixture_edits_run_complete_modules() {
    for (path, module) in [
        ("tests/misc/rfc0165_messages.rs", "rfc0165_messages"),
        ("tests/misc/rfc0165_corpus.rs", "rfc0165_corpus"),
        ("tests/misc/rfc0165_project_run.rs", "rfc0165_project_run"),
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 1, "{path}: {commands:?}");
        assert!(
            commands[0].contains(&format!("test(/^{}::/)", module))
                && commands[0].contains("cargo nextest run --test misc"),
            "{path}: {commands:?}"
        );
        assert!(
            !commands[0].contains("rfc0165_route_gate")
                && !commands[0].contains("oracle_agrees_on_the_admitted_examples_shard_3"),
            "{path} retained the canonical checkpoint: {commands:?}"
        );
    }
}

#[test]
fn build_read_host_edits_keep_authority_bounds_and_abi_probes() {
    for path in [
        "crates/witchy-runtime/src/runtime/host/build_read.rs",
        "crates/witchy-runtime/src/runtime/host/build_read_tests.rs",
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 2, "{path}: {commands:?}");
        assert!(
            commands[0].contains("runtime::host::build_read_tests::")
                && commands[0].contains(
                    "build_read_captures_authenticate_kind_session_projection_and_cleanup",
                ),
            "{path}: {commands:?}",
        );
        assert!(
            commands[1].contains(
                "wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature",
            ),
            "{path}: {commands:?}",
        );
        assert!(
            commands.iter().all(|command| {
                !command.contains("example_tests") && !command.contains("check.sh --wasm")
            }),
            "{path}: {commands:?}",
        );
    }
}

#[test]
fn build_env_host_edits_keep_authority_bounds_and_abi_probes() {
    for path in [
        "crates/witchy-runtime/src/runtime/host/build_env.rs",
        "crates/witchy-runtime/src/runtime/host/build_env_tests.rs",
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 2, "{path}: {commands:?}");
        assert!(
            commands[0].contains("runtime::host::build_env_tests::")
                && commands[0].contains(
                    "build_env_captures_reauthenticate_root_and_optional_response_and_cleanup",
                ),
            "{path}: {commands:?}",
        );
        assert!(
            commands[1].contains(
                "wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature",
            ),
            "{path}: {commands:?}",
        );
    }
}

#[test]
fn canonical_build_recipe_edits_keep_cross_stage_contracts() {
    for path in [
        "crates/witchy-lower/src/canonical_build_read.rs",
        "crates/witchy-types/src/pipeline/build_recipe.rs",
        "crates/witchy-wir/src/canonical_interpreter/build_read.rs",
        "crates/witchy-wir/src/canonical_safewir/build_read.rs",
    ] {
        let commands = selected(&["--edit-loop", path]);
        assert_eq!(commands.len(), 4, "{path}: {commands:?}");
        for contract in [
            "build_read_catalog_separates_root_authority_from_bounded_responses",
            "build_env_catalog_authenticates_root_and_keeps_presence_internal",
            "build_env_oracle_keeps_denied_unset_empty_and_private_sessions_distinct",
        ] {
            assert!(commands[0].contains(contract), "{path}: {contract}: {commands:?}");
        }
        assert!(
            commands[1].contains("--test codegen")
                && commands[1]
                    .contains("grantful_build_primitives_compile_to_build_imports_only"),
            "{path}: {commands:?}",
        );
        for module in [
            "source::build_recipe_tests::",
            "source::build_env_recipe_tests::",
            "rfc0103_build_step_confinement::",
            "rfc0165_build_read_project::",
            "rfc0165_build_env_project::",
        ] {
            assert!(commands[2].contains(module), "{path}: {module}: {commands:?}");
        }
        for contract in [
            "runtime::host::build_read_tests::",
            "runtime::host::build_env_tests::",
            "build_read_captures_authenticate_kind_session_projection_and_cleanup",
            "build_env_captures_reauthenticate_root_and_optional_response_and_cleanup",
        ] {
            assert!(commands[3].contains(contract), "{path}: {contract}: {commands:?}");
        }
    }
}

#[test]
fn integration_grants_edit_runs_only_its_misc_module() {
    let commands = selected(&[
        "--edit-loop",
        "tests/misc/rfc0077_integration_grants.rs",
    ]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(
        commands[0].starts_with("./scripts/nextest-admitted.sh witchy -- ")
            && commands[0].contains("test(/^rfc0077_integration_grants::/)")
            && commands[0].contains(
                "not test(/^rfc0077_integration_grants::source_integration_async_recipe_/)",
            ),
        "{commands:?}",
    );
    assert_ne!(
        commands[0],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test misc",
        "the edit loop expanded to every misc test: {commands:?}",
    );
}

#[test]
fn canonical_type_edit_uses_the_bounded_semantic_sentinels() {
    let commands = selected(&[
        "--edit-loop",
        "crates/witchy-types/src/canonical_aggregates.rs",
    ]);
    assert_eq!(commands.len(), 3, "canonical type edit expanded past its sentinels: {commands:?}");
    assert!(commands[0].contains("constant_prefix_publication_rejects_gaps_and_missing_children"), "{commands:?}");
    assert!(commands[0].contains("scalar_move_range_is_bounds_checked_and_admitted"), "{commands:?}");
    assert!(commands[1].contains("canonical_pipeline::list_"), "{commands:?}");
    assert!(commands[2].contains("admitted_aggregates_render_structurally_as_the_interpreter_prints_them"), "{commands:?}");
}

#[test]
fn native_cli_entrypoints_use_the_bounded_compiler_shard() {
    let expected = [
        "cargo check -p witchy --all-targets",
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^(checked_cli_pipeline_tests|cli::|runtime_parity_tests|source::tests|test_mode_link_tests)::/)'",
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test cli_subcommands",
        "cargo nextest run -p witchy-syntax",
    ];

    for path in ["src/main.rs", "src/cli.rs"] {
        assert_eq!(selected(&[path]), expected, "focused route for {path}");
    }
    let source = selected(&["src/source.rs"]);
    assert_eq!(
        source.first().map(String::as_str),
        Some("cargo nextest run --test misc -E 'test(=rfc0165_packaged_source::packaged_source_rejects_custom_owner_and_unsafe_input_paths)'")
    );
    assert_eq!(
        source.get(1).map(String::as_str),
        Some("cargo nextest run --test cli_subcommands -E 'test(=compile_accepts_complete_authenticated_package_owners)'")
    );
    assert_eq!(&source[2..], expected, "focused route for src/source.rs");
}

#[test]
fn native_cli_entrypoint_shard_does_not_expand_to_the_workspace_suite() {
    let commands = selected(&["src/main.rs", "src/cli.rs", "src/source.rs"]);
    assert!(!commands.iter().any(|command| command.contains("--workspace")), "{commands:?}");
    assert!(!commands.iter().any(|command| command == "./scripts/check.sh --fast"), "{commands:?}");
}

#[test]
fn broad_compiler_change_subsumes_the_native_cli_entrypoint_shard() {
    assert_eq!(
        selected(&["src/main.rs", "crates/witchy-types/src/typeck.rs"]),
        ["./scripts/check.sh --fast"]
    );
}

#[test]
fn nested_integration_module_uses_its_root_cargo_target() {
    assert_eq!(
        selected(&["--edit-loop", "tests/rfc0122.rs"]),
        ["./scripts/nextest-admitted.sh witchy -- cargo nextest run --test rfc0122"],
    );
}

#[test]
fn router_changes_run_router_regressions() {
    let output = route(&["scripts/test-for-paths.sh"]);
    assert!(output.contains("for f in scripts/*.sh scripts/bin/*; do bash -n"));
    assert!(output.contains("./scripts/check.sh --queue-infra"));
    assert!(
        !output.contains("cargo nextest run -p merge-queue-tests --test test_for_paths"),
        "the full queue shard already runs the complete router test binary: {output}",
    );

    let edit_commands = selected(&["--edit-loop", "scripts/test-for-paths.sh"]);
    assert!(
        edit_commands
            .iter()
            .any(|command| command == "cargo nextest run -p merge-queue-tests --test test_for_paths"),
        "the representative queue edit shard does not subsume router tests: {edit_commands:?}",
    );
    assert!(
        edit_commands
            .iter()
            .any(|command| command == "./scripts/check.sh --queue-infra-edit"),
        "missing queue edit shard: {edit_commands:?}",
    );
}

#[test]
fn agent_check_changes_verify_utility_qos_without_a_rust_build() {
    for path in ["scripts/agent-check.sh", "scripts/test-agent-check-qos.sh"] {
        assert_eq!(
            selected(&[path]),
            [
                "for f in scripts/*.sh scripts/bin/*; do bash -n \"$f\"; done",
                "./scripts/test-agent-check-qos.sh",
            ],
            "{path}"
        );
    }
}

#[test]
fn queue_substrate_changes_use_the_hermetic_queue_shard() {
    assert_eq!(
        selected(&["scripts/merge-queue.sh"]),
        [
            "for f in scripts/*.sh scripts/bin/*; do bash -n \"$f\"; done",
            "./scripts/check.sh --queue-infra",
        ]
    );
}

#[test]
fn standalone_queue_fixture_uses_only_the_hermetic_shard() {
    assert_eq!(
        selected(&["tools/merge-queue-tests/tests/merge_queue.rs"]),
        ["./scripts/check.sh --queue-infra"]
    );
    assert_eq!(
        selected(&["tools/merge-queue-tests/tests/merge_queue/gate_behavior/prewarm.rs"]),
        ["./scripts/check.sh --queue-infra"]
    );
    assert_eq!(
        selected(&["--edit-loop", "tools/merge-queue-tests/tests/merge_queue.rs"]),
        ["./scripts/check.sh --queue-infra-edit"]
    );
}

#[test]
fn workspace_manifest_runs_the_stage_crate_dag_probe_early() {
    assert_eq!(
        selected(&["Cargo.toml"]),
        ["python3 scripts/check-stage-dependencies.py"]
    );
}

#[test]
fn nextest_config_change_runs_one_parse_smoke() {
    assert_eq!(
        selected(&["--edit-loop", ".config/nextest.toml"]),
        ["cargo nextest run -p merge-queue-tests --test test_for_paths --ignore-default-filter -E 'test(=nextest_config_change_runs_one_parse_smoke)'"],
    );
}

#[test]
fn gate_report_change_runs_its_end_to_end_probe() {
    assert_eq!(
        selected(&["--edit-loop", "scripts/gate-report.sh"]),
        [
            "bash -n scripts/gate-report.sh",
            "cargo nextest run -p merge-queue-tests --test merge_queue --ignore-default-filter -E 'test(=gate_behavior::state_and_gate::gate_report_is_read_only_and_aggregates_batches_failures_and_phases)'",
        ],
    );
}

#[test]
fn stage_dependency_contract_and_probe_route_to_lightweight_checks() {
    for path in [
        "scripts/check-stage-dependencies.py",
        "spec/stage-dependencies.json",
    ] {
        assert_eq!(
            selected(&["--edit-loop", path]),
            [
                "python3 scripts/check-stage-dependencies.py",
                "cargo nextest run -p repo-tests --test repo_hygiene -E 'test(architecture::stage_crates_follow_the_declared_dependency_dag)'",
            ],
            "{path}",
        );
    }
}

#[test]
fn source_backed_test_driver_runs_the_native_host_catalog_probe_early() {
    let probe = "cargo nextest run --test misc -E 'test(=wasm_abi_catalog::native_host_links_every_catalog_import_with_the_declared_signature)'";
    for path in [
        "crates/witchy-types/src/pipeline/test_driver.rs",
        "crates/witchy-types/src/pipeline/test_driver/console_fixture.rs",
        "src/commands/test_runner/canonical_bare.rs",
        "src/commands/test_runner/canonical_console_fixture.rs",
    ] {
        let commands = selected(&[path]);
        assert_eq!(
            commands.first().map(String::as_str),
            Some(probe),
            "focused route for {path}: {commands:?}"
        );
    }
}

#[test]
fn worktree_module_and_script_route_to_the_lightweight_integration_binary() {
    assert_eq!(
        selected(&["tools/merge-queue-tests/tests/worktree/status.rs"]),
        ["cargo nextest run -p merge-queue-tests --test worktree"]
    );
    let commands = selected(&["scripts/worktree-status.sh"]);
    assert!(commands.contains(&"./scripts/check.sh --queue-infra".to_owned()));
    assert_eq!(gate_nextest(&["scripts/worktree-status.sh"]), "--test worktree\n");
    assert!(!commands.iter().any(|command| command.contains("worktree_status")));
}

#[test]
fn spec_freshness_script_runs_its_behavior_check() {
    let output = route(&["scripts/check-spec-freshness.sh"]);
    assert!(output.contains("for f in scripts/*.sh scripts/bin/*; do bash -n"));
    assert!(output.contains("./scripts/check-spec-freshness.sh"));
    assert!(output.contains("cargo nextest run -p repo-tests --test repo_hygiene"));
}

#[test]
fn repository_contracts_route_without_the_compiler_package() {
    for path in [
        "tools/repo-tests/tests/repo_hygiene/architecture.rs",
        "tools/repo-tests/tests/repo_hygiene/release_packaging.rs",
        "tools/repo-tests/tests/repo_hygiene/rfc_status_hygiene.rs",
    ] {
        assert_eq!(
            selected(&[path]),
            ["cargo nextest run -p repo-tests --test repo_hygiene"],
            "focused route for {path}",
        );
        let gate = gate_nextest(&[path]);
        assert!(gate.contains("-p repo-tests"), "gate route for {path}: {gate}");
        assert!(!gate.contains("-p witchy"), "compiler package leaked into {path}: {gate}");
    }
}

#[test]
fn rfc_edits_run_the_lightweight_cross_link_contract() {
    let commands = selected(&["rfcs/0170-compiler-boundaries.md"]);
    assert_eq!(
        commands,
        ["cargo nextest run -p repo-tests --test repo_hygiene -E 'test(rfc_status_hygiene::)'"]
    );
    let gate = gate_nextest(&["rfcs/0170-compiler-boundaries.md"]);
    assert!(gate.contains("-p repo-tests"), "{gate}");
    assert!(!gate.contains("-p witchy"), "{gate}");
}

#[test]
fn gate_cost_report_runs_its_log_segmentation_regression() {
    for path in ["scripts/gate-test-cost.py", "scripts/test-gate-test-cost.py"] {
        assert_eq!(selected(&[path]), ["python3 scripts/test-gate-test-cost.py"]);
    }
}

#[test]
fn staged_change_is_discovered_without_explicit_paths() {
    let repo = TempRepo::new();
    let root = repo.path();
    git(root, &["init", "-q"]);
    git(root, &["symbolic-ref", "HEAD", "refs/heads/master"]);
    git(root, &["config", "user.name", "Witchy Test"]);
    git(root, &["config", "user.email", "witchy-test@example.invalid"]);
    fs::write(root.join("README.md"), "base\n").expect("write fixture base");
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base"]);
    fs::write(root.join("README.md"), "staged\n").expect("write staged fixture");
    git(root, &["add", "README.md"]);

    let output = Command::new("bash")
        .current_dir(root)
        .arg("scripts/test-for-paths.sh")
        .output()
        .expect("run staged path router");
    assert!(
        output.status.success(),
        "router: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("router output is UTF-8");
    // The README route no longer selects the nightly-deferred documentation
    // sweep (it executes the frozen source interpreter), so a staged README
    // is routed as a prose-only change. What this test proves is discovery:
    // the staged path was seen and classified, not reported as "no changed
    // files".
    assert!(
        stdout.contains("prose-only change")
            || stdout.contains("documentation_examples_are_valid")
            || stdout.contains("example_tests"),
        "staged README route: {stdout}"
    );
    assert!(!stdout.contains("no changed files"), "staged README was missed: {stdout}");
}

#[test]
fn rust_path_drops_nextest_checks_covered_by_fast() {
    assert_eq!(
        selected(&["crates/witchy-interp/src/comptime.rs"]),
        ["./scripts/check.sh --fast"]
    );
}

#[test]
fn lower_path_drops_covered_nextest_checks_and_the_browser_build() {
    // The `--wasm` shard cross-compiles the compiler for the browser
    // playground and validates the runnable book: docs-site work. It used to
    // ride along on every witchy-lower/-wir/-runtime edit at a 908s presubmit
    // median. Decided 2026-09-21: only the compiler is gated.
    assert_eq!(selected(&["crates/witchy-lower/src/lib.rs"]), ["./scripts/check.sh --fast"]);
    for path in [
        "crates/witchy-wir/src/lib.rs",
        "crates/witchy-runtime/src/lib.rs",
        "book/src/intro.md",
        "examples/hello/src/main.witchy",
    ] {
        let commands = selected(&[path]);
        assert!(
            !commands.iter().any(|command| command.contains("check.sh --wasm")),
            "{path} still adds the browser build to the dev loop: {commands:?}",
        );
    }
}

#[test]
fn std_path_without_rust_keeps_its_focused_checks() {
    let commands = selected(&["std/list.witchy"]);
    assert_eq!(commands.len(), 3, "{commands:?}");
    assert!(commands[0].contains("example_tests"), "{commands:?}");
    assert!(commands[1].contains("stdlib_docs_are_current"), "{commands:?}");
    assert!(commands[2].contains("fmt --check"), "{commands:?}");
    assert!(commands[2].contains("${CARGO_TARGET_DIR:-target}/debug/witchy"), "{commands:?}");
}

#[test]
fn mixed_rust_and_std_keeps_only_fast_and_std_formatting() {
    let commands = selected(&["crates/witchy-syntax/src/parser.rs", "std/meta.witchy"]);
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert_eq!(commands[0], "./scripts/check.sh --fast");
    assert!(commands[1].contains("fmt --check"), "{commands:?}");
    assert!(commands[1].contains("${CARGO_TARGET_DIR:-target}/debug/witchy"), "{commands:?}");
}

#[test]
fn project_formatting_uses_the_isolated_cargo_target() {
    let commands = selected(&["projects/glamour/examples/accessibility/src/main.witchy"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(commands[0].contains("${CARGO_TARGET_DIR:-target}/debug/witchy"), "{commands:?}");
    assert!(commands[0].contains("fmt --check"), "{commands:?}");
}

#[test]
fn focused_mode_retains_crate_nextest_checks() {
    let commands = selected_focused(&["crates/witchy-syntax/src/parser.rs"]);
    assert_eq!(
        commands,
        [
            "cargo nextest run -p witchy-syntax",
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run -E 'test(/^example_tests::/)'",
        ]
    );
}

/// Master broke on 2026-08-27 because an async-lowering change in
/// `witchy-types` gated GREEN over 1,068 tests in 15.4s without ever running
/// `rfc0130_generator`, whose owned-frame census the change invalidated. A
/// single-crate selection cannot see a cross-pipeline conformance suite, so a
/// compiler-stage change must always pull them in. They are cheap -- most
/// average about a second.
#[test]
fn gate_nextest_compiler_stage_changes_select_rfc_conformance_binaries() {
    for path in [
        "crates/witchy-types/src/typed_async_lowering.rs",
        "crates/witchy-lower/src/codegen/mod.rs",
        "crates/witchy-wir/src/lib.rs",
        "crates/witchy-runtime/src/runtime.rs",
        "crates/witchy-syntax/src/linker.rs",
        "crates/witchy-interp/src/typed_async.rs",
    ] {
        let out = gate_nextest(&[path]);
        assert!(
            out.contains("binary_id(/^witchy::rfc0/)"),
            "{path} did not select the RFC conformance binaries:\n{out}"
        );
        assert!(
            out.contains("-p witchy"),
            "{path} selected conformance binaries without the root package:\n{out}"
        );
    }
}

/// Deleting an integration test must not select the binary it removed.
/// Naming `--test <gone>` fails the whole plan; the root-integration budget
/// census is what a deletion can actually invalidate.
#[test]
fn a_deleted_integration_test_selects_the_budget_census_not_its_own_binary() {
    let commands = selected(&["--edit-loop", "tests/definitely_not_a_real_test_file.rs"]);
    assert!(
        commands.iter().all(|command| !command.contains("--test definitely_not_a_real_test_file")),
        "selected the binary of a file that is not there: {commands:?}"
    );
    assert!(
        commands.iter().any(|command| {
            command.contains("root_integration_test_binaries_stay_within_budget")
        }),
        "a deletion must still re-check the root-integration budget: {commands:?}"
    );
}

/// `semantic_conformance` carries independent expected results, which is the
/// adjudication a backend-agreement test cannot give. It is a module of
/// `witchy::misc`, so a single-crate selection never reaches it: before this,
/// a canonical-checker change gated green without ever running the suite that
/// says what the programs it admits should do.
#[test]
fn gate_nextest_compiler_stage_changes_select_semantic_conformance() {
    for path in [
        "crates/witchy-wir/src/canonical_safewir/check/memory.rs",
        "crates/witchy-types/src/typeck.rs",
        "crates/witchy-lower/src/codegen/mod.rs",
    ] {
        let out = gate_nextest(&[path]);
        assert!(
            out.contains("test(/^semantic_conformance::/)"),
            "{path} did not select the independent expected results:\n{out}"
        );
    }
}

/// The converse: a change that cannot affect compiler semantics must NOT drag
/// the conformance suites in, or the selection stops being a selection.
#[test]
fn gate_nextest_queue_scripts_do_not_select_rfc_conformance_binaries() {
    let out = gate_nextest(&["scripts/merge-queue.sh"]);
    assert!(
        !out.contains("binary_id(/^witchy::rfc0/)"),
        "a queue-script change pulled in the conformance suites:\n{out}"
    );
    assert!(
        !out.contains("test(/^semantic_conformance::/)"),
        "a queue-script change pulled in the independent expected results:\n{out}"
    );
}

#[test]
fn canonical_native_edit_reuses_one_build_without_changing_scope_or_receipts() {
    let output = Command::new("bash")
        .arg(workspace_root().join("scripts/test-canonical-native-edit.sh"))
        .output()
        .expect("run mocked canonical-native edit workflow");
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn canonical_native_reuse_helper_changes_run_the_mocked_workflow() {
    for path in ["scripts/canonical-native-edit.sh", "scripts/test-canonical-native-edit.sh"] {
        let commands = selected(&["--edit-loop", path]);
        assert!(commands.iter().any(|command| command == "./scripts/test-canonical-native-edit.sh"), "{commands:?}");
        assert!(!commands.iter().any(|command| command.contains("cargo ")), "mocked tooling check started Cargo: {commands:?}");
        let gate = gate_nextest(&[path]);
        assert!(gate.contains("test_for_paths"), "missing landing regression: {gate}");
    }
}

/// The edit loop is deliberately bounded (see the sentinel assertions above),
/// so it is a weaker signal than the gate. Measured 2026-09-20 over the last
/// 150 queue attempts: a branch carrying a green `edit` receipt went red
/// 44.8% of the time against 15.0% for branches with no receipt at all, and
/// 13 of the 21 identified failures were in `witchy::misc`, which the bounded
/// plan does not run whole. agent-check says so rather than letting a green
/// edit loop read as gate-ready.
#[test]
fn a_bounded_edit_loop_names_the_binary_it_did_not_run() {
    let script = workspace_root().join("scripts/agent-check.sh");
    let body = fs::read_to_string(&script).expect("read agent-check.sh");
    let function = body
        .split_once("report_bounded_edit_gap() {")
        .expect("agent-check defines report_bounded_edit_gap")
        .1;
    let function = format!(
        "report_bounded_edit_gap() {{{}",
        &function[..function.find("\n}\n").expect("function ends") + 3]
    );

    let fired = |plan: &str, changed: &str| -> bool {
        let script = format!(
            "set -u\n{function}\n./scripts/test-for-paths.sh() {{ printf '%s' \"{plan}\"; }}\n             git() {{ printf '%s' \"{changed}\"; }}\nreport_bounded_edit_gap\n"
        );
        let output = Command::new("bash")
            .current_dir(workspace_root())
            .args(["-c", &script])
            .output()
            .expect("run the gap probe");
        String::from_utf8_lossy(&output.stdout).contains("bounded edit loop")
    };

    let semantics = "crates/witchy-types/src/typeck.rs\n";
    assert!(fired("  cargo nextest run -p witchy-types\n", semantics), "bounded semantics plan must name the gap");
    assert!(!fired("  ./scripts/check.sh --fast\n", semantics), "the fast gate already runs it");
    assert!(
        !fired(
            "  ./scripts/nextest-admitted.sh witchy -- cargo nextest run -p witchy --features witchy-wir/native --test misc\n",
            semantics,
        ),
        "a whole-binary run already covers it",
    );
    assert!(!fired("  cargo nextest run -p witchy-types\n", "std/list.witchy\n"), "non-semantics paths must stay quiet");
}

#[test]
fn extensionless_script_shims_are_syntax_checked() {
    // `scripts/bin/cargo` and `scripts/bin/witchy` have no suffix, so the
    // `scripts/*.sh` rule never matched them: adding one used to classify as a
    // prose-only change and nothing ran `bash -n` over it.
    for path in ["scripts/bin/cargo", "scripts/bin/witchy"] {
        let commands = selected(&[path]);
        assert!(
            commands.iter().any(|command| command.contains("bash -n")),
            "{path} selected no syntax check: {commands:?}",
        );
        assert!(
            commands
                .iter()
                .any(|command| command.contains("scripts/bin/*")),
            "{path}'s syntax check does not cover the shims themselves: {commands:?}",
        );
    }
    // The existing rule still covers ordinary scripts, and now covers the
    // shims too.
    let commands = selected(&["scripts/worktree-warm.sh"]);
    assert!(
        commands
            .iter()
            .any(|command| command.contains("scripts/*.sh scripts/bin/*")),
        "ordinary scripts lost or narrowed their syntax check: {commands:?}",
    );
}

#[test]
fn mbx_policy_runs_only_the_worktree_cache_contracts() {
    let commands = selected(&[".mbx.toml"]);
    assert_eq!(
        commands,
        ["cargo nextest run -p merge-queue-tests --test worktree"],
        "MBX policy selected unrelated product work: {commands:?}",
    );
    let gate = gate_nextest(&[".mbx.toml"]);
    assert!(gate.contains("--test worktree"), "MBX gate missed worktree contracts: {gate}");
    assert!(!gate.contains("WORKSPACE"), "MBX policy fell through to workspace: {gate}");
}

#[test]
fn script_rules_gate_on_the_binary_their_focused_check_names() {
    // These rules already knew which binary covers them: each ran
    // `cargo nextest run -p merge-queue-tests --test worktree` (or the
    // queue-infra shard) as a
    // focused check. But none of them called `add_gate_test`, so the GATE
    // selection stayed empty and fell through to WORKSPACE -- a full
    // 4,000-test workspace build and run to validate a shell edit. Replaying
    // the mapper over 200 merged commits, `scripts/` paths were the single
    // biggest driver of WORKSPACE fallback.
    //
    // The covering binary is a checkable fact, not a guess:
    // tools/merge-queue-tests/tests/worktree/{status,warm,create,rfc_status}.rs reference the four
    // worktree scripts, and tools/merge-queue-tests/tests/merge_queue/
    // coordinator.rs references agent-check.sh and submit-with-presubmit.sh.
    for (path, binary) in [
        ("scripts/worktree-status.sh", "worktree"),
        ("scripts/worktree-warm.sh", "worktree"),
        ("scripts/worktree-create.sh", "worktree"),
        ("scripts/rfc-status.sh", "worktree"),
        ("scripts/agent-check.sh", "merge_queue"),
        ("scripts/test-agent-check-qos.sh", "merge_queue"),
        ("scripts/submit-with-presubmit.sh", "merge_queue"),
        ("scripts/test-submit-with-presubmit.sh", "merge_queue"),
    ] {
        let output = gate_nextest(&[path]);
        assert!(
            !output.contains("WORKSPACE"),
            "{path} still falls through to the whole workspace: {output}",
        );
        assert!(
            output.contains(&format!("--test {binary}")),
            "{path} did not select {binary}, the binary that covers it: {output}",
        );
    }
}


#[test]
fn gate_names_every_target_of_every_selected_package() {
    // `--test` is a GLOBAL target filter, so naming only the root binaries an
    // expression needs silently drops each crate package's own tests (803 ->
    // 160, reported green). Naming EVERY target of every selected package is
    // sound because filters union. This pins the complete set for a typeck
    // change against the filesystem, so a new integration test file cannot be
    // left out of the gate by a stale list.
    let output = gate_nextest(&["crates/witchy-types/src/typeck.rs"]);
    let selection = output.lines().next().unwrap_or_default();
    let words: Vec<&str> = selection.split_whitespace().collect();
    let named = |target: &str| words.windows(2).any(|pair| pair == ["--test", target]);
    assert!(words.contains(&"--lib"), "unit tests dropped: {output}");
    let root = workspace_root();
    for (dir, keep) in [("crates/witchy-types/tests", "" ), ("tests", "rfc0")] {
        for entry in std::fs::read_dir(root.join(dir)).expect("list tests") {
            let path = entry.expect("dir entry").path();
            if path.extension().is_none_or(|extension| extension != "rs") { continue; }
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            if !name.starts_with(keep) { continue; }
            assert!(named(&name), "{dir}/{name}.rs is not built by the gate: {output}");
        }
    }
    assert!(named("misc"), "semantic_conformance lives in misc: {output}");
    assert!(!named("glamour") && !named("e2e"), "feature-gated targets named: {output}");
}

#[test]
fn repository_contracts_do_not_disable_named_compiler_targets() {
    // repo-tests lives under tools/, not crates/. Treating every workspace
    // package as crates/<name> made this sound target-union optimization fail
    // closed to whole packages. A compiler + std + contract batch then linked
    // every root integration binary even though the expression ran a subset.
    let output = gate_nextest(&[
        "crates/witchy-types/src/typeck.rs",
        "std/dict.witchy",
        "tools/repo-tests/tests/repo_hygiene/architecture.rs",
    ]);
    let selection = output.lines().next().unwrap_or_default();
    for required in [
        "--lib",
        "--test typeck",
        "--test repo_hygiene",
        "--bin witchy",
        "--test misc",
    ] {
        assert!(
            selection.contains(required),
            "named target union omitted {required}: {output}",
        );
    }
    assert_eq!(selection.matches("--bin witchy").count(), 1, "{output}");
    assert_eq!(selection.matches("--test misc").count(), 1, "{output}");
}

#[test]
fn a_crate_with_a_binary_keeps_the_whole_package_form() {
    // `--lib` plus `tests/*.rs` would miss a bin target's unit tests. Check
    // EVERY crate that has one (or explicit targets) rather than one path that
    // happens to route elsewhere: whatever route the mapper takes for it, it
    // must never emit the narrowed `--lib` form.
    let root = workspace_root();
    let mut checked = 0;
    for entry in std::fs::read_dir(root.join("crates")).expect("list crates") {
        let dir = entry.expect("dir entry").path();
        let manifest = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap_or_default();
        let has_bin = dir.join("src/main.rs").exists() || dir.join("src/bin").is_dir()
            || manifest.lines().any(|line| line.starts_with("[[") || line.starts_with("autotests") || line.starts_with("autobins"));
        if !has_bin || !dir.join("src/lib.rs").exists() { continue; }
        checked += 1;
        let path = format!("crates/{}/src/lib.rs", dir.file_name().unwrap().to_string_lossy());
        let output = gate_nextest(&[path.as_str(), "crates/witchy-types/src/typeck.rs"]);
        let selection = output.lines().next().unwrap_or_default();
        assert!(
            !selection.contains("--lib"),
            "{path} has a bin or explicit targets but the gate narrowed to named targets: {output}",
        );
    }
    assert!(checked > 0, "no bin-bearing crate found; this test no longer checks anything");
}

#[test]
fn compiler_change_mixed_with_queue_scripts_names_no_foreign_target() {
    // The queue binaries live in merge-queue-tests, which the workspace stage
    // never selects. Naming them next to `-p witchy-types -p witchy` makes
    // cargo reject the whole run before building anything.
    for queue_path in ["scripts/merge-queue.sh", "scripts/test-for-paths.sh", "tools/merge-queue-tests/tests/test_for_paths.rs"] {
        let output = gate_nextest(&["crates/witchy-types/src/typeck.rs", queue_path]);
        let selection = output.lines().next().unwrap_or_default();
        let words: Vec<&str> = selection.split_whitespace().collect();
        for foreign in ["merge_queue", "test_for_paths"] {
            assert!(
                !words.windows(2).any(|pair| pair == ["--test", foreign]),
                "{queue_path}: names --test {foreign}, which no selected package has: {output}",
            );
        }
    }
}

#[test]
fn a_compiler_crate_change_runs_its_downstream_compiler_crates_tests() {
    // A change to an upstream pipeline crate is observed by the crates that
    // consume it: witchy-lower's canonical_pipeline admission tests exercise
    // the witchy-wir checker directly. bd239ce55 changed the checker, gated
    // green on witchy-wir's own tests alone, and left master red on
    // `canonical_pipeline::tests::mixed_var_list_write_back_origins_fail_closed`.
    for (changed, downstream) in [
        ("crates/witchy-syntax/src/lib.rs", &["witchy-types", "witchy-wir", "witchy-lower"][..]),
        ("crates/witchy-types/src/lib.rs", &["witchy-wir", "witchy-lower"][..]),
        ("crates/witchy-wir/src/canonical_safewir/check.rs", &["witchy-lower", "witchy-runtime"][..]),
    ] {
        let output = gate_nextest(&[changed]);
        for crate_name in downstream {
            assert!(
                output.contains(&format!("package({crate_name})")),
                "{changed} does not run {crate_name}'s tests: {output}",
            );
        }
    }
}

/// The RFC-pinned example modules (`example_tests::rfc0*`) are language
/// contracts -- what an RFC admits and refuses -- living in the `witchy` bin.
/// A compiler-stage change must run them at the gate and in the edit loop:
/// two view-carrier changes gated green without them and left master red on
/// two refusal pins (2026-09-22).
#[test]
fn compiler_stage_changes_run_the_rfc_pinned_example_modules() {
    for changed in [
        "crates/witchy-types/src/pipeline.rs",
        "crates/witchy-lower/src/canonical_lower.rs",
        "crates/witchy-wir/src/canonical_wasmir.rs",
    ] {
        let output = gate_nextest(&[changed]);
        assert!(
            output.contains("test(/^example_tests::rfc0/)") || output.contains("test(/^example_tests::/)"),
            "{changed} gates without the RFC-pinned example modules: {output}",
        );
        let commands = selected(&["--edit-loop", changed]);
        assert!(
            commands.iter().any(|command| command.contains("example_tests::rfc0")
                || command.contains("test(/^example_tests::/)")),
            "{changed} presubmits without the RFC-pinned example modules: {commands:?}",
        );
    }
}
