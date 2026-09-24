//! RFC-0167 rows C and O: the installed `check`, `fmt`, and run workflows on
//! canonical contract source, driven through the exact-tip `witchy` binary.
//!
//! One program exercises views (record, String, Float, List), omitted and
//! written unit results, `let _ =`, `var` write-back through a loop, and a
//! closure capturing a record view. `check` accepts it, the run prints the
//! independent expected transcript, `fmt` leaves it unchanged, and `check`
//! names each retired spelling instead of a generic parse error.
use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_witchy");

fn witchy(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn witchy")
}

fn text(output: &Output) -> String {
    format!(
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn scratch_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("witchy-rfc0167-installed-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write(dir: &std::path::Path, name: &str, source: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, source).expect("write fixture");
    path.to_str().expect("utf-8 path").to_string()
}

#[test]
fn installed_check_names_each_retired_spelling() {
    let dir = scratch_dir("retired");
    for (name, source, diagnostic) in [
        (
            "mutref.witchy",
            "fn bump(r: &mut Int):\n    r\n\nfn main(console: Console):\n    console.print(\"x\")\n",
            "`&mut` is retired",
        ),
        (
            "lifetime.witchy",
            "fn first(r: &'a Int) -> Int:\n    (*r)\n\nfn main(console: Console):\n    console.print(\"x\")\n",
            "explicit reference lifetimes are retired",
        ),
        (
            "lifetimeparam.witchy",
            "fn first<'a>(r: &Int) -> Int:\n    (*r)\n\nfn main(console: Console):\n    console.print(\"x\")\n",
            "lifetime parameters are retired",
        ),
        (
            "writethrough.witchy",
            "fn main(console: Console):\n    var x = 3\n    let r = &x\n    *r = 4\n    console.print(show(x))\n",
            "write-through dereference is retired",
        ),
        (
            "nil.witchy",
            "fn main(console: Console):\n    let u = Nil\n    console.print(\"x\")\n",
            "`Nil` is retired",
        ),
    ] {
        let path = write(&dir, name, source);
        let check = witchy(&["check", &path]);
        assert!(!check.status.success(), "{name} must be refused: {}", text(&check));
        assert!(
            text(&check).contains(diagnostic),
            "{name} must name the retired spelling ({diagnostic}): {}",
            text(&check),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
