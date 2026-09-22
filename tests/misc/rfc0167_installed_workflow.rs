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

const CONTRACT: &str = r#"type Point:
    x: Int
    y: Int

fn keep(p: &Point) -> &Point:
    p

fn label_length(s: &String) -> Int:
    (*s).length()

fn grow(var xs: List(Int)):
    for i in 0..3:
        xs.push(i)

fn note(console: Console, text: String):
    console.print(text)

fn announce(console: Console) -> ():
    console.print("unit")

fn main(console: Console):
    var p = Point(x: 3, y: 4)
    let view = keep(&p)
    p = Point(x: 9, y: 9)
    let name = "witchy"
    let scale = 2.5
    let sv = &scale
    var xs = [7]
    let before = &xs
    grow(xs)
    let f = fn(k: Int) -> Int: k + (*view).x
    let _ = label_length(&name)
    note(console, "views")
    announce(console)
    console.print(show((*view).x * 10 + (*view).y))
    console.print(show(label_length(&name)))
    let big: Int = if (*sv) > 2.0: 1 else: 0
    console.print(show(big))
    console.print(show((*before).length() * 10 + xs.length()))
    console.print(show(f(100)))
"#;

/// Independent expectation: the record view keeps (3, 4) after `p` is
/// reassigned (34); "witchy" has 6 bytes; 2.5 > 2.0; the list view keeps one
/// element while `grow` leaves four (14); the closure reads x = 3 (103).
const EXPECTED: &str = "views\nunit\n34\n6\n1\n14\n103\n";

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
fn installed_check_run_and_fmt_accept_canonical_contract_source() {
    let dir = scratch_dir("contract");
    let path = write(&dir, "contract.witchy", CONTRACT);

    let check = witchy(&["check", &path]);
    assert!(check.status.success(), "check: {}", text(&check));

    let run = witchy(&[&path]);
    assert!(run.status.success(), "run: {}", text(&run));
    assert_eq!(String::from_utf8_lossy(&run.stdout), EXPECTED, "run: {}", text(&run));

    let fmt = witchy(&["fmt", &path]);
    assert!(fmt.status.success(), "fmt: {}", text(&fmt));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CONTRACT, "canonical source is a fmt fixed point");
    let fmt_check = witchy(&["fmt", "--check", &path]);
    assert!(fmt_check.status.success(), "fmt --check: {}", text(&fmt_check));
    let _ = std::fs::remove_dir_all(&dir);
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
