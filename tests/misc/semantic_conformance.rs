//! Independent language conformance floor.
//!
//! Backend parity is necessary but cannot detect a shared semantic mistake.
//! These cases name exact expected values, rejections, and authority directly;
//! the interpreter is used only in the positive-control fault injection that
//! demonstrates this distinction.

use std::collections::{BTreeMap, BTreeSet};

use witchy::runtime::{Capabilities, Runtime};
use witchy::{capabilities, codegen, interpreter, typeck};

struct ValueCase {
    name: &'static str,
    source: &'static str,
    expected: &'static [&'static str],
}

const VALUE_CASES: &[ValueCase] = &[
    ValueCase {
        name: "arithmetic precedence",
        source: r#"
fn main(console: Console):
    console.print("${1 + 2 * 3}")
"#,
        expected: &["7"],
    },
    ValueCase {
        name: "algebraic data and exhaustive matching",
        source: r#"
type Choice:
    Number(Int)
    Text(String)

fn render(choice: Choice) -> String:
    match choice:
        Number(value) -> "number:${value}"
        Text(value) -> "text:" + value

fn main(console: Console):
    console.print(render(Number(7)))
    console.print(render(Text("witchy")))
"#,
        expected: &["number:7", "text:witchy"],
    },
    ValueCase {
        name: "result propagation",
        source: r#"
fn increment(value: Result(Int, String)) -> Result(Int, String):
    let number = value?
    Ok(number + 1)

fn main(console: Console):
    match increment(Ok(41)):
        Ok(value) -> console.print("${value}")
        Err(error) -> console.print(error)
    match increment(Err("failed")):
        Ok(value) -> console.print("${value}")
        Err(error) -> console.print(error)
"#,
        expected: &["42", "failed"],
    },
];

fn checked(source: &str) -> witchy::pipeline::CheckedModule {
    witchy::resolve_std_only_checked(source).expect("check conformance program")
}

fn compiled_output(module: &witchy::pipeline::CheckedModule) -> Vec<String> {
    let wasm = codegen::compile_checked_module_binary(module)
        .expect_lowered("compile conformance program");
    let mut runtime = Runtime::batch().expect("create conformance runtime");
    let mut actor = runtime
        .spawn(
            &wasm,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            128,
        )
        .expect("spawn conformance program");
    actor.run().expect("run conformance program");
    actor.output()
}

fn compare_expected(
    name: &str,
    expected: &[&str],
    actual: &[String],
) -> Result<(), String> {
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_string()).collect();
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{name}: expected {expected:?}, got {actual:?}"))
    }
}

#[test]
fn compiled_language_values_match_independent_expectations() {
    for case in VALUE_CASES {
        let actual = compiled_output(&checked(case.source));
        compare_expected(case.name, case.expected, &actual)
            .unwrap_or_else(|error| panic!("{error}"));
    }
}

#[test]
fn invalid_program_matches_the_exact_language_rejection() {
    let error = typeck::check_str(
        "fn pick(x: Int, x: Int) -> Int:\n    x\n",
    )
    .expect_err("duplicate parameters must be rejected");
    assert_eq!(
        error,
        "type error: parameter `x` is declared more than once in function `pick`; \
         parameter names must be unique",
    );
}

#[test]
fn entry_authority_matches_the_exact_capability_footprint() {
    let module = checked(
        "fn main(console: Console, root: Dir[Read], network: Net[Connect, Tcp]):\n\
         \x20   console.print(\"ready\")\n",
    );
    let expected = BTreeMap::from([
        ("Console", BTreeSet::from(["Read", "Write"])),
        ("Dir", BTreeSet::from(["Read"])),
        ("Net", BTreeSet::from(["Connect", "Tcp"])),
    ]);
    assert_eq!(capabilities::run_grant(module.module()), expected);
}

#[test]
fn independent_expectation_rejects_a_shared_semantic_mutation() {
    let case = &VALUE_CASES[0];
    let mutated = case.source.replacen("1 + 2 * 3", "1 - 2 * 3", 1);
    let module = checked(&mutated);
    let compiled = compiled_output(&module);
    let interpreted = interpreter::run_checked_module(&module, ".", Vec::new())
        .expect("run mutated oracle program");

    assert_eq!(
        compiled, interpreted,
        "the seeded shared-stage mutation must preserve backend parity",
    );
    assert_eq!(
        compare_expected(case.name, case.expected, &compiled),
        Err(
            "arithmetic precedence: expected [\"7\"], got [\"-5\"]"
                .to_string(),
        ),
        "the independent expectation must reject a mutation parity cannot see",
    );
}
