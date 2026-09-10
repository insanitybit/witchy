//! Pending bounded regex-spans coverage probes. Keep all three implementations
//! independently adjudicated by literal outputs, including character offsets.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};
use witchy_wir::canonical_interpreter::{interpret_program, InterpreterValue};
use witchy_wir::canonical_safewir::FunctionId;

const PATTERNS: &str = include_str!("../../examples/patterns/src/patterns.witchy");
const PATTERNS_OUTPUT: &[&str] = &[
    "match  ^h.*o$  ~  hello",
    "no     ^h.*o$  ~  hi there",
    "match  colou?r  ~  color",
    "match  colou?r  ~  colour",
    "match  ab+a  ~  abbba",
    "no     ab+a  ~  aa",
    "match  cat  ~  the cat sat",
    "no     ^cat  ~  the cat sat",
];

fn span_source() -> String {
    format!(r#"
import regex

fn spans(console: Console, pattern: String, text: String):
    for span in regex.find_all(pattern, text):
        let (start, end) = span
        console.print("${{start}},${{end}}")

fn main(console: Console):
    console.print("unicode")
    spans(console, ".", "{}a{}")
    console.print("empty pattern")
    spans(console, "", "{}{}")
    console.print("alternation")
    spans(console, "(ab|c)+", "xabccy")
    console.print("no match")
    spans(console, "z+", "{}")
    console.print("done")
"#, '\u{e9}', '\u{1f642}', '\u{e9}', '\u{1f642}', '\u{e9}')
}

const SPAN_OUTPUT: &[&str] = &[
    "unicode", "0,1", "1,2", "2,3",
    "empty pattern", "0,0", "1,1", "2,2",
    "alternation", "1,5", "no match", "done",
];

fn four_way(source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source).expect("checked regex source");
    assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect("source interpreter"), expected);
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("regex spans must admit without a raw fallback");
    let entry = artifact.entry().expect("entry ABI");
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .expect("independently checked canonical oracle input");
    let capabilities = entry.capabilities().iter().copied()
        .map(InterpreterValue::entry_capability).collect();
    let oracle = interpret_program(&validated, function, capabilities, Default::default())
        .expect("independent canonical execution");
    assert_eq!(oracle.output, expected);
    assert_eq!(oracle.leaked_objects, 0);
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).expect("exact-byte admitted Wasm");
    vm.run().expect("Wasm execution");
    assert_eq!(vm.output(), expected);
    assert_eq!(vm.canonical_heap_leaks().unwrap().unwrap(), 0);
}

#[test]
fn regex_source_contract_including_invalid_pattern() {
    for (source, expected) in [(PATTERNS.to_owned(), PATTERNS_OUTPUT), (span_source(), SPAN_OUTPUT)] {
        let checked = witchy::resolve_std_only_checked(&source).expect("source checks");
        assert_eq!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
            .expect("source interpreter"), expected);
    }
    let checked = witchy::resolve_std_only_checked(r#"
import regex
fn main(console: Console):
    console.print("${regex.matches("[", "text")}")
"#).expect("regex validity is a runtime operation");
    let error = witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .expect_err("invalid regex aborts instead of silently not matching");
    assert!(error.to_string().contains("invalid regex pattern"), "{error}");
}

#[test]
fn regex_patterns_example_four_way() {
    four_way(PATTERNS, PATTERNS_OUTPUT);
}

#[test]
fn regex_spans_character_offsets_zero_width_and_nonmatches_four_way() {
    four_way(&span_source(), SPAN_OUTPUT);
}

#[test]
fn regex_invalid_pattern_aborts_all_three_implementations() {
    let source = r#"
import regex
fn main(console: Console):
    console.print("${regex.matches("[", "text")}")
"#;
    let checked = witchy::resolve_std_only_checked(source).expect("invalid regex is a runtime error");
    assert!(witchy::interpreter::run_checked_module(&checked, ".", Vec::new())
        .unwrap_err().to_string().contains("invalid regex pattern"));
    let artifact = codegen::compile_checked_module_artifact(&checked).expect_lowered("invalid pattern still admits");
    let entry = artifact.entry().unwrap();
    let function = FunctionId::new(entry.export().strip_prefix('f').unwrap().parse().unwrap());
    let validated = codegen::validate_checked_module_for_oracle(&checked).unwrap();
    let capabilities = entry.capabilities().iter().copied().map(InterpreterValue::entry_capability).collect();
    assert!(matches!(interpret_program(&validated, function, capabilities, Default::default()),
        Err(witchy_wir::canonical_interpreter::InterpretError::ExplicitTrap)));
    let mut runtime = Runtime::batch().unwrap();
    let mut vm = runtime.spawn_admitted(&artifact, Capabilities {
        print: true, quiet: true, ..Default::default()
    }, 256).unwrap();
    assert!(vm.run().is_err(), "invalid regex must trap in admitted Wasm");
    assert!(vm.output().is_empty());
}

#[test]
fn regex_argument_effect_order_and_owned_result_four_way() {
    four_way(r#"
import regex
fn marked(console: Console, label: String, value: String) -> String:
    console.print(label)
    value
fn main(console: Console):
    console.print("${regex.matches(marked(console, "pattern", "a+"), marked(console, "text", "baa"))}")
    let spans = regex.find_all("", "")
    console.print("${spans.length()}")
"#, &["pattern", "text", "true", "1"]);
}
