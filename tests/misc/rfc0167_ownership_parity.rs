//! RFC-0167 ownership rows with independent expectations, the canonical
//! SafeWIR interpreter oracle, and admitted unoptimized Wasm.

use witchy::codegen;
use witchy::runtime::{Capabilities, Runtime};

struct ResetOptimizationOverride;

impl Drop for ResetOptimizationOverride {
    fn drop(&mut self) {
        witchy::opt::set_for_tests(None);
    }
}

struct PositiveCase {
    row: &'static str,
    source: &'static str,
    expected: &'static [&'static str],
}

const POSITIVE_CASES: &[PositiveCase] = &[
    PositiveCase {
        row: "D",
        source: r#"
fn consume(own text: String) -> String:
    text

fn main(console: Console):
    let original = "hello"
    let snapshot = original
    let result = consume(original)
    console.print("${snapshot}:${result}")
"#,
        expected: &["hello:hello"],
    },
    PositiveCase {
        row: "F",
        source: r#"
fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [2]
    values

fn main(console: Console):
    var values: frozen List(Int) = [1]
    let replacement = replace(values)
    console.print("${values[0]}:${replacement[0]}")
"#,
        expected: &["2:2"],
    },
    PositiveCase {
        row: "F",
        source: r#"
type Snapshot:
    values: List(Int)

fn replace(var snapshot: frozen Snapshot) -> frozen Snapshot:
    snapshot = Snapshot([2])
    snapshot

fn main(console: Console):
    var snapshot: frozen Snapshot = Snapshot([1])
    let replacement = replace(snapshot)
    console.print("${snapshot.values[0]}:${replacement.values[0]}")
"#,
        expected: &["2:2"],
    },
    PositiveCase {
        row: "F",
        source: r#"
fn carry(own value: frozen a) -> frozen a:
    value

fn main(console: Console):
    let value: frozen List(Int) = [7]
    let result = carry(value)
    console.print("${result[0]}")
"#,
        expected: &["7"],
    },
    PositiveCase {
        row: "F",
        source: r#"
fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [3]
    values

fn main(console: Console):
    var values: frozen List(Int) = [1]
    let action = replace
    let result = action(values)
    console.print("${values[0]}:${result[0]}")
"#,
        expected: &["3:3"],
    },
    PositiveCase {
        row: "E",
        source: r#"
fn update(var values: unique List(Int)):
    values[0] = 9

fn main(console: Console):
    var values: unique List(Int) = [1, 2]
    update(values)
    console.print("${values[0]}")
"#,
        expected: &["9"],
    },
    PositiveCase {
        row: "E",
        source: r#"
fn relay(own values: unique List(Int)) -> unique List(Int):
    values

fn main(console: Console):
    let values: unique List(Int) = [4, 5]
    let result = relay(values)
    console.print("${result}")
"#,
        expected: &["[4, 5]"],
    },
    PositiveCase {
        row: "E",
        source: r#"
fn relay(own value: unique Int) -> unique Int:
    value

fn main(console: Console):
    let value: unique Int = 41
    let result = relay(value)
    console.print("${result + 1}")
"#,
        expected: &["42"],
    },
];

struct NegativeCase {
    row: &'static str,
    source: &'static str,
    expected_diagnostic: &'static str,
}

const NEGATIVE_CASES: &[NegativeCase] = &[NegativeCase {
    row: "E",
    source: r#"
mode opt

fn update(var values: unique List(Int)):
    values[0] = 9

fn main(console: Console):
    var values = [1, 2]
    update(values)
"#,
    expected_diagnostic: "cannot repair ordinary storage into a `unique` parameter",
}];

