//! `console.read_line()` on admitted Wasm: one host read per call under the
//! actual Console token, the line without its terminator, empty at end of
//! input. `Console[Read]` and `Console[Write]` entry parameters share the one
//! Console token, and the read import exists only under a read grant.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

fn run(source: &str, caps: Capabilities) -> Result<Vec<String>, String> {
    let checked = witchy::resolve_std_only_checked(source).expect("check program");
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("compile program");
    let mut runtime = Runtime::batch().expect("create runtime");
    let mut actor = runtime.spawn_admitted(&artifact, caps, 128).map_err(|error| error.to_string())?;
    actor.run().map_err(|error| error.to_string())?;
    Ok(actor.output().to_vec())
}

fn reading(lines: &[&str]) -> Capabilities {
    Capabilities {
        print: true, quiet: true, console_read: true,
        console_input: Some(lines.iter().map(|line| line.to_string()).collect()),
        ..Default::default()
    }
}

#[test]
fn read_line_returns_each_line_without_its_terminator_then_empty() {
    let out = run("fn main(c: Console):\n    let a = c.read_line()\n    let b = c.read_line()\n    let z = c.read_line()\n    c.print(\"[${a}][${b}][${z}]\")\n",
        reading(&["one\r\n", "two"])).expect("run program");
    assert_eq!(out, ["[one][two][]"]);
}

#[test]
fn read_and_write_halves_of_console_are_separate_entry_parameters() {
    let out = run("fn main(input: Console[Read], output: Console[Write]):\n    let name = input.read_line()\n    output.print(\"hello, ${name}\")\n",
        reading(&["ada"])).expect("run program");
    assert_eq!(out, ["hello, ada"]);
}

#[test]
fn read_line_without_a_read_grant_cannot_run() {
    let error = run("fn main(c: Console):\n    c.print(c.read_line())\n",
        Capabilities { print: true, quiet: true, ..Default::default() })
        .expect_err("a program that reads Console needs a read grant");
    assert!(error.contains("console_read_line_at"), "{error}");
}
