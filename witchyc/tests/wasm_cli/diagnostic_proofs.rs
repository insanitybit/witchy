//! Source locations and explanations for refused proof obligations.
use super::{Command, Fixture, PathBuf, fs};

#[test]
fn refused_owner_assignment_names_the_place() {
    assert_refused_proof_at(
        "messages/proof-points-at-assignment.refused.wy",
        "16:5",
        "`(*shelf).right` may still hold an owner when this assigns over it",
    );
}

#[test]
fn refused_unreachable_proof_points_at_the_statement() {
    assert_refused_proof_at(
        "messages/proof-points-at-unreachable.refused.wy",
        "12:9",
        "some path reaches this `unreachable`",
    );
}

#[test]
fn refused_postcondition_proof_points_at_the_return_and_names_the_clause() {
    assert_refused_proof_at(
        "messages/proof-points-at-claim.refused.wy",
        "17:12",
        "`ensures when Full = *slot: false` (case.wy:10) is not shown at this `return`",
    );
}

#[test]
fn refused_vacant_discard_names_the_owner() {
    assert_refused_proof_at(
        "vacant-discard/possibly-full.refused.wy",
        "12:24",
        "`held` may still hold an owner here",
    );
}

// Each diagnostic uses a separate fresh compiler and output path. The test
// harness can run the four cases independently when a checker is configured.
fn assert_refused_proof_at(source: &str, at: &str, said: &str) {
    use std::os::unix::fs::PermissionsExt;

    let binding = std::env::var_os("WITCHY_WASM_TEST_READY_BINDING");
    let lean = std::env::var_os("WITCHY_WASM_TEST_LEAN_ROOT");
    if binding.is_none() && lean.is_none() {
        eprintln!("skipped: configure WITCHY_WASM_TEST_READY_BINDING or WITCHY_WASM_TEST_LEAN_ROOT");
        return;
    }
    let fixture = Fixture::new();
    let root = fs::canonicalize(&fixture.0).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    fs::copy(fixtures.join(source), root.join("case.wy")).unwrap();
    for attempt in 0..=3 {
        let mut command = Command::new(env!("CARGO_BIN_EXE_witchyc"));
        command
            .args(["build", "case.wy", "-o", "case.wasm"])
            .current_dir(&root)
            .env_clear()
            .env("TMPDIR", &root);
        if let Some(binding) = &binding {
            command.env("WITCHY_WASM_READY_BINDING", binding);
        }
        if let Some(lean) = &lean {
            command.env("WITCHY_WASM_LEAN_ROOT", lean);
        }
        let output = command.output().unwrap();
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success() && !root.join("case.wasm").exists(), "{source}: {errors}");
        // A busy host can hit the helper deadline. Any other refusal must
        // identify the failed proof at its source.
        if binding.is_some() && attempt < 3 && errors.contains("native diagnostic deadline") {
            continue;
        }
        assert!(errors.contains(&format!("case.wy:{at}: W0402: ")), "{source} is not refused at {at}: {errors}");
        assert!(errors.contains(said), "{source}: {errors}");
        return;
    }
}