fn run_positive(case: &PositiveCase, mode: &str) {
    let source = if mode == "normal" {
        case.source.to_owned()
    } else {
        format!("mode opt\n\n{}", case.source)
    };
    let checked = witchy::resolve_std_only_checked(&source)
        .unwrap_or_else(|error| panic!("row {} {mode} source checks: {error}", case.row));
    let expected: Vec<String> = case.expected.iter().map(|line| (*line).to_owned()).collect();

    let artifact = codegen::compile_checked_module_artifact(&checked)
        .expect_lowered(&format!("row {} {mode} unoptimized canonical lowering", case.row));
    let entry = artifact.entry().expect("canonical ownership entry");
    let function = witchy_wir::canonical_safewir::FunctionId::new(
        entry
            .export()
            .strip_prefix('f')
            .expect("numeric entry export")
            .parse()
            .expect("numeric entry function"),
    );
    let validated = codegen::validate_checked_module_for_oracle(&checked)
        .unwrap_or_else(|error| panic!("row {} {mode} canonical validation: {error}", case.row));
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
    .unwrap_or_else(|error| panic!("row {} {mode} canonical interpreter: {error}", case.row));
    assert_eq!(oracle.output, expected, "row {} {mode} canonical result", case.row);
    assert_eq!(oracle.leaked_objects, 0, "row {} {mode} canonical heap audit", case.row);

    let mut runtime = Runtime::batch().expect("canonical runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            Capabilities {
                print: true,
                print_int: true,
                quiet: true,
                ..Default::default()
            },
            256,
        )
        .unwrap_or_else(|error| panic!("row {} {mode} admitted Wasm: {error}", case.row));
    vm.run()
        .unwrap_or_else(|error| panic!("row {} {mode} Wasm execution: {error}", case.row));
    assert_eq!(vm.output(), expected, "row {} {mode} admitted Wasm result", case.row);
    assert_eq!(vm.canonical_heap_leaks().unwrap(), Some(0));
}

#[test]
fn supported_ownership_rows_match_canonical_oracle_and_unoptimized_wasm() {
    witchy::opt::set_for_tests(Some(witchy::opt::OptSet::none()));
    let _reset = ResetOptimizationOverride;
    for case in POSITIVE_CASES {
        run_positive(case, "normal");
        run_positive(case, "opt");
    }
}

#[test]
fn frozen_deep_mutation_is_rejected_before_lowering() {
    let direct_result = r#"
mode opt

fn pass(own values: frozen List(Int)) -> frozen List(Int):
    values

fn main():
    let values: frozen List(Int) = [1]
    var result = pass(values)
    result[0] = 2
"#;
    let indirect_result = r#"
mode opt

fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [2]
    values

fn main():
    var values: frozen List(Int) = [1]
    let action = replace
    var result = action(values)
    result[0] = 3
"#;
    let root_mutation = r#"
mode opt

fn bad(var values: frozen List(Int)):
    values[0] = 2

fn main():
    ()
"#;
    for (case, source, root) in [
        ("root mutation", root_mutation, "values"),
        ("direct frozen result", direct_result, "result"),
        ("indirect frozen result", indirect_result, "result"),
    ] {
        let error = witchy::resolve_std_only_checked(source)
            .expect_err("row F must reject deep mutation through frozen storage")
            .to_string();
        assert!(
            error.contains(&format!("cannot mutate through frozen value `{root}`")),
            "row F {case} diagnostic must identify the frozen place: {error}"
        );
    }
}

#[test]
fn frozen_qualifier_matrix_keeps_immutable_views_and_capabilities_fail_closed() {
    let accepted = r#"
mode opt

fn share(value: &frozen String) -> &frozen String:
    value
"#;
    witchy::resolve_std_only_checked(accepted)
        .expect("row F permits an immutable view of frozen storage");

    for (case, source, diagnostic) in [
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
            "unique frozen value",
            "mode opt\n\nfn bad(value: unique frozen String) -> ():\n    ()\n",
            "cannot qualify the same value",
        ),
        (
            "frozen aggregate exclusive field",
            "mode opt\n\ntype Holder:\n    value: &mut String\n\nfn bad(value: frozen Holder) -> ():\n    ()\n",
            "`&mut` is retired",
        ),
        (
            "frozen host capability",
            "fn bad(value: frozen Console) -> ():\n    ()\n",
            "frozen capabilities do not exist",
        ),
        (
            "frozen aggregate transitively carrying a capability",
            "type HeldConsole:\n    console: Console\n\nfn bad(value: frozen HeldConsole) -> ():\n    ()\n",
            "frozen capabilities do not exist",
        ),
        (
            "direct frozen must payload",
            "must type Ticket:\n    Ticket(Int)\n\nfn bad(value: frozen Ticket) -> ():\n    ()\n",
            "must-consume payload",
        ),
        (
            "frozen aggregate transitively carrying a must payload",
            "must type Ticket:\n    Ticket(Int)\n\ntype Envelope:\n    ticket: Ticket\n\nfn bad(value: frozen Envelope) -> ():\n    ()\n",
            "must-consume payload",
        ),
        (
            "frozen generic storage carrying a must payload",
            "must type Ticket:\n    Ticket(Int)\n\ntype Boxed(a):\n    value: a\n\nfn bad(value: frozen Boxed(Ticket)) -> ():\n    ()\n",
            "must-consume payload",
        ),
        (
            "frozen aggregate carrying an affine once callable",
            "type Deferred:\n    action: once fn() -> ()\n\nfn bad(value: frozen Deferred) -> ():\n    ()\n",
            "once-callable",
        ),
        (
            "frozen generic storage carrying a unique payload",
            "type Boxed(a):\n    value: a\n\nfn bad(value: frozen Boxed(unique String)) -> ():\n    ()\n",
            "unique (affine)",
        ),
    ] {
        let error = match witchy::resolve_std_only_checked(source) {
            Ok(_) => panic!("row F {case} was accepted"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains(diagnostic),
            "row F {case} diagnostic changed or form was accepted: {error}"
        );
    }
}

