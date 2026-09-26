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

#[test]
fn imported_opt_body_keeps_call_scoped_copy_rule() {
    let worker = r#"mode opt

fn consume(value: String) -> Int:
    value.length()

pub fn inspect(let value: String) -> Int:
    consume(value)
"#;
    let main = "import worker\n\nfn main(console: Console):\n    let _ = worker.inspect(\"hello\")\n    console.print(\"done\")\n";
    let error = super::source::checked_bundle(vec![("worker", worker), ("main", main)], "main")
        .expect_err("an imported opt body cannot copy-repair call-scoped access");
    assert!(error.contains("call-scoped `let` parameter"), "{error}");

    let normal_worker = worker.replacen("mode opt\n\n", "", 1);
    super::source::checked_bundle(vec![("worker", &normal_worker), ("main", main)], "main")
        .expect("normal source may make the checked independent copy");
}
