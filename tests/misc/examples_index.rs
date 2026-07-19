use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

fn runnable_examples(examples: &Path) -> BTreeSet<String> {
    fs::read_dir(examples)
        .expect("read examples directory")
        .map(|entry| entry.expect("read example directory entry"))
        .filter(|entry| entry.file_type().expect("read example file type").is_dir())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().expect("example name is UTF-8");
            let path = entry.path();
            let entrypoint = path.join("src").join(format!("{name}.witchy"));
            (path.join("witchy.toml").is_file() && entrypoint.is_file()).then_some(name)
        })
        .collect()
}

fn project_locks(projects: &Path) -> Vec<PathBuf> {
    let mut locks = Vec::new();
    for workspace in fs::read_dir(projects).expect("read example projects directory") {
        let workspace = workspace.expect("read example project entry");
        if !workspace.file_type().expect("read project entry type").is_dir() {
            continue;
        }
        for rune in fs::read_dir(workspace.path()).expect("read example project workspace") {
            let rune = rune.expect("read example project rune");
            if !rune.file_type().expect("read project rune type").is_dir() {
                continue;
            }
            let lock = rune.path().join("witchy.lock");
            if lock.is_file() {
                locks.push(lock);
            }
        }
    }
    locks.sort();
    locks
}

fn inventory_section(readme: &str) -> &str {
    let start_marker = "<!-- runnable-inventory:start -->";
    let end_marker = "<!-- runnable-inventory:end -->";
    let start = readme
        .find(start_marker)
        .expect("examples README has a runnable inventory start marker");
    let body = &readme[start + start_marker.len()..];
    let end = body
        .find(end_marker)
        .expect("examples README has a runnable inventory end marker");
    &body[..end]
}

#[test]
fn runnable_examples_are_exhaustively_indexed() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let examples = root.join("examples");
    let readme = fs::read_to_string(examples.join("README.md"))
        .expect("read examples/README.md");
    let section = inventory_section(&readme);
    let link = Regex::new(r"\[[^\]]+\]\(([^)]+)\)").expect("valid link regex");
    let destinations: Vec<String> = link
        .captures_iter(section)
        .map(|capture| {
            capture[1]
                .strip_suffix('/')
                .expect("inventory links end in a slash")
                .to_owned()
        })
        .collect();
    let indexed: BTreeSet<String> = destinations.iter().cloned().collect();

    assert_eq!(
        destinations.len(),
        indexed.len(),
        "runnable inventory contains a duplicate link"
    );
    assert_eq!(
        indexed,
        runnable_examples(&examples),
        "update the complete rune inventory in examples/README.md"
    );
}

#[test]
fn committed_project_locks_match_their_path_dependencies() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let locks = project_locks(&root.join("examples/projects"));
    assert!(!locks.is_empty(), "no committed example project locks discovered");

    for lock in locks {
        let rune = lock.parent().expect("lock has a rune directory");
        let workspace = rune.parent().expect("rune has a workspace directory");
        let rune_name = rune.file_name().expect("rune directory has a name");
        let before = fs::read(&lock).expect("read committed project lock");
        let output = Command::new(env!("CARGO_BIN_EXE_witchy"))
            .current_dir(workspace)
            .args(["pm", "verify"])
            .arg(rune_name)
            .output()
            .expect("run witchy pm verify");

        assert!(
            output.status.success(),
            "{} is stale:\nstdout: {}\nstderr: {}",
            lock.strip_prefix(&root).unwrap_or(&lock).display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(&lock).expect("re-read committed project lock"),
            before,
            "pm verify modified {}",
            lock.strip_prefix(&root).unwrap_or(&lock).display()
        );
    }
}
