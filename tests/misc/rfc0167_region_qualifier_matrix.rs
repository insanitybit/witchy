//! RFC-0167 row L: exact-tip region, qualifier, and admitted-layout matrix.
//!
//! This deliberately exercises only the checked canonical subset.  A refusal
//! is evidence for an unsupported proof boundary, not permission to insert a
//! normal-mode repair into opt lowering.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        witchy::opt::set_for_tests(None);
    }
}

fn assert_canonical_oracle_and_admitted_wasm(label: &str, source: &str, expected: &[&str]) {
    let checked = witchy::resolve_std_only_checked(source)
        .unwrap_or_else(|error| panic!("{label}: source must check: {error}"));
    let expected: Vec<String> = expected.iter().map(|line| (*line).to_owned()).collect();
    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered(&format!("{label}: unoptimized canonical lowering"));
    let entry = artifact.entry().expect("canonical region entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry
            .export()
            .strip_prefix('f')
            .expect("numeric entry export")
            .parse()
            .expect("numeric entry function"),
    );
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .unwrap_or_else(|error| panic!("{label}: canonical validation: {error}"));
    let arguments = entry
        .capabilities()
        .iter()
        .copied()
        .map(witchy_wir::canonical_interpreter::InterpreterValue::entry_capability)
        .collect();
    let oracle = witchy_wir::canonical_interpreter::interpret_program(
        &validated,
        function,
        arguments,
        Default::default(),
    )
    .unwrap_or_else(|error| panic!("{label}: canonical interpreter: {error}"));
    assert_eq!(oracle.output, expected, "{label}: canonical result");
    assert_eq!(oracle.leaked_objects, 0, "{label}: canonical heap audit");

    let mut runtime = Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities { print: true, print_int: true, quiet: true, ..Default::default() },
            256,
        )
        .unwrap_or_else(|error| panic!("{label}: admitted Wasm spawn: {error}"));
    vm.run()
        .unwrap_or_else(|error| panic!("{label}: admitted Wasm run: {error}"));
    assert_eq!(vm.output(), expected, "{label}: admitted Wasm result");
    assert_eq!(vm.canonical_heap_leaks().expect("canonical heap audit"), Some(0));
}

#[test]
fn row_l_checked_region_promotions_match_canonical_oracle_and_unoptimized_admitted_wasm() {
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimizationOverride;
    assert_eq!(
        witchy::opt::active_schema_key().enabled_mask(),
        0,
        "this is the optional-optimization-disabled canonical baseline",
    );

    assert_canonical_oracle_and_admitted_wasm(
        "normal region copy-out preserves the earlier snapshot",
        r#"
fn main(console: Console):
    let parent = [1]
    let value: List(Int) = region -> List(Int):
        var local = parent
        local.push(2)
        local
    console.print("${parent[0]}:${parent.length()}:${value.length()}")
"#,
        &["1:1:2"],
    );
    assert_canonical_oracle_and_admitted_wasm(
        "opt parent-owned result has a copy-free promotion proof",
        r#"
mode opt

fn main(console: Console):
    let parent = "parent-owned"
    let value: String = region -> String:
        parent
    console.print(value)
"#,
        &["parent-owned"],
    );
    assert_canonical_oracle_and_admitted_wasm(
        "opt packed region result carries an admitted exact descriptor",
        r#"
mode opt

type Point packed:
    x: Int
    y: Int

fn main(console: Console):
    let point: Point = region -> Point:
        Point(40, 2)
    console.print("${point.x + point.y}")
"#,
        &["42"],
    );
    assert_canonical_oracle_and_admitted_wasm(
        "exact fresh must constructor transfers its single obligation",
        r#"
must type Ticket:
    Ticket(Int)

fn make() -> Ticket:
    region -> Ticket:
        Ticket(42)

fn finish(own ticket: Ticket):
    let _ = 0

fn main():
    finish(make())
"#,
        &[],
    );
}

#[test]
fn row_l_qualifier_and_view_forms_accept_only_the_checked_relations() {
    witchy::resolve_std_only_checked(
        r#"
mode opt

fn shared_frozen(value: &frozen String) -> &frozen String:
    value

fn fresh_unique() -> unique List(Int):
    region -> unique List(Int):
        [1, 2]

fn transfer_frozen(own parent: frozen String) -> frozen String:
    region -> frozen String:
        parent
"#,
    )
    .expect("row L preserves the declared frozen/unique/reference relations");

    for (label, source, diagnostic) in [
        (
            "outer frozen reference",
            "mode opt\n\nfn bad(value: frozen &String) -> ():\n    ()\n",
            "outer `frozen` reference qualifiers are invalid",
        ),
        (
            "exclusive frozen referent",
            "mode opt\n\nfn bad(value: &mut frozen String) -> ():\n    ()\n",
            "`&mut` is retired",
        ),
        (
            "two ownership qualifiers",
            "mode opt\n\nfn bad(value: unique frozen String) -> ():\n    ()\n",
            "cannot qualify the same value",
        ),
        (
            "region-born reference escape",
            "mode opt\n\nfn bad() -> &String:\n    region:\n        let local = \"region-owned\"\n        let view = &local\n        view\n",
            "region-born view",
        ),
    ] {
        let error = witchy::resolve_std_only_checked(source)
            .expect_err(&format!("row L {label} must fail closed"))
            .to_string();
        let required_terms: Vec<&str> = if label == "region-born reference escape" {
            vec![diagnostic, "backing root is rewound"]
        } else {
            vec![diagnostic]
        };
        assert!(
            required_terms.iter().all(|term| error.contains(*term)),
            "row L {label} diagnostic changed or form was accepted: {error}",
        );
    }
}

#[test]
fn row_l_canonical_lowering_refuses_hidden_opt_copy_and_nonfresh_must_promotion() {
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimizationOverride;

    for (label, source, required_terms) in [
        (
            "opt region-born managed result without a copy-free proof",
            r#"
mode opt

fn main(console: Console):
    let value: String = region -> String:
        "must-not-hide" + "-copy"
    console.print(value)
"#,
            ["region", "copy-free"],
        ),
        (
            "must region result stored before return",
            r#"
must type Ticket:
    Ticket(Int)

fn make() -> Ticket:
    region -> Ticket:
        let ticket = Ticket(1)
        ticket

fn finish(own ticket: Ticket):
    let _ = 0

fn main():
    finish(make())
"#,
            ["region", "must-bearing"],
        ),
    ] {
        let checked = witchy::resolve_std_only_checked(source)
            .unwrap_or_else(|error| panic!("{label}: source must check before canonical refusal: {error}"));
        let refusal = codegen::compile_checked_module_artifact(&checked).expect_unsupported(label);
        assert!(
            required_terms.iter().all(|term| refusal.message.contains(*term)),
            "{label}: refusal must name the exact proof boundary: {}",
            refusal.message,
        );
    }
}