#[test]
fn frozen_erased_payload_shells_are_not_treated_as_copyable_storage() {
    for (case, source, diagnostic) in [
        (
            "Dynamic envelope",
            "import dynamic\n\nfn bad() -> ():\n    let envelope = dynamic.dynamic(1)\n    let frozen: frozen dynamic.Dynamic = envelope\n    ()\n",
            "frozen values cannot contain an existential payload",
        ),
        (
            "trait existential",
            "trait Marker:\n    fn mark(let self) -> Int\n\nimpl Marker for Int:\n    fn mark(let self) -> Int:\n        self\n\nfn bad() -> ():\n    let value = 1 as dyn Marker\n    let frozen: frozen dyn Marker = value\n    ()\n",
            "frozen values cannot contain an existential payload",
        ),
    ] {
        let error = witchy::resolve_std_only_checked(source)
            .expect_err("row F must not freeze an erased payload shell")
            .to_string();
        assert!(
            error.contains(diagnostic),
            "row F {case} diagnostic changed or form was accepted: {error}"
        );
    }

    witchy::resolve_std_only_checked(
        "fn keep(value: frozen List(Int)) -> frozen List(Int):\n    value\n",
    )
    .expect("a frozen copyable payload remains transportable");
}

#[test]
fn frozen_indirect_var_calls_require_every_joined_callee_to_replace() {
    let source = r#"
mode opt

fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [2]
    values

fn mutate(var values: List(Int)) -> List(Int):
    values = [3]
    values

fn main():
    var values: frozen List(Int) = [1]
    let action = if true: replace else: mutate
    let _ = action(values)
"#;
    let error = witchy::resolve_std_only_checked(source)
        .expect_err("a conditional function value cannot bless frozen storage when one branch may mutate deeply")
        .to_string();
    assert!(
        error.contains("cannot pass frozen value `values` to a non-frozen or erased `var` parameter contract"),
        "row F joined callable must fail closed: {error}"
    );
}

#[test]
fn opt_unique_requires_an_explicit_unique_owner_at_the_source_boundary() {
    for case in NEGATIVE_CASES {
        let error = witchy::resolve_std_only_checked(case.source)
            .expect_err("opt unique call without a unique owner must reject");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains(case.expected_diagnostic),
            "row {} diagnostic changed: {diagnostic}",
            case.row
        );
    }
}

