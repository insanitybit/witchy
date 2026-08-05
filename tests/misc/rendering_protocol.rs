//! Rendering is a language protocol, not behavior selected by an import.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

use super::temp_dir::TempDir;

fn write(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, source).unwrap();
    path
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

#[test]
fn interpolation_is_show_driven_with_or_without_import() {
    let dir = TempDir::new("rendering");
    let body = r#"
import set

type Label:
    Label(String)

impl Show for Label:
    fn show(self) -> String:
        match self:
            Label(s) -> "<" + s + ">"

fn main(console: Console):
    let label = Label("x")
    let values = set.from_list([1, 1, 2, 3])
    console.print("${label}")
    console.print("${90000ms}")
    console.print("${values}")
    console.print(f"f={label}/{90000ms}")
    console.print(show.render(label))
    show.say(console, label)
"#;
    let without_import = write(&dir, "without_import.witchy", body);
    let with_import = write(&dir, "with_import.witchy", &format!("import show\n{body}"));
    let expected = "<x>\n1m30s\n{1, 2, 3}\nf=<x>/1m30s\n<x>\n<x>\n";

    for path in [&without_import, &with_import] {
        let path = path.to_str().unwrap();
        let output = run(&[path]);
        assert!(
            output.status.success(),
            "{path}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected, "{path}");

        let parity = run(&["parity", path]);
        assert!(
            parity.status.success()
                && String::from_utf8_lossy(&parity.stdout).contains("outcome=agree"),
            "{path}: {}{}",
            String::from_utf8_lossy(&parity.stdout),
            String::from_utf8_lossy(&parity.stderr),
        );
    }

    std::fs::remove_dir_all(dir).unwrap();
}
