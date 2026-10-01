//! `&str` uses an owned String snapshot on the canonical normal-mode path.
//! This admits direct, returned, and trait-method calls before a zero-copy
//! string-slice carrier is available.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

#[test]
fn string_slice_views_survive_calls_and_owner_reassignment() {
    let source = r#"
import string

trait MatchText:
    fn matches(self: &Self, value: String) -> Bool

impl MatchText for str:
    fn matches(self: &str, value: String) -> Bool:
        string.to_string(self) == value

fn middle(value: &str) -> &str:
    string.slice(value, 1, 4)

fn main(console: Console):
    var text = "abcdef"
    let borrowed = string.as_str(&text)
    let middle_view = middle(borrowed)
    text = "xxxxx"
    let result = if middle_view.matches("bcd") && string.len(middle_view) == 3 && text == "xxxxx": 37 else: 0
    console.print("${result}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("type-check slice view");
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("admit slice view");
    let mut runtime = Runtime::batch().expect("runtime");
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities { print: true, quiet: true, ..Capabilities::none() }, 128)
        .expect("admitted instance");
    vm.run().expect("execute slice view");
    assert_eq!(vm.output(), vec!["37"]);
    assert_eq!(vm.canonical_heap_leaks().expect("heap audit"), Some(0));
}
