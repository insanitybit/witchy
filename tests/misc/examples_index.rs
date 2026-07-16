use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

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
