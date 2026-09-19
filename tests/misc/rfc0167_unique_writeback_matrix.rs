//! RFC-0167 row E: `var unique` write-back never duplicates its root owner.
//!
//! A `var unique` parameter may mutate and write back its original root while
//! returning a *different*, freshly established unique root.  Returning that
//! parameter itself would create two owning outputs for one root and must be
//! rejected before either interpreter or Wasm lowering.

fn check(source: &str) -> Result<witchy_types::pipeline::CheckedModule, witchy::ResolveStdError> {
    witchy::resolve_std_only_checked(source)
}

fn check_bundle(
    sources: Vec<(&str, &str)>,
) -> Result<witchy_types::pipeline::CheckedModule, witchy_types::pipeline::PipelineError> {
    witchy_types::pipeline::link_checked_from_source_bundle(
        sources
            .into_iter()
            .map(|(name, source)| (name.to_owned(), source.as_bytes().to_vec()))
            .collect(),
        "main",
        witchy_interp::comptime::expand_compile_time,
    )
}

fn assert_rejects_duplicate_unique_writeback(source: &str) {
    let error = match check(source) {
        Ok(_) => panic!("a unique write-back root cannot also be returned as a unique result"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("root-owner proof") || error.contains("unique") || error.contains("write-back"),
        "duplicate unique/write-back rejection must remain source-visible: {error}"
    );
}

#[test]
fn direct_unique_writeback_cannot_return_its_parameter_in_normal_or_opt_mode() {
    let body = r#"
fn duplicate(var values: unique List(Int)) -> unique List(Int):
    values
"#;
    for source in [body.to_owned(), format!("mode opt\n{body}")] {
        assert_rejects_duplicate_unique_writeback(&source);
    }
}

#[test]
fn early_return_unique_writeback_cannot_return_its_parameter_in_normal_or_opt_mode() {
    let body = r#"
fn duplicate(var values: unique List(Int), early: Bool) -> unique List(Int):
    if early:
        return values
    values
"#;
    for source in [body.to_owned(), format!("mode opt\n{body}")] {
        assert_rejects_duplicate_unique_writeback(&source);
    }
}

#[test]
fn error_value_return_cannot_hide_a_duplicate_unique_writeback_root() {
    let body = r#"
fn duplicate(var values: unique List(Int), succeed: Bool) -> Result(unique List(Int), String):
    if succeed:
        return Ok(values)
    Err("no result")
"#;
    for source in [body.to_owned(), format!("mode opt\n{body}")] {
        assert_rejects_duplicate_unique_writeback(&source);
    }
}

#[test]
fn result_unique_owner_accepts_own_ok_err_and_exact_direct_relay() {
    let source = r#"
fn wrap(own values: unique List(Int)) -> Result(unique List(Int), String):
    Ok(values)

fn relay(own values: unique List(Int)) -> Result(unique List(Int), String):
    wrap(values)

fn fail() -> Result(unique List(Int), String):
    Err("no result")
"#;
    check(source).expect("Result unique owners retain direct own, Err, and exact relay proofs");
}

#[test]
fn indirect_unique_writeback_cannot_return_its_parameter() {
    let source = r#"
fn duplicate(var values: unique List(Int)) -> unique List(Int):
    values

fn main():
    let operation = duplicate
    var values: unique List(Int) = [1]
    let _ = operation(values)
"#;
    assert_rejects_duplicate_unique_writeback(source);
}

const WORKER: &str = r#"mode opt

pub fn split(var values: unique List(Int)) -> unique List(Int):
    values[0] = 9
    [7]
"#;

const API: &str = r#"mode opt

import worker

pub fn direct_split(var values: unique List(Int)) -> unique List(Int):
    worker.split(values)
"#;

#[test]
fn fresh_unique_result_coexists_with_unique_writeback_across_normal_and_opt_boundaries() {
    let main = r#"import api

fn main(console: Console):
    var values: unique List(Int) = [1]
    let result = api.direct_split(values)
    console.print("${values[0]}:${result[0]}")
"#;
    let checked = check_bundle(vec![("worker", WORKER), ("api", API), ("main", main)])
        .expect("a fresh result has a distinct owner from the committed write-back root");
    witchy::enforce_performance_modes(checked.module(), "main")
        .expect("normal preparation and direct opt-to-opt transport retain both distinct facts");

}

#[test]
fn function_value_unique_result_route_fails_closed_without_a_root_owner_proof() {
    let api = r#"mode opt

import worker

pub fn indirect_split(var values: unique List(Int)) -> unique List(Int):
    let operation = worker.split
    operation(values)
"#;
    let main = r#"import api

fn main():
    var values: unique List(Int) = [1]
    let _ = api.indirect_split(values)
"#;
    let error = check_bundle(vec![("worker", WORKER), ("api", api), ("main", main)])
        .expect_err("an eta-expanded unique result cannot acquire a root-owner proof");
    assert!(
        error.to_string().contains("root-owner proof"),
        "function-value result must remain fail-closed: {error}"
    );
}
