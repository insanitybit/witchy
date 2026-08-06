use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

fn example_sources(examples: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for entry in fs::read_dir(examples).expect("read examples directory") {
        let entry = entry.expect("read example directory entry");
        if !entry.file_type().expect("read example file type").is_dir() {
            continue;
        }
        let src = entry.path().join("src");
        let Ok(files) = fs::read_dir(src) else {
            continue;
        };
        sources.extend(
            files
                .map(|file| file.expect("read example source entry").path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "witchy")),
        );
    }
    sources.sort();
    sources
}

#[test]
fn example_sources_use_current_rune_paths() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stale = Regex::new(r"examples/[A-Za-z0-9_-]+\.witchy").expect("valid stale-path regex");
    let current = Regex::new(r"examples/[A-Za-z0-9_-]+/src/[A-Za-z0-9_-]+\.witchy")
        .expect("valid current-path regex");

    let sources = example_sources(&root.join("examples"));
    assert!(!sources.is_empty(), "no example sources discovered");
    for path in sources {
        let source = fs::read_to_string(&path).expect("read example source");
        assert!(
            !stale.is_match(&source),
            "{} contains a retired flat example path",
            path.strip_prefix(&root).unwrap_or(&path).display()
        );
        for referenced in current.find_iter(&source) {
            assert!(
                root.join(referenced.as_str()).is_file(),
                "{} references missing {}",
                path.strip_prefix(&root).unwrap_or(&path).display(),
                referenced.as_str()
            );
        }
    }
}
