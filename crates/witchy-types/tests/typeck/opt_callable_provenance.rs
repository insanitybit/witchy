//! RFC-0167 opt higher-order contracts through the production source linker.

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
    let error = super::source::checked_bundle(vec![("worker", WORKER), ("main", main)], "main")
        .expect_err("normal function cannot satisfy opt callback precondition");
    assert!(
        error.to_string().contains("requires an opt-compatible callable"),
        "{error}"
    );
}