#[test]
fn root_unique_owner_return_proof_rejects_aliases_and_nonmatching_shapes() {
    let accepted = r#"
fn list_early(own values: unique List(Int), return_now: Bool) -> unique List(Int):
    if return_now:
        return values
    values

fn dict_early(own values: unique Dict(String, Int), return_now: Bool) -> unique Dict(String, Int):
    if return_now:
        return values
    values

fn list_branch(own values: unique List(Int), keep_tail: Bool) -> unique List(Int):
    if keep_tail:
        values
    else:
        return values

fn dict_branch(own values: unique Dict(String, Int), keep_tail: Bool) -> unique Dict(String, Int):
    if keep_tail:
        values
    else:
        return values

fn nested_literal() -> unique List(unique String):
    ["fresh"]

fn nested_direct_result() -> unique List(unique String):
    nested_literal()

fn nested_transfer(own values: unique List(unique String)) -> unique List(unique String):
    values
"#;
    let accepted_opt = format!("mode opt\n\n{accepted}");
    for source in [accepted, accepted_opt.as_str()] {
        witchy::resolve_std_only_checked(source)
            .expect("fresh, direct, and transferred nested unique owners survive control flow");
    }

    let aliases = r#"
fn bad(own values: unique List(Int)) -> unique List(Int):
    let alias = values
    return alias
"#;
    let error = witchy::resolve_std_only_checked(aliases)
        .expect_err("an alias must not inherit a root-owner return proof")
        .to_string();
    assert!(
        error.contains("no root-owner proof"),
        "alias rejection must remain at the root-owner proof boundary: {error}"
    );

    let mismatched = r#"
fn bad(own values: unique List(String)) -> unique List(Int):
    if true:
        return values
    []
"#;
    let error = witchy::resolve_std_only_checked(mismatched)
        .expect_err("an own unique parameter of another type must not prove this result")
        .to_string();
    assert!(
        error.contains("expected") || error.contains("no root-owner proof"),
        "mismatched owner type must reject rather than inherit a name-only proof: {error}"
    );

    let projection = r#"
fn bad(own values: unique List(unique List(Int))) -> unique List(Int):
    values[0]
"#;
    let error = witchy::resolve_std_only_checked(projection)
        .expect_err("a projection must not inherit the enclosing root-owner proof")
        .to_string();
    assert!(
        error.contains("no root-owner proof"),
        "projection rejection must remain at the root-owner proof boundary: {error}"
    );

    let nested_alias = r#"
fn bad(own values: unique List(unique List(Int))) -> unique List(unique List(Int)):
    let alias = values
    alias
"#;
    let error = witchy::resolve_std_only_checked(nested_alias)
        .expect_err("an alias must not inherit nested unique ownership")
        .to_string();
    assert!(
        error.contains("no root-owner proof"),
        "nested aliases must remain outside the root-owner proof: {error}"
    );

    let ordinary_nested_element = r#"
fn bad() -> unique List(unique String):
    let ordinary = "not an owner"
    [ordinary]
"#;
    let error = witchy::resolve_std_only_checked(ordinary_nested_element)
        .expect_err("an ordinary nested element cannot be promoted into a unique owner")
        .to_string();
    assert!(
        error.contains("no root-owner proof"),
        "ordinary nested storage must not satisfy the recursive owner proof: {error}"
    );
}

/// An indirect call may carry a `var` write-back only when every candidate
/// callee writes back the same way.
///
/// Row F takes a function value of a callable with a `var` parameter. The
/// indirect-call check refused it because the target's write-back result is
/// `DerivedResult::ReallocatesParameter`, which the admitted-result list did
/// not carry. The gate stays closed rather than becoming a blanket allowance:
/// `indirect_target_summary` joins every function sharing the signature, and
/// two callees whose results differ collapse to `Unknown`, which is still
/// refused. This pins both directions.
#[test]
fn an_indirect_write_back_needs_every_candidate_callee_to_agree() {
    let agreeing = r#"
fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [3]
    values

fn swap(var values: frozen List(Int)) -> frozen List(Int):
    values = [4]
    values

fn main(console: Console):
    var values: frozen List(Int) = [1]
    let action = if values[0] == 1: replace else: swap
    let result = action(values)
    console.print("${values[0]}:${result[0]}")
"#;
    let checked = witchy::resolve_std_only_checked(agreeing)
        .expect("callees that write back identically admit");
    let artifact = witchy::codegen::compile_checked_module_artifact(&checked)
        .expect_lowered("indirect write-back lowers");
    let mut runtime = witchy::runtime::Runtime::batch().expect("runtime");
    let mut vm = runtime
        .spawn_admitted(
            &artifact,
            witchy::runtime::Capabilities { print: true, quiet: true, ..Default::default() },
            256,
        )
        .expect("spawn indirect write-back");
    vm.run().expect("run indirect write-back");
    // Independent expectation: `values[0] == 1` selects `replace`, which
    // writes `[3]` back through the `var` parameter and returns that storage.
    assert_eq!(vm.output(), ["3:3"]);

    let disagreeing = r#"
fn replace(var values: frozen List(Int)) -> frozen List(Int):
    values = [3]
    values

fn keep(var values: frozen List(Int)) -> frozen List(Int):
    values

fn main(console: Console):
    var values: frozen List(Int) = [1]
    let action = if values[0] == 1: replace else: keep
    let result = action(values)
    console.print("${values[0]}:${result[0]}")
"#;
    let checked = witchy::resolve_std_only_checked(disagreeing)
        .expect("the disagreement is a lowering refusal, not a type error");
    assert!(
        !matches!(
            witchy::codegen::compile_checked_module_artifact(&checked),
            witchy::codegen::LoweringOutcome::Lowered(_)
        ),
        "callees whose write-back results differ must not share one indirect call"
    );
}
