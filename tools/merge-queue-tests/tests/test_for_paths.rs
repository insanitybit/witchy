use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
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
    let output = gate_nextest(&["src/example_tests/rfc0122_wasm_list_carrier.rs"]);
    assert!(
        output.contains("example_tests::rfc0122_wasm_list_carrier"),
        "example_tests-only mapping did not partition to the touched module: {output}",
    );
    assert!(
        !output.contains("test(/^example_tests::/)") || output.contains("rfc0122_wasm_list_carrier"),
        "example_tests-only mapping kept the whole matrix: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "example_tests-only mapping fell back to the full workspace: {output}",
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
        "tests/misc/architecture.rs",
        "scripts/test-for-paths.sh",
    ]);
    let args = output.lines().next().unwrap_or_default();
    assert!(args.contains("-p witchy"), "root package was omitted: {output}");
    assert!(
        !args.contains("--bin witchy"),
        "bin narrowing excluded the misc integration test: {output}",
    );
    assert!(
        output.contains("binary(misc)")
            && output.contains("documentation_examples_are_valid"),
        "mixed selection omitted a required target: {output}",
    );
}

/// The narrowing must stay off wherever the selection can name a target outside
/// the bin. `--bin` ANDs with `-p` instead of unioning, so a wrong narrowing
/// does not error -- it silently runs a subset and reports green.
#[test]
fn gate_nextest_does_not_narrow_past_the_bin_target() {
    // A crate package plus the rfc0* conformance binaries.
    let compiler = gate_nextest(&["crates/witchy-lower/src/codegen/mod.rs"]);
    assert!(
        !compiler.contains("--bin witchy"),
        "a compiler-stage change narrowed away its crate and conformance tests: {compiler}",
    );
    // A free-form witchy test filter that is not an `example_tests` name.
    let lsp = gate_nextest(&["src/lsp.rs"]);
    assert!(
        !lsp.contains("--bin witchy"),
        "a non-example_tests filter narrowed to the bin target: {lsp}",
    );
}

/// The narrowing above is only sound while `example_tests` is compiled into the
/// bin target. That is a structural fact, so assert it structurally rather than
/// trusting it to stay true.
#[test]
fn example_tests_is_declared_in_the_bin_target() {
    let main = std::fs::read_to_string(
        workspace_root().join("src/main.rs"),
    )
    .expect("read src/main.rs");
    assert!(
        main.contains("mod example_tests;"),
        "example_tests is no longer declared in the bin target; \
         scripts/test-for-paths.sh narrows docs gates to `--bin witchy` on that basis",
    );
}

