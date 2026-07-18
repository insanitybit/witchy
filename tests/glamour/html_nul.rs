//! Compile-time HTML marker collision coverage (BUG-439).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");
const ERROR: &str = "glamour html: NUL is not allowed in static template text";

fn workdir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "witchy-glamour-html-nul-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create workdir");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("projects/glamour/src/glamour.witchy"),
        dir.join("glamour.witchy"),
    )
    .expect("copy glamour module");
    dir
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run witchy")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn html_rejects_static_nul_without_affecting_real_holes_or_strings() {
    let dir = workdir();

    for (name, escaped) in [
        ("bare", r"\0"),
        ("nondigit-marker", r"\0x\0"),
        ("numeric-marker", r"\012\0"),
    ] {
        let source = format!(
            "from glamour import VNode\n\ntype Msg:\n    Noop\n\nfn view() -> VNode(Msg):\n    html\"<div>{escaped}</div>\"\n\nfn main(console: Console):\n    console.print(\"unused\")\n"
        );
        let file = format!("bad_{name}.witchy");
        std::fs::write(dir.join(&file), source).expect("write collision source");
        let output = run(&dir, &["check", &file]);
        let message = combined(&output);
        assert!(
            !output.status.success() && message.contains(ERROR),
            "{name} should fail with the controlled marker error:\n{message}"
        );
    }

    let control = r#"from glamour import VNode

type Msg:
    Noop

fn view(name: String) -> VNode(Msg):
    html"<div>${name}</div>"

fn main(console: Console):
    let ordinary = "\0"
    console.print("${ordinary.length()}")
"#;
    std::fs::write(dir.join("control.witchy"), control).expect("write control source");

    let direct = run(&dir, &["control.witchy"]);
    assert!(
        direct.status.success(),
        "ordinary NUL string and real HTML hole should run:\n{}",
        combined(&direct)
    );
    assert_eq!(String::from_utf8_lossy(&direct.stdout), "1\n");

    let parity = run(&dir, &["parity", "control.witchy"]);
    assert!(
        parity.status.success()
            && String::from_utf8_lossy(&parity.stdout).contains("outcome=agree"),
        "control should agree on both backends:\n{}",
        combined(&parity)
    );

    std::fs::remove_dir_all(dir).expect("remove workdir");
}
