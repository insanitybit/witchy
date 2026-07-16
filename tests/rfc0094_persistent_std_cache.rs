//! RFC-0094: bundled-stdlib expansion artifacts survive test-process boundaries.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const CHILD_ENV: &str = "WITCHY_RFC0094_CACHE_CHILD";
const CACHE_DIR_ENV: &str = "WITCHY_TEST_STDLIB_CACHE_DIR";
const DUMP_ENV: &str = "WITCHY_RFC0094_LINKED_DUMP";

fn cache_root() -> PathBuf {
    std::env::temp_dir().join(format!("witchy-rfc0094-{}", std::process::id()))
}

fn run_child(root: &Path, dump: &Path) {
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .arg("--exact")
        .arg("persistent_cache_child_populates_semver")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(CACHE_DIR_ENV, root)
        .env(DUMP_ENV, dump)
        .output()
        .expect("spawn cache child");
    assert!(
        output.status.success(),
        "cache child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn semver_entry(root: &Path) -> PathBuf {
    let version = root.join("v1");
    let mut matches = std::fs::read_dir(&version)
        .expect("cache version directory")
        .flatten()
        .map(|entry| entry.path().join("semver.wstd"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected one semver cache entry under {version:?}");
    matches.pop().unwrap()
}

fn assert_same_file(actual: &Path, expected: &Path, context: &str) {
    let actual = std::fs::read(actual).expect("read comparison file");
    let expected = std::fs::read(expected).expect("read reference file");
    let first_difference = actual
        .iter()
        .zip(&expected)
        .position(|(left, right)| left != right);
    assert!(
        actual == expected,
        "{context}: actual={} bytes expected={} bytes first_difference={first_difference:?}",
        actual.len(),
        expected.len()
    );
}

#[test]
fn persistent_cache_child_populates_semver() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }
    let source = r#"
import semver

fn main(console: Console):
    match semver.parse("1.2.3"):
        Ok(version) -> console.print(semver.format(version))
        Err(_) -> console.print("error")
"#;
    let parsed = witchy::parser::parse_module(source).expect("parse semver consumer");
    let linked = witchy::pipeline::link(vec![("main".into(), parsed)], "main")
        .expect("link semver consumer");
    witchy::typeck::check(&linked).expect("typecheck semver consumer");
    let dump = std::env::var_os(DUMP_ENV).expect("child linked-AST dump path");
    std::fs::write(dump, format!("{linked:#?}"))
        .expect("write linked AST for parent comparison");
    let output = witchy::interpreter::run_module(linked, ".", Vec::new())
        .expect("run semver consumer");
    assert_eq!(output, ["1.2.3"]);
}

#[test]
fn expanded_std_cache_hits_across_processes_and_repairs_corruption() {
    let root = cache_root();
    let _ = std::fs::remove_dir_all(&root);
    let cold_dump = root.join("cold-linked.txt");
    let warm_dump = root.join("warm-linked.txt");
    let repaired_dump = root.join("repaired-linked.txt");

    run_child(&root, &cold_dump);
    let entry = semver_entry(&root);
    let first = std::fs::read(&entry).expect("read populated cache entry");
    let first_modified = std::fs::metadata(&entry)
        .expect("cache metadata")
        .modified()
        .expect("cache modification time");

    // Even filesystems with coarse timestamp granularity must distinguish a
    // rewrite here. A true cross-process hit performs no write at all.
    std::thread::sleep(Duration::from_millis(1100));
    run_child(&root, &warm_dump);
    let second_modified = std::fs::metadata(&entry)
        .expect("cache metadata after hit")
        .modified()
        .expect("cache modification time after hit");
    assert_eq!(first_modified, second_modified, "a cache hit must not rewrite the entry");
    assert!(std::fs::read(&entry).expect("read cache hit") == first);
    assert_same_file(
        &warm_dump,
        &cold_dump,
        "persistent cache hit changed the linked AST",
    );

    std::fs::write(&entry, b"corrupt cache entry").expect("corrupt cache entry");
    run_child(&root, &repaired_dump);
    let repaired = std::fs::read(&entry).expect("read repaired cache entry");
    assert_ne!(repaired, b"corrupt cache entry");
    assert!(repaired == first, "recompilation must restore the exact artifact");
    assert_same_file(
        &repaired_dump,
        &cold_dump,
        "corruption fallback changed the linked AST",
    );

    std::fs::remove_dir_all(root).expect("remove test cache");
}