#[test]
fn gate_nextest_crate_plus_example_tests_unions_without_workspace() {
    let output = gate_nextest(&[
        "crates/witchy-interp/src/lib.rs",
        "src/example_tests/rfc0122_wasm_list_carrier.rs",
    ]);
    let lines: Vec<_> = output.lines().collect();
    assert!(
        output.contains("-p witchy-interp"),
        "crate+example_tests mapping dropped the crate: {output}",
    );
    assert!(
        output.contains("example_tests::rfc0122_wasm_list_carrier"),
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
        "check/clippy package line should be mapped crates only: {output}",
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
        !output.contains("example_tests"),
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
        !output.contains("example_tests"),
        "interp-only crate mapping still auto-attached the example_tests matrix: {output}",
    );
    assert!(
        !output.contains("WORKSPACE"),
        "interp-only mapping fell back to WORKSPACE: {output}",
    );
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
        !output.contains("example_tests") && !output.contains("WORKSPACE"),
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
fn gate_nextest_nested_integration_module_fails_safe_to_workspace() {
    let output = gate_nextest(&[
        "crates/witchy-types/src/typeck.rs",
        "tests/browser/shim.rs",
        "tests/rfc0082/dynamic_methods.rs",
    ]);
    assert_eq!(
        output.trim(),
        "WORKSPACE",
        "nested integration modules became nonexistent binary selectors: {output}",
    );
}

#[test]
fn gate_nextest_src_commands_web_is_not_workspace() {
    let output = gate_nextest(&["src/commands/web/tests.rs"]);
    assert!(
        output.contains("commands::web"),
        "src/commands/web mapping did not select commands::web tests: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "src/commands/web mapping fell back to the full workspace: {output}",
    );
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
fn gate_nextest_highlighter_js_is_not_workspace() {
    let output = gate_nextest(&["web/witchy-highlight.js"]);
    assert!(
        output.contains("--test browser"),
        "highlighter JS did not select the browser test binary: {output}",
    );
    assert!(
        !output.contains("WORKSPACE") && !output.contains("--workspace"),
        "highlighter JS fail-safed to the full workspace: {output}",
    );
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
        Some("cargo nextest run --test misc -E 'test(architecture::root_integration_test_binaries_stay_within_budget)'")
    );
    assert!(commands.contains(&"./scripts/check.sh --fast".to_owned()), "{commands:?}");

    let existing = selected(&["tests/differential_fuzz.rs"]);
    assert!(
        !existing.iter().any(|command| command.contains("root_integration_test_binaries_stay_within_budget")),
        "an existing driver edit cannot increase the binary count: {existing:?}"
    );
}

#[test]
fn wir_abi_surfaces_run_the_catalog_contracts_first() {
    for path in [
        "crates/witchy-wir/src/wir_prelude.rs",
        "crates/witchy-wir/examples/abi_catalog.rs",
        "tests/misc/wasm_abi_catalog.rs",
        "spec/wasm-abi.md",
    ] {
        let commands = selected(&[path]);
        assert!(commands[0].contains("every_prelude_import_has_abi_metadata"), "{path}: {commands:?}");
        assert!(commands[0].contains("parsed_prelude_imports_are_complete"), "{path}: {commands:?}");
        assert!(commands[1].contains("public_spec_contains_the_generated_complete_catalog"), "{path}: {commands:?}");
        assert!(commands[1].contains("native_host_links_every_catalog_import_with_the_declared_signature"), "{path}: {commands:?}");
        assert!(commands[1].contains("capability_imports_name_their_authority_family"), "{path}: {commands:?}");
        assert!(commands[1].contains("precompiled_runner_authority_rows_are_cataloged"), "{path}: {commands:?}");
    }
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
fn registry_replay_edit_loop_stays_on_its_three_contracts() {
    let commands = selected(&[
        "--edit-loop",
        "rfcs/0165-acceptance-ledger.md",
        "src/source.rs",
        "src/source/package_inputs.rs",
        "src/source/package_inputs/registry.rs",
        "src/source/package_inputs/registry_replay_tests.rs",
        "src/source/package_inputs/registry_verify.witchy",
        "src/trusted_publisher/source_inputs.rs",
        "tests/misc/rfc0165_packaged_source.rs",
    ]);
    assert_eq!(commands.len(), 3, "{commands:?}");
    assert!(commands[0].contains("packaged_source_rejects_custom_owner_and_unsafe_input_paths"), "{commands:?}");
    assert!(commands[1].contains("compile_accepts_complete_authenticated_package_owners"), "{commands:?}");
    assert!(commands[2].contains("nextest-admitted.sh"), "{commands:?}");
    assert!(commands[2].contains("source::package_inputs::registry_replay_tests::"), "{commands:?}");
    assert!(!commands.iter().any(|command| command.contains("rfc0165_oracle")), "{commands:?}");
}

#[test]
fn standalone_registry_replay_tests_run_their_own_contract() {
    let commands = selected(&[
        "--edit-loop",
        "src/source/package_inputs/registry_replay_tests.rs",
    ]);
    assert_eq!(commands.len(), 3, "{commands:?}");
    assert!(
        commands.iter().any(|command| command.contains("source::package_inputs::registry_replay_tests::")),
        "standalone registry replay edits skipped their test family: {commands:?}",
    );
}

#[test]
fn edit_loop_stops_after_named_cheap_probes_when_available() {
    let commands = selected(&["--edit-loop", "crates/witchy-wir/src/wir_prelude.rs"]);
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert!(
        commands[0].contains("every_prelude_import_has_abi_metadata"),
        "{commands:?}"
    );
    assert!(commands[0].contains("--features native"), "{commands:?}");
    assert!(
        commands[1].contains("public_spec_contains_the_generated_complete_catalog"),
        "{commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|command| command.contains("check.sh --wasm")),
        "{commands:?}"
    );

    let focused = selected(&["--focused", "crates/witchy-wir/src/wir_prelude.rs"]);
    assert!(
        focused
            .iter()
            .any(|command| command.contains("check.sh --wasm")),
        "{focused:?}"
    );

    let mixed = selected(&[
        "--edit-loop",
        "crates/witchy-wir/src/wir_prelude.rs",
        "crates/witchy-lower/src/canonical_lower.rs",
    ]);
    assert_eq!(mixed.len(), 3, "canonical edit loop expanded past its sentinels: {mixed:?}");
    assert!(mixed[0].contains("constant_prefix_publication_rejects_gaps_and_missing_children"), "{mixed:?}");
    assert!(mixed[0].contains("test(boolean_reaches::)"), "{mixed:?}");
    assert!(mixed[0].contains("scalar_move_range_is_bounds_checked_and_admitted"), "{mixed:?}");
    assert!(mixed[0].contains("scalar_clone_range_is_ownership_safe_and_admitted"), "{mixed:?}");
    assert!(mixed[0].contains("owned_tail_nonprefix_range_requires_a_checked_end_bound"), "{mixed:?}");
    assert!(mixed[0].contains("canonical_capabilities_lower_only_to_externref"), "{mixed:?}");
    assert!(mixed[1].contains("canonical_pipeline::tests::list_string_"), "{mixed:?}");
    assert!(mixed[2].contains("native_host_links_every_catalog_import_with_the_declared_signature"), "{mixed:?}");
    assert!(mixed[2].contains("list_pop_projected_coordinates_and_nested_payloads_on_three_legs"), "{mixed:?}");
    assert!(mixed[2].contains("admitted_entry_without_a_console_grant_is_refused_before_it_runs"), "{mixed:?}");
    assert!(mixed[2].contains("admitted_guard_near_misses_trap_exactly_at_the_boundary"), "{mixed:?}");
    assert!(mixed[2].contains("admitted_aggregates_render_structurally_as_the_interpreter_prints_them"), "{mixed:?}");
    assert!(mixed[2].contains("rfc0165_owner_adapters::server_pool_oracle::"), "{mixed:?}");
    assert!(mixed[2].contains("source_integration_recipe_runs_selected_console_capture_and_reports_abort"), "{mixed:?}");
    assert!(mixed[2].contains("commands::embedded_pm::tests::"), "{mixed:?}");
    assert!(mixed[2].contains("rfc0165_oracle::admitted_observer_is_not_called_on_refusal"), "{mixed:?}");
    assert!(mixed[2].contains("commands::test_runner::canonical_integration_tests::"), "{mixed:?}");
    assert!(mixed[2].contains("rfc0165_enum_equality::"), "{mixed:?}");
    assert!(!mixed.iter().any(|command| command.contains("check.sh --wasm") || command == "cargo nextest run --test misc"), "{mixed:?}");
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
                || command.contains("cargo clippy")
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
fn canonical_wasmir_edit_keeps_the_expensive_admission_sentinel() {
    let commands = selected(&["--edit-loop", "crates/witchy-wir/src/canonical_wasmir.rs"]);
    assert_eq!(commands.len(), 3, "canonical WasmIR route changed shape: {commands:?}");
    assert!(
        commands[0].contains("outlined_address_predicate_mutation_fails_independent_admission")
            && commands[0].contains("outlined_retain_saturation_predicate_mutation_fails_admission")
            && commands[0].contains("outlined_memory_bounds_body_size_and_executes_shared_nullable_drops"),
        "direct WasmIR edits lost a canonical tail sentinel: {commands:?}",
    );
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
fn embedded_pm_edits_run_only_the_embedded_pm_module() {
    let commands = selected(&["--edit-loop", "src/commands/embedded_pm.rs"]);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(
        commands[0],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --bin witchy -E 'test(/^commands::embedded_pm::tests::/)'"
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
                && commands[0].contains("cargo nextest run --test misc"),
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
    assert!(commands[1].contains("canonical_pipeline::tests::list_string_"), "{commands:?}");
    assert!(commands[2].contains("admitted_aggregates_render_structurally_as_the_interpreter_prints_them"), "{commands:?}");
}

#[test]
fn owned_prefix_routes_stay_on_narrow_publication_surfaces() {
    let checker = selected(&["crates/witchy-wir/src/canonical_safewir/check/memory.rs"]);
    assert!(checker[0].contains("constant_successor_publication_keeps_the_existing_prefix_ownership"), "{checker:?}");
    assert!(checker[0].contains("constant_prefix_publication_rejects_gaps_and_missing_children"), "{checker:?}");

    for path in [
        "crates/witchy-lower/src/canonical_list_update.rs",
        "crates/witchy-lower/src/canonical_pipeline.rs",
    ] {
        let commands = selected(&[path]);
        assert!(commands[0].contains("canonical_pipeline::tests::list_string_"), "{path}: {commands:?}");
        assert!(commands[1].contains("admitted_aggregates_render_structurally_as_the_interpreter_prints_them"), "{path}: {commands:?}");
    }

    let launch = selected(&["tests/misc/rfc0165_admitted_launch.rs"]);
    assert!(launch[0].contains("admitted_aggregates_render_structurally_as_the_interpreter_prints_them"), "{launch:?}");

    let broad_lowering = selected(&["crates/witchy-lower/src/canonical_lower.rs"]);
    assert!(
        !broad_lowering.iter().any(|command| command.contains("admitted_aggregates_render_structurally_as_the_interpreter_prints_them")),
        "the shared lowering file is too broad for the publication probe: {broad_lowering:?}"
    );
}

#[test]
fn native_cli_entrypoints_use_the_bounded_compiler_shard() {
    let expected = [
        "cargo check -p witchy --all-targets",
        "cargo clippy -p witchy --all-targets -- -D clippy::correctness -D clippy::suspicious -D unused_must_use",
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
fn e2e_integration_test_keeps_its_dedicated_shard() {
    let output = route(&["tests/e2e.rs"]);
    assert!(output.contains("./scripts/check.sh --e2e"));
    assert!(!output.contains("cargo nextest run --test e2e"));
    assert!(!output.contains("./scripts/check.sh --fast"));
}

#[test]
fn nested_e2e_module_uses_the_real_cargo_target_and_module_filter() {
    assert_eq!(
        selected(&["--edit-loop", "tests/e2e/sandbox_grants.rs"]),
        ["./scripts/nextest-admitted.sh witchy -- cargo nextest run --test e2e -E 'test(/^sandbox_grants::/)'"],
    );
}

#[test]
fn complete_e2e_shard_subsumes_a_nested_module_probe() {
    let commands = selected(&["--edit-loop", "tests/e2e/offline_pm.rs", "tests/e2e.rs"]);
    assert_eq!(commands, ["./scripts/check.sh --e2e"], "{commands:?}");
    assert!(
        !commands.iter().any(|command| command.contains("--test e2e/")),
        "router emitted a nonexistent nested Cargo target: {commands:?}",
    );
}

#[test]
fn router_changes_run_router_regressions() {
    let output = route(&["scripts/test-for-paths.sh"]);
    assert!(output.contains("for f in scripts/*.sh; do bash -n"));
    assert!(output.contains("cargo nextest run -p merge-queue-tests --test test_for_paths"));
    assert!(output.contains("./scripts/check.sh --queue-infra"));
}

#[test]
fn agent_check_changes_verify_utility_qos_without_a_rust_build() {
    for path in ["scripts/agent-check.sh", "scripts/test-agent-check-qos.sh"] {
        assert_eq!(
            selected(&[path]),
            [
                "for f in scripts/*.sh; do bash -n \"$f\"; done",
                "./scripts/test-agent-check-qos.sh",
            ],
            "{path}"
        );
    }
}

#[test]
fn glamour_dom_harness_change_runs_its_structural_probe() {
    assert_eq!(
        selected_focused(&["tests/glamour/dom.rs"]),
        ["cargo nextest run --test glamour -E 'test(=dom::every_node_test_driver_is_wired_to_a_rust_test)'"],
    );
}

#[test]
fn queue_substrate_changes_use_the_hermetic_queue_shard() {
    assert_eq!(
        selected(&["scripts/merge-queue.sh"]),
        [
            "for f in scripts/*.sh; do bash -n \"$f\"; done",
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
fn stage_dependency_contract_and_probe_route_to_the_no_build_check() {
    for path in [
        "scripts/check-stage-dependencies.py",
        "spec/stage-dependencies.json",
    ] {
        assert_eq!(
            selected(&["--edit-loop", path]),
            ["python3 scripts/check-stage-dependencies.py"],
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
fn worktree_module_and_script_route_to_the_real_integration_binary() {
    assert_eq!(
        selected(&["tests/worktree/status.rs"]),
        ["./scripts/check.sh --fast"]
    );
    let commands = selected(&["scripts/worktree-status.sh"]);
    assert!(commands.contains(&"cargo nextest run --test worktree".to_owned()));
    assert!(!commands.iter().any(|command| command.contains("worktree_status")));
}

#[test]
fn spec_freshness_script_runs_its_behavior_check() {
    let output = route(&["scripts/check-spec-freshness.sh"]);
    assert!(output.contains("for f in scripts/*.sh; do bash -n"));
    assert!(output.contains("./scripts/check-spec-freshness.sh"));
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
    assert!(
        stdout.contains("documentation_examples_are_valid") || stdout.contains("example_tests"),
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
fn lower_path_keeps_wasm_but_drops_covered_nextest_checks() {
    assert_eq!(
        selected(&["crates/witchy-lower/src/lib.rs"]),
        ["./scripts/check.sh --fast", "./scripts/check.sh --wasm"]
    );
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
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert!(commands[0].contains("${CARGO_TARGET_DIR:-target}/debug/witchy"), "{commands:?}");
    assert!(commands[0].contains("fmt --check"), "{commands:?}");
    assert_eq!(commands[1], "./scripts/check.sh --e2e");
}

#[test]
fn glamour_runtime_changes_get_javascript_and_integration_checks() {
    assert_eq!(
        selected(&["web/witchy-runtime/glamour-dom.mjs"]),
        [
            "find web/witchy-runtime -type f -name '*.mjs' -exec node --check {} \\;",
            "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test glamour -E 'test(/^dom::/)'",
        ]
    );
}

#[test]
fn browser_runtime_modules_route_to_their_consolidated_binary_modules() {
    assert_eq!(
        selected(&["web/witchy-runtime/witchy-runnable.test.mjs"])[1],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test browser -E 'test(/^shim::/)'"
    );
    assert_eq!(
        selected(&["web/witchy-runtime/encoding-abi.test.mjs"])[1],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test browser -E 'test(/^encoding::/)'"
    );
}

#[test]
fn shared_browser_host_runs_every_dependent_integration_binary() {
    let commands = selected(&["web/witchy-runtime/witchy-runtime.mjs"]);
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert!(commands[0].contains("node --check"), "{commands:?}");
    assert_eq!(
        commands[1],
        "./scripts/nextest-admitted.sh witchy -- cargo nextest run --test browser --test glamour --test misc -E 'binary(browser) or (binary(glamour) and test(/^dom::/)) or (binary(misc) and test(/^wasm_abi_catalog::/))'"
    );
}

#[test]
fn import_catalog_change_runs_only_its_consolidated_module() {
    let commands = selected(&["web/witchy-runtime/import-catalog.test.mjs"]);
    assert_eq!(commands.len(), 2, "{commands:?}");
    assert!(commands[0].contains("node --check"), "{commands:?}");
    assert_eq!(
        commands[1],
        "cargo nextest run --test misc -E 'test(/^wasm_abi_catalog::/)'"
    );
}

#[test]
fn unwrapped_browser_module_still_gets_javascript_syntax_checks() {
    let output = route(&["web/witchy-runtime/hex-strict.test.mjs"]);
    assert!(output.contains("node --check"), "{output}");
    assert!(!output.contains("prose-only"), "{output}");
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

/// The converse: a change that cannot affect compiler semantics must NOT drag
/// the conformance suites in, or the selection stops being a selection.
#[test]
fn gate_nextest_queue_scripts_do_not_select_rfc_conformance_binaries() {
    let out = gate_nextest(&["scripts/merge-queue.sh"]);
    assert!(
        !out.contains("binary_id(/^witchy::rfc0/)"),
        "a queue-script change pulled in the conformance suites:\n{out}"
    );
}
