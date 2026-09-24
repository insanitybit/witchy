//! RFC-0167 opt higher-order contracts through the production source linker.

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

const WORKER: &str = r#"mode opt

pub fn plus_one(value: Int) -> Int:
    value + 1

pub fn relay(callback: fn(Int) -> Int) -> fn(Int) -> Int:
    callback
"#;

#[test]
fn normal_callable_cannot_enter_opt_higher_order_parameter() {
    let main = r#"import worker

fn ordinary(value: Int) -> Int:
    value + 1

fn main(console: Console):
    console.print("${worker.relay(ordinary)(41)}")
"#;
    let error = check_bundle(vec![("worker", WORKER), ("main", main)])
        .expect_err("normal function cannot satisfy opt callback precondition");
    assert!(
        error.to_string().contains("requires an opt-compatible callable"),
        "{error}"
    );
}

