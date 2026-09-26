//! A crate that sets `[lib] test = false` never compiles its `cfg(test)` copy,
//! so an in-crate `#[test]` there would silently never run. Such crates keep
//! their tests in one integration binary under `tests/` instead (building the
//! lib a second time for a handful of unit tests was ~25% of the test build).

use std::fs;
use std::path::Path;

fn lib_tests_disabled(manifest: &str) -> bool {
    let Some(lib) = manifest.split("\n[").find(|section| section.starts_with("lib]")) else {
        return false;
    };
    lib.lines().any(|line| line.replace(' ', "") == "test=false")
}

/// `#[cfg(test)]`, and also `#[cfg(all(test, unix))]` or `any(test, ..)`: a
/// bare `test` predicate outside a string (`feature = "test-fixtures"` is not
/// one) and outside `not(..)`, which is harmless without a lib test target.
fn cfg_mentions_test(line: &str) -> bool {
    let unquoted: String = line.split('"').step_by(2).collect::<Vec<_>>().join(" ");
    !unquoted.contains("not(")
        && unquoted.split(|c: char| !c.is_alphanumeric() && c != '_').any(|word| word == "test")
}

fn in_crate_tests(dir: &Path, found: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            in_crate_tests(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let text = fs::read_to_string(&path).expect("read source");
            let attribute = |line: &str| {
                let line = line.trim_start();
                line.starts_with("#[test]") || (line.starts_with("#[cfg(") && cfg_mentions_test(line))
            };
            if text.lines().any(attribute) {
                found.push(path.display().to_string());
            }
        }
    }
}

#[test]
fn crates_without_a_lib_test_target_carry_no_in_crate_tests() {
    let crates = super::workspace_root().join("crates");
    let mut offenders = Vec::new();
    for entry in fs::read_dir(&crates).expect("read crates dir") {
        let dir = entry.expect("crate entry").path();
        let Ok(manifest) = fs::read_to_string(dir.join("Cargo.toml")) else { continue };
        if lib_tests_disabled(&manifest) && dir.join("src").is_dir() {
            in_crate_tests(&dir.join("src"), &mut offenders);
        }
    }
    assert!(
        offenders.is_empty(),
        "these crates set `[lib] test = false`, so tests in src/ never run; \
         put them in the crate's integration binary under tests/ instead: {offenders:#?}",
    );
}
